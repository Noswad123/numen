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
const MAX_EXPLICIT_FAMILIAR_IDS: usize = 256;
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
    #[serde(skip_serializing_if = "Option::is_none")]
    familiar_id: Option<String>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FamiliarPresentation {
    pub id: String,
    pub preset: String,
    pub presentation: Presentation,
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

pub(crate) struct Store {
    sources: BTreeMap<SourceId, Source>,
    familiars: BTreeMap<String, Familiar>,
    // Only explicit IDs and main need history; generated IDs use the counter.
    used_ids: std::collections::BTreeSet<String>,
    next_familiar: u64,
}

#[derive(Default)]
struct Familiar {
    cue: Option<(Cue, Instant)>,
    next_cue: u64,
    focus: Option<FocusTarget>,
    focus_summary: Option<String>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            sources: BTreeMap::new(),
            familiars: BTreeMap::from([("main".into(), Familiar::default())]),
            used_ids: std::collections::BTreeSet::from(["main".into()]),
            next_familiar: 1,
        }
    }
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
    pub(crate) fn empty() -> Self {
        Self {
            familiars: BTreeMap::new(),
            ..Self::default()
        }
    }

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

    fn focused(&self, familiar: &str) -> Option<Selection> {
        let f = self.familiars.get(familiar)?.focus.as_ref()?;
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
        if self
            .familiars
            .get("main")
            .is_some_and(|f| f.focus.is_none())
        {
            let first = self
                .visible()
                .into_iter()
                .find(|s| self.owner(&FocusTarget::from(s)).is_none());
            if let Some(first) = first {
                let main = self.familiars.get_mut("main").unwrap();
                main.focus = Some(FocusTarget::from(&first));
                main.focus_summary = first.summary;
            }
        }
    }

    fn owner(&self, target: &FocusTarget) -> Option<&str> {
        self.familiars
            .iter()
            .find(|(_, f)| f.focus.as_ref() == Some(target))
            .map(|(id, _)| id.as_str())
    }

    pub fn require_familiar(&self, id: &str) -> Result<()> {
        crate::protocol::validate_id(id)?;
        ensure!(self.familiars.contains_key(id), "unknown familiar: {id}");
        Ok(())
    }

    pub fn focus(&mut self, source: &str, instance: Option<&str>, activity: &str) -> Result<()> {
        self.focus_for("main", source, instance, activity)
    }

    pub fn focus_for(
        &mut self,
        familiar: &str,
        source: &str,
        instance: Option<&str>,
        activity: &str,
    ) -> Result<()> {
        self.require_familiar(familiar)?;
        let selected = self.resolve(source, instance, activity)?;
        let target = FocusTarget::from(&selected);
        ensure!(
            self.owner(&target).is_none_or(|owner| owner == familiar),
            "target already claimed"
        );
        let f = self.familiars.get_mut(familiar).unwrap();
        f.focus = Some(target);
        f.focus_summary = selected.summary;
        f.cue = None;
        Ok(())
    }

    fn resolve(&self, source: &str, instance: Option<&str>, activity: &str) -> Result<Selection> {
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
        Ok(matches[0].clone())
    }

    pub fn cycle(&mut self) -> Result<()> {
        self.cycle_for("main")
    }

    pub fn cycle_for(&mut self, familiar: &str) -> Result<()> {
        self.require_familiar(familiar)?;
        let visible: Vec<_> = self
            .visible()
            .into_iter()
            .filter(|s| {
                self.owner(&FocusTarget::from(s))
                    .is_none_or(|id| id == familiar)
            })
            .collect();
        ensure!(!visible.is_empty(), "no visible activities to focus");
        let next = visible
            .iter()
            .position(|s| Some(FocusTarget::from(s)) == self.familiars[familiar].focus)
            .map_or(0, |i| (i + 1) % visible.len());
        let f = self.familiars.get_mut(familiar).unwrap();
        f.focus = Some(FocusTarget::from(&visible[next]));
        f.focus_summary = visible[next].summary.clone();
        f.cue = None;
        Ok(())
    }

    pub fn status(&self, offset: usize) -> Status {
        self.status_inner(offset, None)
    }

    pub fn status_for(&self, offset: usize, familiar: Option<&str>) -> Result<Status> {
        if let Some(id) = familiar {
            self.require_familiar(id)?;
        }
        Ok(self.status_inner(offset, familiar))
    }

    fn status_inner(&self, offset: usize, familiar: Option<&str>) -> Status {
        let id = familiar.unwrap_or("main");
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
            focus: self.familiars.get(id).and_then(|f| f.focus.clone()),
            focus_available: self.focused(id).is_some(),
            activities,
            offset,
            total,
            next_offset: (end < total).then_some(end),
            familiar_id: familiar.map(str::to_owned),
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
        self.presentations(now);
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
            Event::Quit
            | Event::Service
            | Event::Status { .. }
            | Event::Focus { .. }
            | Event::Next { .. }
            | Event::Reveal { .. }
            | Event::FamiliarList { .. }
            | Event::Targets { .. }
            | Event::Open { .. }
            | Event::Attach { .. }
            | Event::GoToMouse { .. }
            | Event::Close { .. } => {
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
        let owner = completed
            .as_ref()
            .and_then(|s| self.owner(&FocusTarget::from(s)))
            .map(str::to_owned);
        // Expire and suppress old cues before considering a new transition.
        self.presentations(now);
        let Some(owner) = owner else {
            return Ok(());
        };
        let view = self.presentation_for(&owner, now);
        let f = self.familiars.get_mut(&owner).unwrap();
        let attention_pending = view.base.as_ref().is_some_and(|s| s.attention);
        let success_would_hide_failure = completed
            .as_ref()
            .is_some_and(|s| s.state == ActivityState::Succeeded)
            && f.cue
                .as_ref()
                .is_some_and(|(c, _)| c.selection.state == ActivityState::Failed);
        if let Some(selection) = completed
            && !(attention_pending || success_would_hide_failure)
        {
            f.next_cue = f.next_cue.saturating_add(1);
            f.cue = Some((
                Cue {
                    id: f.next_cue,
                    selection,
                },
                now + CUE_DURATION,
            ));
        }
        Ok(())
    }

    pub fn disconnect(&mut self, id: &SourceId) {
        self.sources.remove(id);
        for f in self.familiars.values_mut() {
            if f.cue
                .as_ref()
                .is_some_and(|(c, _)| c.selection.source == id.0 && c.selection.instance == id.1)
            {
                f.cue = None;
            }
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
        self.presentations(now);
        expired
    }

    pub fn presentation(&mut self, now: Instant) -> Presentation {
        self.presentation_for("main", now)
    }

    pub fn presentations(&mut self, now: Instant) -> Vec<FamiliarPresentation> {
        let ids: Vec<_> = self.familiars.keys().cloned().collect();
        ids.into_iter()
            .map(|id| FamiliarPresentation {
                presentation: self.presentation_for(&id, now),
                id,
                preset: "djinn".into(),
            })
            .collect()
    }

    fn presentation_for(&mut self, id: &str, now: Instant) -> Presentation {
        let best = self.focused(id);
        let Some(f) = self.familiars.get_mut(id) else {
            return Presentation::default();
        };
        if let Some(selected) = &best {
            f.focus_summary = selected.summary.clone();
        }
        if best.is_none()
            || best.as_ref().is_some_and(|s| s.attention)
            || f.cue.as_ref().is_some_and(|(_, until)| now >= *until)
        {
            f.cue = None;
        }
        Presentation {
            following: f.focus.as_ref().map(|target| Following {
                source: target.source.clone(),
                instance: target.instance.clone(),
                activity: target.activity.clone(),
                summary: f.focus_summary.clone(),
                available: best.is_some(),
            }),
            base: best,
            cue: f.cue.as_ref().map(|(c, _)| c.clone()),
        }
    }

    pub fn open(&mut self, items: &[crate::protocol::OpenRequest]) -> Result<Vec<String>> {
        use crate::protocol::{MAX_FAMILIARS, validate_id};
        ensure!(
            (1..=MAX_FAMILIARS).contains(&items.len()),
            "expected 1..16 items"
        );
        ensure!(
            self.familiars.len() + items.len() <= MAX_FAMILIARS,
            "familiar limit reached"
        );
        let mut next = self.next_familiar;
        let mut pending = BTreeMap::new();
        let mut ids = Vec::new();
        // Reserve explicit IDs before generating any IDs, independent of batch order.
        let mut reserved = std::collections::BTreeSet::new();
        for item in items {
            ensure!(item.preset == "djinn", "unknown preset");
            if let Some(id) = &item.id {
                validate_id(id)?;
                let historical_generated = id
                    .strip_prefix("familiar-")
                    .and_then(|suffix| suffix.parse::<u64>().ok())
                    .filter(|n| id == &format!("familiar-{n}"))
                    .is_some_and(|n| n < self.next_familiar);
                ensure!(
                    !historical_generated
                        && !self.used_ids.contains(id)
                        && reserved.insert(id.clone()),
                    "familiar ID already used: {id}"
                );
            }
        }
        ensure!(
            self.used_ids.len() - 1 + reserved.len() <= MAX_EXPLICIT_FAMILIAR_IDS,
            "explicit familiar ID lifetime limit is 256; omit id to generate an ID"
        );
        let mut targets = Vec::new();
        for item in items {
            let selected = self.resolve(
                &item.target.source,
                item.target.instance.as_deref(),
                &item.target.activity,
            )?;
            let target = FocusTarget::from(&selected);
            ensure!(
                self.owner(&target).is_none() && !targets.contains(&target),
                "target already claimed"
            );
            targets.push(target.clone());
            let id = if let Some(id) = &item.id {
                id.clone()
            } else {
                loop {
                    let id = format!("familiar-{next}");
                    next = next
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("familiar ID space exhausted"))?;
                    if !self.used_ids.contains(&id) && reserved.insert(id.clone()) {
                        break id;
                    }
                }
            };
            pending.insert(
                id.clone(),
                Familiar {
                    focus: Some(target),
                    focus_summary: selected.summary,
                    ..Familiar::default()
                },
            );
            ids.push(id);
        }
        self.next_familiar = next;
        self.used_ids
            .extend(items.iter().filter_map(|item| item.id.clone()));
        self.familiars.extend(pending);
        Ok(ids)
    }

    pub fn attach(&mut self, items: &[crate::protocol::AttachRequest]) -> Result<Vec<String>> {
        let ids: Vec<_> = items.iter().map(|i| i.id.clone()).collect();
        crate::protocol::validate_ids(&ids)?;
        let mut pending = Vec::new();
        let mut targets = Vec::new();
        for item in items {
            self.require_familiar(&item.id)?;
            let selected = self.resolve(
                &item.target.source,
                item.target.instance.as_deref(),
                &item.target.activity,
            )?;
            let target = FocusTarget::from(&selected);
            ensure!(
                self.owner(&target).is_none_or(|owner| owner == item.id)
                    && !targets.contains(&target),
                "target already claimed"
            );
            targets.push(target.clone());
            pending.push((item.id.clone(), target, selected.summary));
        }
        for (id, target, summary) in pending {
            let f = self.familiars.get_mut(&id).unwrap();
            f.focus = Some(target);
            f.focus_summary = summary;
            f.cue = None;
        }
        Ok(ids)
    }

    pub fn close(&mut self, ids: &[String]) -> Result<Vec<String>> {
        crate::protocol::validate_ids(ids)?;
        for id in ids {
            self.require_familiar(id)?;
        }
        for id in ids {
            self.familiars.remove(id);
        }
        Ok(ids.to_vec())
    }

    pub fn list(&self, offset: usize) -> serde_json::Value {
        let rows: Vec<_> = self.familiars.iter().skip(offset).take(STATUS_PAGE_SIZE).map(|(id, f)| {
            let live = self.focused(id);
            let mut row = serde_json::json!({"id":id,"preset":"djinn","attachment":f.focus,"available":live.is_some(),"capabilities":["info","attach","close"]});
            if let Some(summary) = live.as_ref().map(|s| &s.summary).unwrap_or(&f.focus_summary) { row["summary"] = summary.clone().into(); }
            row
        }).collect();
        Self::page("familiars", rows, offset, self.familiars.len())
    }

    pub fn targets(&self, offset: usize, familiar: Option<&str>) -> Result<serde_json::Value> {
        if let Some(id) = familiar {
            self.require_familiar(id)?;
        }
        let visible = self.visible();
        let rows = visible
            .iter()
            .skip(offset)
            .take(STATUS_PAGE_SIZE)
            .map(|s| {
                let owner = self.owner(&FocusTarget::from(s));
                let mut row = serde_json::to_value(s).expect("selection is serializable");
                row["claimed_by"] = serde_json::json!(owner);
                row["eligible"] = serde_json::json!(owner.is_none() || owner == familiar);
                row
            })
            .collect();
        Ok(Self::page("targets", rows, offset, visible.len()))
    }

    fn page(
        key: &str,
        rows: Vec<serde_json::Value>,
        offset: usize,
        total: usize,
    ) -> serde_json::Value {
        let end = offset.saturating_add(rows.len());
        let mut page = serde_json::json!({"offset":offset,"total":total,"next_offset":(end < total).then_some(end)});
        page[key] = rows.into();
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AttachRequest, OpenRequest, TargetRequest};

    fn target(source: &str) -> TargetRequest {
        TargetRequest {
            source: source.into(),
            instance: None,
            activity: "task".into(),
        }
    }

    #[test]
    fn all_controls_are_rejected_by_producer_apply_without_side_effects() {
        let now = Instant::now();
        let mut s = Store::default();
        s.register(id("a"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        let mut completed = a(2, ActivityState::Failed);
        completed.summary = Some("Keep this summary".into());
        update(&mut s, "a", completed.clone(), now).unwrap();
        let before = s.presentations(now);
        let cue_deadline = s.familiars["main"].cue.as_ref().unwrap().1;
        let next_cue = s.familiars["main"].next_cue;
        let status = serde_json::to_value(s.status(0)).unwrap();
        let list = s.list(0);
        let used_ids = s.used_ids.clone();
        let next_familiar = s.next_familiar;
        for kind in [
            serde_json::json!({"type":"companion.quit"}),
            serde_json::json!({"type":"companion.service"}),
            serde_json::json!({"type":"companion.status"}),
            serde_json::json!({"type":"companion.focus","source":"a","activity":"task"}),
            serde_json::json!({"type":"companion.next"}),
            serde_json::json!({"type":"companion.reveal"}),
            serde_json::json!({"type":"familiar.list"}),
            serde_json::json!({"type":"companion.targets"}),
            serde_json::json!({"type":"familiar.open","items":[{"source":"a","activity":"task"}]}),
            serde_json::json!({"type":"familiar.attach","items":[{"id":"main","source":"a","activity":"task"}]}),
            serde_json::json!({"type":"familiar.close","ids":["main"]}),
            serde_json::json!({"type":"familiar.go-to-mouse","ids":["main"]}),
        ] {
            let mut packet = kind;
            packet["version"] = 1.into();
            let event = crate::protocol::Request::parse(&serde_json::to_vec(&packet).unwrap())
                .unwrap()
                .event;
            assert!(
                s.apply(&id("a"), event, now + Duration::from_secs(9))
                    .unwrap_err()
                    .to_string()
                    .contains("control is not a producer event")
            );
            assert_eq!(s.sources[&id("a")].seen, now);
            assert_eq!(s.familiars["main"].cue.as_ref().unwrap().1, cue_deadline);
            assert_eq!(s.familiars["main"].next_cue, next_cue);
            assert_eq!(
                s.sources[&id("a")].records["task"].value,
                Some(completed.clone())
            );
            assert_eq!(s.sources[&id("a")].records["task"].sequence, 2);
            assert_eq!(serde_json::to_value(s.status(0)).unwrap(), status);
            assert_eq!(s.list(0), list);
            assert_eq!(s.used_ids, used_ids);
            assert_eq!(s.next_familiar, next_familiar);
            assert_eq!(s.presentations(now), before);
        }
    }

    fn open_request(source: &str) -> OpenRequest {
        OpenRequest {
            id: None,
            preset: "djinn".into(),
            target: target(source),
        }
    }

    #[test]
    fn empty_store_never_bootstraps_main_and_preserves_id_history() {
        let now = Instant::now();
        let mut s = Store::empty();
        assert!(s.presentations(now).is_empty());
        assert!(s.status(0).focus.is_none());
        s.register(id("a"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        update(&mut s, "a", a(2, ActivityState::Succeeded), now).unwrap();
        assert!(s.presentations(now).is_empty());
        assert_eq!(
            s.targets(0, None).unwrap()["targets"][0]["claimed_by"],
            serde_json::Value::Null
        );
        let mut main = open_request("a");
        main.id = Some("main".into());
        assert!(s.open(&[main]).is_err());
        for n in 1..=1000 {
            let ids = s.open(&[open_request("a")]).unwrap();
            assert_eq!(ids, [format!("familiar-{n}")]);
            s.close(&ids).unwrap();
            assert_eq!(
                s.used_ids,
                std::collections::BTreeSet::from(["main".into()])
            );
        }
        assert_eq!(s.next_familiar, 1001);
        assert!(s.presentations(now).is_empty());
    }

    #[test]
    fn independent_views_keep_cues_summaries_and_unavailable_bindings() {
        let now = Instant::now();
        let mut s = Store::default();
        for who in ["a", "b", "c"] {
            let mut activity = a(1, ActivityState::Working);
            activity.summary = Some(format!("Summary {who}"));
            s.register(id(who), vec![activity], now).unwrap();
        }
        let second = s.open(&[open_request("b")]).unwrap().remove(0);
        let initial = s.presentations(now);
        assert_eq!(initial.len(), 2);
        assert!(initial.iter().all(|v| v.presentation.cue.is_none()));
        update(&mut s, "b", a(2, ActivityState::Failed), now).unwrap();
        assert_eq!(
            s.presentation(now),
            initial
                .iter()
                .find(|v| v.id == "main")
                .unwrap()
                .presentation
        );
        assert!(s.presentation_for(&second, now).cue.is_some());
        let mut completed = a(3, ActivityState::Failed);
        completed.summary = Some("Finished B".into());
        update(&mut s, "b", completed, now).unwrap();
        s.disconnect(&id("b"));
        let disconnected = s.presentation_for(&second, now);
        assert!(disconnected.base.is_none() && disconnected.cue.is_none());
        let following = disconnected.following.unwrap();
        assert!(!following.available);
        assert_eq!(following.summary.as_deref(), Some("Finished B"));
        assert!(s.focus("b", None, "task").is_err());
        s.register(id("b"), vec![a(1, ActivityState::Succeeded)], now)
            .unwrap();
        assert!(s.open(&[open_request("b")]).is_err());
        assert!(s.focus("b", None, "task").is_err());
        s.cycle().unwrap();
        assert_eq!(s.presentation(now).base.unwrap().source, "c");
        s.attach(&[AttachRequest {
            id: second.clone(),
            target: target("a"),
        }])
        .unwrap();
        assert!(s.presentation_for(&second, now).cue.is_none());
        assert_eq!(
            s.presentation_for(&second, now)
                .following
                .unwrap()
                .summary
                .as_deref(),
            Some("Summary a")
        );
        s.close(&[second]).unwrap();
        assert_eq!(s.sources.len(), 3);
        assert_eq!(s.presentation(now).base.unwrap().source, "c");
    }

    #[test]
    fn batches_are_atomic_and_ids_are_never_reused() {
        let now = Instant::now();
        let mut s = Store::default();
        for who in ["a", "b", "c"] {
            s.register(id(who), vec![a(1, ActivityState::Working)], now)
                .unwrap();
        }
        let before = s.presentations(now);
        assert!(s.open(&[open_request("b"), open_request("a")]).is_err());
        assert!(s.open(&[open_request("b"), open_request("b")]).is_err());
        assert_eq!(s.presentations(now), before);
        assert_eq!(s.next_familiar, 1);
        let mut explicit = open_request("c");
        explicit.id = Some("familiar-1".into());
        let ids = s.open(&[open_request("b"), explicit]).unwrap();
        assert_eq!(ids, ["familiar-2", "familiar-1"]);
        let before = s.presentations(now);
        assert!(
            s.attach(&[
                AttachRequest {
                    id: ids[0].clone(),
                    target: target("b")
                },
                AttachRequest {
                    id: ids[1].clone(),
                    target: target("a")
                },
            ])
            .is_err()
        );
        assert_eq!(s.presentations(now), before);
        assert!(s.close(&[ids[0].clone(), "missing".into()]).is_err());
        assert_eq!(s.presentations(now), before);
        s.close(&ids).unwrap();
        let mut reused = open_request("b");
        reused.id = Some(ids[0].clone());
        assert!(s.open(&[reused]).is_err());
        assert_eq!(s.sources[&id("a")].seen, now);
        assert_eq!(s.sources[&id("b")].seen, now);
    }

    #[test]
    fn generated_id_history_stays_bounded_across_one_thousand_open_close_cycles() {
        let now = Instant::now();
        let mut s = Store::default();
        for who in ["a", "b"] {
            s.register(id(who), vec![a(1, ActivityState::Working)], now)
                .unwrap();
        }
        for n in 1..=1000 {
            let ids = s.open(&[open_request("b")]).unwrap();
            assert_eq!(ids, [format!("familiar-{n}")]);
            s.close(&ids).unwrap();
            assert_eq!(s.used_ids.len(), 1);
        }
        assert_eq!(s.next_familiar, 1001);
        assert!(s.used_ids.contains("main"));
        assert_eq!(s.familiars.len(), 1);
    }

    #[test]
    fn explicit_id_history_limit_is_atomic_and_does_not_limit_generated_ids() {
        let now = Instant::now();
        let mut s = Store::default();
        for who in ["a", "b", "c", "d"] {
            s.register(id(who), vec![a(1, ActivityState::Working)], now)
                .unwrap();
        }
        for n in 0..MAX_EXPLICIT_FAMILIAR_IDS - 1 {
            let mut item = open_request("b");
            item.id = Some(format!("{n:096}"));
            let ids = s.open(&[item]).unwrap();
            s.close(&ids).unwrap();
        }
        let before = s.presentations(now);
        let history = s.used_ids.clone();
        let counter = s.next_familiar;
        let mut last = open_request("c");
        last.id = Some("last-explicit".into());
        let mut excess = open_request("d");
        excess.id = Some("excess-explicit".into());
        let error = s
            .open(&[open_request("b"), last.clone(), excess.clone()])
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("explicit familiar ID lifetime limit is 256")
        );
        assert_eq!(s.presentations(now), before);
        assert_eq!(s.used_ids, history);
        assert_eq!(s.next_familiar, counter);
        let ids = s.open(&[last]).unwrap();
        s.close(&ids).unwrap();
        assert_eq!(s.used_ids.len(), MAX_EXPLICIT_FAMILIAR_IDS + 1);
        assert!(
            s.open(&[excess])
                .unwrap_err()
                .to_string()
                .contains("explicit familiar ID lifetime limit is 256")
        );
        let ids = s.open(&[open_request("b")]).unwrap();
        assert_eq!(ids, ["familiar-1"]);
        s.close(&ids).unwrap();
        assert_eq!(s.used_ids.len(), MAX_EXPLICIT_FAMILIAR_IDS + 1);
    }

    #[test]
    fn explicit_ids_cannot_reuse_historical_generated_ids() {
        let now = Instant::now();
        let mut s = Store::default();
        for who in ["a", "b"] {
            s.register(id(who), vec![a(1, ActivityState::Working)], now)
                .unwrap();
        }
        let ids = s.open(&[open_request("b")]).unwrap();
        s.close(&ids).unwrap();
        assert!(!s.used_ids.contains(&ids[0]));
        let mut reused = open_request("b");
        reused.id = Some(ids[0].clone());
        let error = s.open(&[reused]).unwrap_err();
        assert!(error.to_string().contains("familiar ID already used"));
        assert_eq!(s.next_familiar, 2);
        assert_eq!(s.used_ids.len(), 1);
        assert_eq!(s.familiars.len(), 1);
        // Noncanonical spellings are distinct IDs and still consume explicit history.
        for id in ["familiar-01", "familiar-18446744073709551616"] {
            let mut item = open_request("b");
            item.id = Some(id.into());
            let ids = s.open(&[item]).unwrap();
            s.close(&ids).unwrap();
        }
        assert_eq!(s.next_familiar, 2);
        assert_eq!(s.used_ids.len(), 3);
    }

    #[test]
    fn explicit_future_ids_stay_reserved_and_batch_order_does_not_matter() {
        let now = Instant::now();
        let mut s = Store::default();
        for who in ["a", "b", "c"] {
            s.register(id(who), vec![a(1, ActivityState::Working)], now)
                .unwrap();
        }
        let mut future = open_request("b");
        future.id = Some("familiar-3".into());
        let ids = s.open(&[future.clone()]).unwrap();
        s.close(&ids).unwrap();
        assert_eq!(s.next_familiar, 1);
        assert!(s.open(&[future]).is_err());
        let mut explicit = open_request("c");
        explicit.id = Some("familiar-1".into());
        let ids = s.open(&[open_request("b"), explicit]).unwrap();
        assert_eq!(ids, ["familiar-2", "familiar-1"]);
        s.close(&ids).unwrap();
        assert_eq!(s.next_familiar, 3);
        let ids = s.open(&[open_request("b")]).unwrap();
        assert_eq!(ids, ["familiar-4"]);
        assert_eq!(s.next_familiar, 5);
        assert_eq!(s.used_ids.len(), 3);
        assert!(!s.used_ids.contains("familiar-2"));
        assert!(!s.used_ids.contains("familiar-4"));
    }

    #[test]
    fn closed_main_never_resurrects_and_discovery_is_selection_specific() {
        let now = Instant::now();
        let mut s = Store::default();
        s.register(id("a"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        s.register(id("b"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        let second = s.open(&[open_request("b")]).unwrap().remove(0);
        let targets = s.targets(0, None).unwrap();
        assert!(
            targets["targets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["eligible"] == false)
        );
        let own = s.targets(0, Some(&second)).unwrap();
        assert_eq!(own["targets"][1]["eligible"], true);
        assert_eq!(own["targets"][0]["eligible"], false);
        s.close(&["main".into()]).unwrap();
        update(&mut s, "a", a(2, ActivityState::Succeeded), now).unwrap();
        assert_eq!(s.presentations(now).len(), 1);
        assert!(s.status(0).focus.is_none());
        assert!(!s.status(0).focus_available);
        assert!(s.status_for(0, Some("main")).is_err());
        assert!(s.cycle().is_err());
        assert_eq!(s.targets(0, None).unwrap()["targets"][0]["eligible"], true);
        s.disconnect(&id("a"));
        assert_eq!(s.targets(0, None).unwrap()["total"], 1);
        s.close(&[second]).unwrap();
        s.register(id("c"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        assert!(s.presentations(now).is_empty());
    }

    #[test]
    fn attach_requires_live_unambiguous_target_and_controls_do_not_keep_alive() {
        let now = Instant::now();
        let mut s = Store::default();
        for instance in ["one", "two"] {
            s.register(
                ("a".into(), instance.into()),
                vec![a(1, ActivityState::Working)],
                now,
            )
            .unwrap();
        }
        let request = AttachRequest {
            id: "main".into(),
            target: target("a"),
        };
        assert!(s.attach(std::slice::from_ref(&request)).is_err());
        let mut exact = request.clone();
        exact.target.instance = Some("two".into());
        s.attach(&[exact.clone()]).unwrap();
        s.status_for(0, Some("main")).unwrap();
        s.targets(0, Some("main")).unwrap();
        s.list(0);
        assert_eq!(s.expire(now + SOURCE_TIMEOUT).len(), 2);
        assert!(s.attach(&[exact]).is_err());
        assert!(s.open(&[open_request("a")]).is_err());
    }

    #[test]
    fn capacity_and_four_row_pages_fit_worst_case_acknowledgments() {
        let now = Instant::now();
        let mut s = Store::default();
        let source = "s".repeat(96);
        let instance = "i".repeat(96);
        let activities: Vec<_> = (0..17)
            .map(|n| {
                let mut activity = a(1, ActivityState::Waiting);
                activity.activity = format!("{n:096}");
                activity.attention = true;
                activity.reason = Some("\\".repeat(128));
                activity.summary = Some("\"".repeat(160));
                activity
            })
            .collect();
        s.register((source.clone(), instance.clone()), activities.clone(), now)
            .unwrap();
        let items: Vec<_> = activities
            .iter()
            .skip(1)
            .take(15)
            .enumerate()
            .map(|(n, activity)| OpenRequest {
                id: Some(format!("{n:096}")),
                preset: "djinn".into(),
                target: TargetRequest {
                    source: source.clone(),
                    instance: Some(instance.clone()),
                    activity: activity.activity.clone(),
                },
            })
            .collect();
        s.open(&items).unwrap();
        let mut extra = items[0].clone();
        extra.id = None;
        extra.target.activity = activities[16].activity.clone();
        assert!(s.open(&[extra]).is_err());
        assert_eq!(s.familiars.len(), 16);
        for offset in [0, 4, 8, 12, 16, usize::MAX] {
            for (page, key) in [
                (s.list(offset), "familiars"),
                (s.targets(offset, None).unwrap(), "targets"),
            ] {
                assert!(page[key].as_array().unwrap().len() <= 4);
                let ack = serde_json::json!({"version":1,"ok":true,"data":page});
                assert!(serde_json::to_vec(&ack).unwrap().len() <= crate::protocol::MAX_LINE);
            }
        }
        let before = s.presentations(now);
        let mut invalid = items[0].clone();
        invalid.preset = "wisp".into();
        assert!(s.open(&[invalid]).is_err());
        assert_eq!(s.presentations(now), before);
    }
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
        let pin = s.familiars["main"].focus.as_ref().unwrap();
        assert!(!following.available);
        assert_eq!(following.source, pin.source);
        assert_eq!(following.instance, pin.instance);
        assert_eq!(following.activity, pin.activity);
    }

    #[test]
    fn reveal_is_not_a_producer_event_and_leaves_store_untouched() {
        let now = Instant::now();
        let mut s = Store::default();
        s.register(id("a"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        s.register(id("b"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        s.focus("b", None, "task").unwrap();
        update(&mut s, "b", a(2, ActivityState::Succeeded), now).unwrap();
        let before = s.presentation(now);
        let cue_deadline = s.familiars["main"].cue.as_ref().unwrap().1;
        let next_cue = s.familiars["main"].next_cue;
        for _ in 0..3 {
            let error = s
                .apply(
                    &id("b"),
                    Event::Reveal { familiars: None },
                    now + Duration::from_secs(9),
                )
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("control is not a producer event")
            );
            assert_eq!(s.presentation(now), before);
            assert_eq!(s.familiars["main"].cue.as_ref().unwrap().1, cue_deadline);
            assert_eq!(s.familiars["main"].next_cue, next_cue);
            assert_eq!(s.sources[&id("b")].seen, now);
        }
        assert_eq!(s.expire(now + SOURCE_TIMEOUT), vec![id("a"), id("b")]);
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
        let pin = s.familiars["main"].focus.clone();
        s.disconnect(&id("work"));
        assert_eq!(s.presentation(now).base, None);
        assert_eq!(s.familiars["main"].focus, pin);
        assert!(s.presentation(now).cue.is_none());
    }

    #[test]
    fn exact_conversation_focus_cycle_and_historical_states() {
        let now = Instant::now();
        let mut s = Store::default();
        assert!(s.familiars["main"].focus.is_none());
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
        assert_eq!(s.familiars["main"].focus.as_ref().unwrap().activity, "aaa");
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
        let pin = s.familiars["main"].focus.clone();
        s.apply(
            &id("a"),
            Event::Snapshot {
                activities: vec![other],
            },
            now,
        )
        .unwrap();
        assert_eq!(s.familiars["main"].focus, pin);
        assert_unavailable(&mut s, now);
        s.cycle().unwrap();
        assert_eq!(s.familiars["main"].focus.as_ref().unwrap().activity, "aaa");
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
        let pin = s.familiars["main"].focus.clone();
        s.expire(now + SOURCE_TIMEOUT);
        assert_eq!(s.familiars["main"].focus, pin);
        s.register(id("background"), vec![a(1, ActivityState::Working)], now)
            .unwrap();
        assert_unavailable(&mut s, now);
        s.cycle().unwrap();
        assert_eq!(
            s.familiars["main"].focus.as_ref().unwrap().source,
            "background"
        );
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
