use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use anyhow::{Result, bail, ensure};
use serde::Serialize;

use crate::protocol::{Activity, ActivityState, Event, MAX_ACTIVITIES, MAX_SOURCES};

pub const SOURCE_TIMEOUT: Duration = Duration::from_secs(10);
pub const STATUS_PAGE_SIZE: usize = 4;
const CUE_DURATION: Duration = Duration::from_millis(1500);
pub(crate) type SourceId = (String, String);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FocusTarget {
    pub source: String,
    pub instance: String,
    pub activity: String,
}

impl From<&Selection> for FocusTarget {
    fn from(s: &Selection) -> Self {
        Self {
            source: s.source.clone(),
            instance: s.instance.clone(),
            activity: s.activity.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Status {
    mode: &'static str,
    focus: Option<FocusTarget>,
    focus_available: bool,
    activities: Vec<Selection>,
    offset: usize,
    total: usize,
    next_offset: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Selection {
    pub source: String,
    pub instance: String,
    pub activity: String,
    pub state: ActivityState,
    pub attention: bool,
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Following {
    pub source: String,
    pub instance: String,
    pub activity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Cue {
    pub id: u64,
    pub selection: Selection,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Presentation {
    pub base: Option<Selection>,
    pub cue: Option<Cue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub following: Option<Following>,
}

#[derive(Clone)]
struct Record {
    sequence: u64,
    value: Option<Activity>,
}

struct Source {
    seen: Instant,
    records: BTreeMap<String, Record>,
}

#[derive(Default)]
pub(crate) struct Store {
    sources: BTreeMap<SourceId, Source>,
    cue: Option<(Cue, Instant)>,
    next_cue: u64,
    focus: Option<FocusTarget>,
    focus_summary: Option<String>,
}

fn selection(id: &SourceId, a: &Activity) -> Selection {
    Selection {
        source: id.0.clone(),
        instance: id.1.clone(),
        activity: a.activity.clone(),
        state: a.state,
        attention: a.attention,
        reason: a.reason.clone(),
        summary: a.summary.clone(),
    }
}

impl Store {
    fn visible(&self) -> Vec<Selection> {
        self.sources
            .iter()
            .flat_map(|(id, source)| {
                source
                    .records
                    .values()
                    .filter_map(move |r| r.value.as_ref().map(|a| selection(id, a)))
            })
            .collect()
    }

    fn focused(&self) -> Option<Selection> {
        let f = self.focus.as_ref()?;
        let id = (f.source.clone(), f.instance.clone());
        self.sources
            .get(&id)?
            .records
            .get(&f.activity)?
            .value
            .as_ref()
            .map(|a| selection(&id, a))
    }

    fn choose_initial(&mut self) {
        if self.focus.is_none() {
            self.focus = self.visible().first().map(FocusTarget::from);
        }
    }

    pub fn focus(&mut self, source: &str, instance: Option<&str>, activity: &str) -> Result<()> {
        crate::protocol::validate_id(source)?;
        crate::protocol::validate_id(activity)?;
        if let Some(instance) = instance {
            crate::protocol::validate_id(instance)?;
        }
        let matches: Vec<_> = self
            .visible()
            .into_iter()
            .filter(|s| {
                s.source == source
                    && s.activity == activity
                    && instance.is_none_or(|i| s.instance == i)
            })
            .collect();
        ensure!(
            matches.len() == 1,
            "focus must match exactly one live activity (found {})",
            matches.len()
        );
        self.focus = Some(FocusTarget::from(&matches[0]));
        self.focus_summary = matches[0].summary.clone();
        self.cue = None;
        Ok(())
    }

    pub fn cycle(&mut self) -> Result<()> {
        let visible = self.visible();
        ensure!(!visible.is_empty(), "no visible activities to focus");
        let next = visible
            .iter()
            .position(|s| Some(FocusTarget::from(s)) == self.focus)
            .map_or(0, |i| (i + 1) % visible.len());
        self.focus = Some(FocusTarget::from(&visible[next]));
        self.focus_summary = visible[next].summary.clone();
        self.cue = None;
        Ok(())
    }

    pub fn status(&self, offset: usize) -> Status {
        let visible = self.visible();
        let total = visible.len();
        let activities: Vec<_> = visible
            .into_iter()
            .skip(offset)
            .take(STATUS_PAGE_SIZE)
            .collect();
        let end = offset.saturating_add(activities.len());
        Status {
            mode: "shared-focused",
            focus: self.focus.clone(),
            focus_available: self.focused().is_some(),
            activities,
            offset,
            total,
            next_offset: (end < total).then_some(end),
        }
    }

    pub fn register(
        &mut self,
        id: SourceId,
        activities: Vec<Activity>,
        now: Instant,
    ) -> Result<()> {
        ensure!(
            !self.sources.contains_key(&id),
            "source and instance already connected"
        );
        ensure!(self.sources.len() < MAX_SOURCES, "source limit reached");
        let records = Self::snapshot(&BTreeMap::new(), activities)?;
        self.sources.insert(id, Source { seen: now, records });
        self.choose_initial();
        self.presentation(now);
        Ok(())
    }

    fn snapshot(
        old: &BTreeMap<String, Record>,
        activities: Vec<Activity>,
    ) -> Result<BTreeMap<String, Record>> {
        let mut records = old.clone();
        for record in records.values_mut() {
            record.value = None;
        }
        let mut seen = std::collections::BTreeSet::new();
        for a in activities {
            a.validate()?;
            ensure!(
                seen.insert(a.activity.clone()),
                "duplicate activity in snapshot"
            );
            if let Some(previous) = old.get(&a.activity) {
                ensure!(
                    a.sequence > previous.sequence
                        || (a.sequence == previous.sequence && previous.value.as_ref() == Some(&a)),
                    "snapshot rolls back or changes an existing sequence"
                );
            }
            records.insert(
                a.activity.clone(),
                Record {
                    sequence: a.sequence,
                    value: Some(a),
                },
            );
        }
        ensure!(
            records.len() <= MAX_ACTIVITIES,
            "source lifetime activity limit is 32"
        );
        Ok(records)
    }

    pub fn apply(&mut self, id: &SourceId, event: Event, now: Instant) -> Result<()> {
        let source = self
            .sources
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("source expired; reconnect and register"))?;
        let mut completed = None;
        match event {
            Event::Register { .. } => bail!("source already registered"),
            Event::Status { .. } | Event::Focus { .. } | Event::Next => {
                bail!("control is not a producer event")
            }
            Event::Heartbeat => {}
            Event::Snapshot { activities } => {
                source.records = Self::snapshot(&source.records, activities)?;
            }
            Event::Remove { activity, sequence } => {
                ensure!(sequence > 0, "sequence must be positive");
                if let Some(old) = source.records.get(&activity) {
                    ensure!(sequence > old.sequence, "stale or duplicate sequence");
                } else {
                    ensure!(
                        source.records.len() < MAX_ACTIVITIES,
                        "source lifetime activity limit is 32"
                    );
                }
                source.records.insert(
                    activity,
                    Record {
                        sequence,
                        value: None,
                    },
                );
            }
            Event::State { activity: a } => {
                a.validate()?;
                let old = source.records.get(&a.activity);
                if let Some(old) = old {
                    ensure!(a.sequence > old.sequence, "stale or duplicate sequence");
                } else {
                    ensure!(
                        source.records.len() < MAX_ACTIVITIES,
                        "source lifetime activity limit is 32"
                    );
                }
                if matches!(a.state, ActivityState::Succeeded | ActivityState::Failed)
                    && old.and_then(|r| r.value.as_ref()).map(|a| a.state) != Some(a.state)
                {
                    completed = Some(selection(id, &a));
                }
                source.records.insert(
                    a.activity.clone(),
                    Record {
                        sequence: a.sequence,
                        value: Some(a),
                    },
                );
            }
        }
        source.seen = now;
        self.choose_initial();
        completed = completed.filter(|s| Some(FocusTarget::from(s)) == self.focus);
        // Expire and suppress old cues before considering a new transition.
        let view = self.presentation(now);
        let attention_pending = view.base.as_ref().is_some_and(|s| s.attention);
        let success_would_hide_failure = completed
            .as_ref()
            .is_some_and(|s| s.state == ActivityState::Succeeded)
            && self
                .cue
                .as_ref()
                .is_some_and(|(c, _)| c.selection.state == ActivityState::Failed);
        if let Some(selection) = completed
            && !(attention_pending || success_would_hide_failure)
        {
            self.next_cue = self.next_cue.saturating_add(1);
            self.cue = Some((
                Cue {
                    id: self.next_cue,
                    selection,
                },
                now + CUE_DURATION,
            ));
        }
        Ok(())
    }

    pub fn disconnect(&mut self, id: &SourceId) {
        self.sources.remove(id);
        if self
            .cue
            .as_ref()
            .is_some_and(|(c, _)| c.selection.source == id.0 && c.selection.instance == id.1)
        {
            self.cue = None;
        }
    }

    pub fn expire(&mut self, now: Instant) -> Vec<SourceId> {
        let expired: Vec<_> = self
            .sources
            .iter()
            .filter(|(_, s)| now.saturating_duration_since(s.seen) >= SOURCE_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            self.disconnect(id);
        }
        self.presentation(now);
        expired
    }

    pub fn presentation(&mut self, now: Instant) -> Presentation {
        let best = self.focused();
        if let Some(selected) = &best {
            self.focus_summary = selected.summary.clone();
        }
        if best.is_none()
            || best.as_ref().is_some_and(|s| s.attention)
            || self.cue.as_ref().is_some_and(|(_, until)| now >= *until)
        {
            self.cue = None;
        }
        Presentation {
            following: self.focus.as_ref().map(|f| Following {
                source: f.source.clone(),
                instance: f.instance.clone(),
                activity: f.activity.clone(),
                summary: self.focus_summary.clone(),
                available: best.is_some(),
            }),
            base: best,
            cue: self.cue.as_ref().map(|(c, _)| c.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn a(sequence: u64, state: ActivityState) -> Activity {
        Activity {
            activity: "task".into(),
            sequence,
            state,
            attention: false,
            reason: None,
            summary: None,
        }
    }
    fn id(s: &str) -> SourceId {
        (s.into(), "one".into())
    }
    fn update(s: &mut Store, who: &str, a: Activity, now: Instant) -> Result<()> {
        s.apply(&id(who), Event::State { activity: a }, now)
    }

    fn assert_unavailable(s: &mut Store, now: Instant) {
        let view = s.presentation(now);
        assert!(view.base.is_none());
        assert!(view.cue.is_none());
        let following = view.following.unwrap();
        let pin = s.focus.as_ref().unwrap();
        assert!(!following.available);
        assert_eq!(following.source, pin.source);
        assert_eq!(following.instance, pin.instance);
        assert_eq!(following.activity, pin.activity);
    }

    #[test]
    fn summaries_follow_only_the_pin_and_do_not_restart_cues() {
        let now = Instant::now();
        let mut s = Store::default();
        let mut selected = a(1, ActivityState::Working);
        selected.summary = Some("Selected conversation".into());
        s.register(id("a"), vec![selected.clone()], now).unwrap();
        s.register(id("b"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        let before = s.presentation(now);
        assert_eq!(before.following.as_ref().unwrap().summary, selected.summary);
        let mut background = a(2, ActivityState::Working);
        background.summary = Some("Background conversation".into());
        update(&mut s, "b", background, now).unwrap();
        assert_eq!(s.presentation(now), before);
        selected.sequence = 2;
        selected.summary = Some("Revised conversation".into());
        update(&mut s, "a", selected.clone(), now).unwrap();
        assert_ne!(s.presentation(now), before);
        assert!(s.presentation(now).cue.is_none());
        selected.sequence = 3;
        selected.state = ActivityState::Succeeded;
        update(&mut s, "a", selected.clone(), now).unwrap();
        let cue = s.presentation(now).cue.unwrap();
        selected.sequence = 4;
        selected.summary = Some("Finished conversation".into());
        update(&mut s, "a", selected, now + Duration::from_millis(500)).unwrap();
        assert_eq!(s.presentation(now).cue, Some(cue));
        assert!(s.presentation(now + CUE_DURATION).cue.is_none());
        s.focus("b", None, "task").unwrap();
        assert_eq!(
            s.presentation(now).following.unwrap().summary.as_deref(),
            Some("Background conversation")
        );
        s.focus("a", None, "task").unwrap();
        // Full replacement clears a previously supplied summary.
        update(&mut s, "a", a(5, ActivityState::Succeeded), now).unwrap();
        assert!(s.presentation(now).following.unwrap().summary.is_none());
        let mut snapshot = a(6, ActivityState::Succeeded);
        snapshot.summary = Some("Snapshot summary".into());
        s.apply(
            &id("a"),
            Event::Snapshot {
                activities: vec![snapshot],
            },
            now,
        )
        .unwrap();
        assert_eq!(
            s.presentation(now).following.unwrap().summary.as_deref(),
            Some("Snapshot summary")
        );
        s.apply(
            &id("a"),
            Event::Snapshot {
                activities: vec![a(7, ActivityState::Succeeded)],
            },
            now,
        )
        .unwrap();
        assert!(s.presentation(now).following.unwrap().summary.is_none());
    }

    #[test]
    fn summary_cache_survives_unavailability_but_never_crosses_focus() {
        let now = Instant::now();
        let mut s = Store::default();
        let mut selected = a(1, ActivityState::Working);
        selected.summary = Some("Remember this conversation".into());
        s.register(id("a"), vec![selected.clone()], now).unwrap();
        s.apply(&id("a"), Event::Snapshot { activities: vec![] }, now)
            .unwrap();
        assert_unavailable(&mut s, now);
        assert_eq!(
            s.presentation(now).following.unwrap().summary,
            selected.summary
        );
        selected.sequence = 2;
        s.apply(
            &id("a"),
            Event::Snapshot {
                activities: vec![selected.clone()],
            },
            now,
        )
        .unwrap();
        s.apply(
            &id("a"),
            Event::Remove {
                activity: "task".into(),
                sequence: 3,
            },
            now,
        )
        .unwrap();
        assert_unavailable(&mut s, now);
        assert_eq!(
            s.presentation(now).following.unwrap().summary,
            selected.summary
        );
        s.disconnect(&id("a"));
        assert_eq!(
            s.presentation(now).following.unwrap().summary,
            selected.summary
        );
        s.register(
            ("a".into(), "two".into()),
            vec![a(1, ActivityState::Working)],
            now,
        )
        .unwrap();
        assert_unavailable(&mut s, now);
        selected.sequence = 1;
        selected.summary = Some("Reconnected conversation".into());
        s.register(id("a"), vec![selected.clone()], now).unwrap();
        assert!(s.presentation(now).following.unwrap().available);
        assert_eq!(
            s.presentation(now).following.unwrap().summary,
            selected.summary
        );
        s.expire(now + SOURCE_TIMEOUT);
        assert_unavailable(&mut s, now);
        assert_eq!(
            s.presentation(now).following.unwrap().summary,
            selected.summary
        );
        s.register(id("b"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        s.cycle().unwrap();
        assert!(s.presentation(now).following.unwrap().summary.is_none());
        s.disconnect(&id("b"));
        assert!(s.presentation(now).following.unwrap().summary.is_none());
    }

    #[test]
    fn selected_source_ignores_background_attention_and_outcomes() {
        let now = Instant::now();
        let mut s = Store::default();
        s.register(id("z"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        s.register(id("a"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        assert_eq!(s.presentation(now).base.unwrap().source, "z");
        update(&mut s, "z", a(2, ActivityState::Succeeded), now).unwrap();
        assert_eq!(
            s.presentation(now).cue.unwrap().selection.state,
            ActivityState::Succeeded
        );
        let mut waiting = a(2, ActivityState::Waiting);
        waiting.attention = true;
        let before = s.presentation(now);
        update(&mut s, "a", waiting, now).unwrap();
        assert_eq!(s.presentation(now), before);
        update(&mut s, "a", a(3, ActivityState::Failed), now).unwrap();
        assert_eq!(s.presentation(now), before);
        update(&mut s, "z", a(3, ActivityState::Working), now).unwrap();
        assert!(s.presentation(now).cue.is_some());
        assert_eq!(
            s.presentation(now).base.unwrap().state,
            ActivityState::Working
        );
        let mut waiting = a(4, ActivityState::Waiting);
        waiting.attention = true;
        update(&mut s, "z", waiting, now).unwrap();
        assert!(s.presentation(now).cue.is_none());
    }

    #[test]
    fn failure_priority_and_exact_cue_expiry() {
        let now = Instant::now();
        let mut s = Store::default();
        for who in ["a", "b"] {
            s.register(id(who), vec![], now).unwrap();
        }
        update(&mut s, "a", a(1, ActivityState::Failed), now).unwrap();
        let cue = s.presentation(now).cue.unwrap();
        update(&mut s, "b", a(1, ActivityState::Succeeded), now).unwrap();
        assert_eq!(s.presentation(now).cue, Some(cue.clone()));
        update(&mut s, "a", a(2, ActivityState::Failed), now).unwrap();
        assert_eq!(s.presentation(now).cue, Some(cue));
        assert!(s.presentation(now + CUE_DURATION).cue.is_none());
    }

    #[test]
    fn terminal_base_and_disconnected_focus_keep_identity() {
        let now = Instant::now();
        let mut s = Store::default();
        s.register(id("idle"), vec![a(1, ActivityState::Idle)], now)
            .unwrap();
        s.register(id("wait"), vec![a(1, ActivityState::Waiting)], now)
            .unwrap();
        s.register(id("work"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        s.focus("work", None, "task").unwrap();
        update(&mut s, "work", a(2, ActivityState::Cancelled), now).unwrap();
        assert_eq!(
            s.presentation(now).base.unwrap().state,
            ActivityState::Cancelled
        );
        assert!(s.presentation(now).cue.is_none());
        let mut attention = a(2, ActivityState::Waiting);
        attention.attention = true;
        update(&mut s, "wait", attention, now).unwrap();
        update(&mut s, "work", a(3, ActivityState::Failed), now).unwrap();
        assert!(s.presentation(now).cue.is_some());
        let pin = s.focus.clone();
        s.disconnect(&id("work"));
        assert_eq!(s.presentation(now).base, None);
        assert_eq!(s.focus, pin);
        assert!(s.presentation(now).cue.is_none());
    }

    #[test]
    fn exact_conversation_focus_cycle_and_historical_states() {
        let now = Instant::now();
        let mut s = Store::default();
        assert!(s.focus.is_none());
        assert!(s.cycle().is_err());
        s.register(id("a"), vec![], now).unwrap();
        let mut other = a(1, ActivityState::Succeeded);
        other.activity = "aaa".into();
        s.apply(
            &id("a"),
            Event::Snapshot {
                activities: vec![a(1, ActivityState::Working), other.clone()],
            },
            now,
        )
        .unwrap();
        assert_eq!(s.focus.as_ref().unwrap().activity, "aaa");
        assert!(s.presentation(now).cue.is_none());
        s.focus("a", None, "task").unwrap();
        other.sequence = 2;
        other.state = ActivityState::Failed;
        let before = s.presentation(now);
        update(&mut s, "a", other.clone(), now).unwrap();
        assert_eq!(s.presentation(now), before);
        update(&mut s, "a", a(2, ActivityState::Succeeded), now).unwrap();
        assert!(s.presentation(now).cue.is_some());
        s.cycle().unwrap();
        assert_eq!(s.presentation(now).base.unwrap().activity, "aaa");
        assert!(s.presentation(now).cue.is_none());
        s.cycle().unwrap();
        assert_eq!(
            s.presentation(now).base.unwrap().state,
            ActivityState::Succeeded
        );
        assert!(s.presentation(now).cue.is_none());
        let pin = s.focus.clone();
        s.apply(
            &id("a"),
            Event::Snapshot {
                activities: vec![other],
            },
            now,
        )
        .unwrap();
        assert_eq!(s.focus, pin);
        assert_unavailable(&mut s, now);
        s.cycle().unwrap();
        assert_eq!(s.focus.as_ref().unwrap().activity, "aaa");
        s.register(
            ("a".into(), "two".into()),
            vec![{
                let mut a = a(1, ActivityState::Working);
                a.activity = "aaa".into();
                a
            }],
            now,
        )
        .unwrap();
        assert!(s.focus("a", None, "aaa").is_err());
        assert!(s.focus("a", Some("missing"), "aaa").is_err());
        assert!(s.focus("missing", None, "aaa").is_err());
        s.focus("a", Some("two"), "aaa").unwrap();
        let pin = s.focus.clone();
        s.expire(now + SOURCE_TIMEOUT);
        assert_eq!(s.focus, pin);
        s.register(id("background"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        assert_unavailable(&mut s, now);
        s.cycle().unwrap();
        assert_eq!(s.focus.as_ref().unwrap().source, "background");
    }

    #[test]
    fn snapshots_atomic_tombstones_and_no_celebration() {
        let now = Instant::now();
        let mut s = Store::default();
        s.register(id("a"), vec![a(2, ActivityState::Succeeded)], now)
            .unwrap();
        assert!(s.presentation(now).cue.is_none());
        assert!(update(&mut s, "a", a(2, ActivityState::Failed), now).is_err());
        let before = s.presentation(now);
        assert!(
            s.apply(
                &id("a"),
                Event::Snapshot {
                    activities: vec![a(1, ActivityState::Working)]
                },
                now
            )
            .is_err()
        );
        assert_eq!(before, s.presentation(now));
        s.apply(
            &id("a"),
            Event::Snapshot {
                activities: vec![a(2, ActivityState::Succeeded)],
            },
            now,
        )
        .unwrap();
        s.apply(&id("a"), Event::Snapshot { activities: vec![] }, now)
            .unwrap();
        assert!(update(&mut s, "a", a(2, ActivityState::Working), now).is_err());
        update(&mut s, "a", a(3, ActivityState::Working), now).unwrap();
        s.apply(
            &id("a"),
            Event::Remove {
                activity: "task".into(),
                sequence: 4,
            },
            now,
        )
        .unwrap();
        assert!(update(&mut s, "a", a(4, ActivityState::Working), now).is_err());
        update(&mut s, "a", a(5, ActivityState::Working), now).unwrap();
    }

    #[test]
    fn liveness_expiry_and_disconnect_are_not_completion() {
        let now = Instant::now();
        let mut s = Store::default();
        s.register(id("a"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        assert!(
            s.expire(now + SOURCE_TIMEOUT - Duration::from_nanos(1))
                .is_empty()
        );
        s.apply(&id("a"), Event::Heartbeat, now + Duration::from_secs(1))
            .unwrap();
        assert!(s.expire(now + SOURCE_TIMEOUT).is_empty());
        assert_eq!(
            s.expire(now + SOURCE_TIMEOUT + Duration::from_secs(1)),
            vec![id("a")]
        );
        assert_unavailable(&mut s, now);
        s.register(id("a"), vec![], now).unwrap();
        update(&mut s, "a", a(1, ActivityState::Failed), now).unwrap();
        s.disconnect(&id("a"));
        assert_unavailable(&mut s, now);
    }

    #[test]
    fn lifetime_limits_and_atomic_multi_activity_rollback() {
        let now = Instant::now();
        let mut s = Store::default();
        s.register(id("a"), vec![a(2, ActivityState::Working)], now)
            .unwrap();
        let mut fresh = a(1, ActivityState::Waiting);
        fresh.activity = "fresh".into();
        assert!(
            s.apply(
                &id("a"),
                Event::Snapshot {
                    activities: vec![fresh, a(1, ActivityState::Idle)]
                },
                now
            )
            .is_err()
        );
        assert_eq!(s.sources[&id("a")].records.len(), 1);
        assert_eq!(
            s.presentation(now).base.unwrap().state,
            ActivityState::Working
        );
        for i in 1..MAX_ACTIVITIES {
            s.apply(
                &id("a"),
                Event::Remove {
                    activity: format!("removed-{i}"),
                    sequence: 1,
                },
                now,
            )
            .unwrap();
        }
        let mut extra = a(1, ActivityState::Idle);
        extra.activity = "extra".into();
        assert!(update(&mut s, "a", extra, now).is_err());
        for i in 1..MAX_SOURCES {
            s.register(id(&format!("source-{i}")), vec![], now).unwrap();
        }
        assert!(s.register(id("overflow"), vec![], now).is_err());
        s.disconnect(&id("a"));
        s.register(id("a"), vec![a(1, ActivityState::Idle)], now)
            .unwrap();
    }
}
