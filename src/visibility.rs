//! Clock-driven visibility policy, independent of rendering and event delivery.

use std::time::{Duration, Instant};

use crate::{activity::Presentation, bubble};

const QUIET_PERIOD: Duration = Duration::from_millis(750);

pub struct BubbleVisibility {
    enabled: bool,
    always: bool,
    duration: Duration,
    until: Option<Instant>,
    quiet_until: Option<Instant>,
    last_text: Option<bubble::BubbleText>,
    has_following: bool,
}

impl BubbleVisibility {
    pub fn new(enabled: bool, always: bool, duration: Duration) -> Self {
        Self {
            enabled,
            always,
            duration,
            until: None,
            quiet_until: None,
            last_text: None,
            has_following: false,
        }
    }

    /// Changes only the lifetime of future accepted reveals, not current deadlines.
    pub fn set_duration(&mut self, duration: Duration) {
        self.duration = duration;
    }

    /// Returns whether a notification was accepted, not whether layout changed.
    pub fn observe(&mut self, view: &Presentation, now: Instant) -> bool {
        self.expire(now);
        let text = bubble::text(view, None);
        let has_following = view.following.is_some();
        let new_selection = has_following
            && (!self.has_following
                || self
                    .last_text
                    .as_ref()
                    .is_none_or(|old| old.identity != text.identity));
        let changed = self.last_text.as_ref().is_some_and(|old| *old != text);
        // Cache even suppressed changes: they must not become delayed notifications.
        self.last_text = Some(text);
        self.has_following = has_following;

        if new_selection {
            return self.reveal(now);
        }
        if !self.enabled || !changed {
            return false;
        }
        if self.visible(now) {
            return true;
        }
        if self.quiet_until.is_some_and(|until| now < until) {
            return false;
        }
        self.reveal(now)
    }

    /// Explicit requests bypass quiet time and refresh the full transient lifetime.
    pub fn reveal(&mut self, now: Instant) -> bool {
        if !self.enabled {
            return false;
        }
        if !self.always {
            self.until = Some(now + self.duration);
            self.quiet_until = None;
        }
        true
    }

    pub fn visible(&self, now: Instant) -> bool {
        self.enabled && (self.always || self.until.is_some_and(|until| now < until))
    }

    /// Only the pending hide needs a wakeup; quiet-time completion is not an event.
    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// Returns true once for each pending timed hide, even if called late.
    pub fn expire(&mut self, now: Instant) -> bool {
        if !self.enabled {
            self.quiet_until = None;
            return self.until.take().is_some();
        }
        if let Some(until) = self.until
            && now >= until
        {
            self.until = None;
            // Anchor to the deadline, not a potentially delayed GUI wakeup.
            self.quiet_until = Some(until + QUIET_PERIOD);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        activity::{Cue, Following, Selection},
        protocol::ActivityState,
    };

    const LIFETIME: Duration = Duration::from_secs(5);

    fn policy() -> BubbleVisibility {
        BubbleVisibility::new(true, false, LIFETIME)
    }

    #[test]
    fn duration_edits_only_affect_future_accepted_reveals() {
        let now = Instant::now();
        let shorter = Duration::from_millis(250);
        let longer = Duration::from_secs(30);
        let mut p = policy();
        p.set_duration(shorter);
        assert!(!p.visible(now));
        assert_eq!(p.deadline(), None);
        assert!(p.observe(&view(), now));
        let end = now + shorter;
        let text = p.last_text.clone();
        p.set_duration(longer);
        assert_eq!(p.deadline(), Some(end));
        assert_eq!(p.last_text, text);
        assert!(p.has_following);
        assert!(p.visible(end - Duration::from_nanos(1)));
        assert!(p.expire(end));
        let quiet = p.quiet_until;
        p.set_duration(shorter);
        assert_eq!(p.quiet_until, quiet);
        assert_eq!(p.last_text, text);
        assert!(!p.visible(end));
        let mut changed = view();
        changed.following.as_mut().unwrap().summary = Some("quiet update".into());
        assert!(!p.observe(&changed, end));
        p.set_duration(longer);
        assert!(!p.observe(&changed, end + QUIET_PERIOD));
        assert!(p.reveal(end));
        assert_eq!(p.deadline(), Some(end + longer));
        // Shortening an already visible reveal must not shorten it either.
        p.set_duration(shorter);
        assert_eq!(p.deadline(), Some(end + longer));
        assert!(p.reveal(end));
        assert_eq!(p.deadline(), Some(end + shorter));
    }

    #[test]
    fn duration_edits_preserve_always_and_disabled_policies() {
        let now = Instant::now();
        for enabled in [false, true] {
            for always in [false, true] {
                let mut p = BubbleVisibility::new(enabled, always, LIFETIME);
                p.observe(&view(), now);
                let deadline = p.deadline();
                let text = p.last_text.clone();
                p.set_duration(Duration::from_millis(250));
                assert_eq!(p.deadline(), deadline);
                assert_eq!(p.last_text, text);
                assert_eq!(p.visible(now), enabled);
                assert_eq!(p.reveal(now), enabled);
                if !enabled || always {
                    assert_eq!(p.deadline(), None);
                    assert_eq!(p.visible(now + LIFETIME), enabled && always);
                }
            }
        }
    }

    fn view() -> Presentation {
        Presentation {
            base: Some(Selection {
                source: "opencode".into(),
                instance: "session-1".into(),
                activity: "conversation-7".into(),
                state: ActivityState::Working,
                attention: false,
                reason: None,
                summary: Some("Working on visibility".into()),
            }),
            cue: None,
            following: Some(Following {
                source: "opencode".into(),
                instance: "session-1".into(),
                activity: "conversation-7".into(),
                summary: Some("Working on visibility".into()),
                available: true,
            }),
        }
    }

    #[test]
    fn starts_hidden_without_empty_startup_notification() {
        let now = Instant::now();
        let mut p = policy();
        assert!(!p.visible(now));
        assert_eq!(p.deadline(), None);
        assert!(!p.expire(now));
        assert!(!p.observe(&Presentation::default(), now));
        assert!(!p.observe(&Presentation::default(), now + LIFETIME));
        assert!(!p.visible(now + LIFETIME));
        assert!(p.observe(&view(), now + LIFETIME));
        assert!(p.visible(now + LIFETIME));
    }

    #[test]
    fn first_focus_status_attention_and_summary_notify() {
        let now = Instant::now();
        let mut p = policy();
        let mut v = view();
        assert!(p.observe(&v, now));
        let next = now + LIFETIME + QUIET_PERIOD;
        v.base.as_mut().unwrap().state = ActivityState::Waiting;
        assert!(p.observe(&v, next));
        let next = next + LIFETIME + QUIET_PERIOD;
        v.base.as_mut().unwrap().attention = true;
        assert!(p.observe(&v, next));
        let next = next + LIFETIME + QUIET_PERIOD;
        v.following.as_mut().unwrap().summary = Some("Revised summary".into());
        assert!(p.observe(&v, next));
        assert_eq!(p.deadline(), Some(next + LIFETIME));
    }

    #[test]
    fn same_view_heartbeat_reason_background_and_cue_expiry_are_noops() {
        let now = Instant::now();
        let mut p = policy();
        let mut v = view();
        assert!(p.observe(&v, now));
        assert!(!p.observe(&v, now + Duration::from_millis(10)));
        v.base.as_mut().unwrap().reason = Some("New producer detail".into());
        v.base.as_mut().unwrap().summary = Some("Not the following summary".into());
        assert!(!p.observe(&v, now + Duration::from_millis(20)));
        let mut background = v.base.clone().unwrap();
        background.source = "background".into();
        background.state = ActivityState::Failed;
        v.cue = Some(Cue {
            id: 1,
            selection: background,
        });
        assert!(!p.observe(&v, now + Duration::from_millis(30)));
        v.cue.as_mut().unwrap().id = 2;
        assert!(!p.observe(&v, now + Duration::from_millis(40)));
        v.cue = None;
        assert!(!p.observe(&v, now + Duration::from_millis(1500)));
        assert_eq!(p.deadline(), Some(now + LIFETIME));
        assert!(!p.observe(&v, now + LIFETIME + QUIET_PERIOD));
        assert!(!p.visible(now + LIFETIME + QUIET_PERIOD));
    }

    #[test]
    fn repeated_updates_never_extend_and_suppressed_keys_are_cached() {
        let now = Instant::now();
        let mut p = policy();
        let mut v = view();
        assert!(p.observe(&v, now));
        for millis in (100..5000).step_by(100) {
            v.following.as_mut().unwrap().summary = Some(format!("Update {millis}"));
            assert!(p.observe(&v, now + Duration::from_millis(millis)));
            assert_eq!(p.deadline(), Some(now + LIFETIME));
        }
        v.following.as_mut().unwrap().summary = Some("At expiry".into());
        assert!(!p.observe(&v, now + LIFETIME));
        assert!(!p.visible(now + LIFETIME));
        assert_eq!(p.deadline(), None);
        v.following.as_mut().unwrap().summary = Some("During quiet".into());
        assert!(!p.observe(&v, now + LIFETIME + Duration::from_millis(749)));
        assert!(!p.observe(&v, now + LIFETIME + QUIET_PERIOD));
        v.following.as_mut().unwrap().summary = Some("At quiet boundary".into());
        assert!(p.observe(&v, now + LIFETIME + QUIET_PERIOD));
    }

    #[test]
    fn exact_expiry_and_late_expiry_anchor_quiet_to_old_deadline() {
        let now = Instant::now();
        let mut p = policy();
        assert!(p.reveal(now));
        let end = now + LIFETIME;
        assert!(p.visible(end - Duration::from_nanos(1)));
        assert!(!p.expire(end - Duration::from_nanos(1)));
        assert!(!p.visible(end));
        assert!(p.expire(end));
        assert!(!p.expire(end));

        let mut p = policy();
        let mut v = view();
        assert!(p.observe(&v, now));
        v.base.as_mut().unwrap().state = ActivityState::Failed;
        let late = end + QUIET_PERIOD;
        assert!(p.observe(&v, late));
        assert_eq!(p.deadline(), Some(late + LIFETIME));
    }

    #[test]
    fn explicit_reveal_refreshes_and_bypasses_quiet_even_without_context() {
        let now = Instant::now();
        let mut p = policy();
        assert!(p.reveal(now));
        let refresh = now + Duration::from_secs(4);
        assert!(p.reveal(refresh));
        assert_eq!(p.deadline(), Some(refresh + LIFETIME));
        assert!(!p.expire(now + LIFETIME));
        assert!(p.expire(refresh + LIFETIME));
        let quiet = refresh + LIFETIME + Duration::from_millis(1);
        assert!(p.reveal(quiet));
        assert_eq!(p.deadline(), Some(quiet + LIFETIME));
    }

    #[test]
    fn focus_switch_bypasses_quiet_and_refreshes_visible_bubble() {
        let now = Instant::now();
        let mut p = policy();
        let mut v = view();
        assert!(p.observe(&v, now));
        v.following.as_mut().unwrap().activity = "conversation-8".into();
        let switched = now + Duration::from_secs(1);
        assert!(p.observe(&v, switched));
        assert_eq!(p.deadline(), Some(switched + LIFETIME));
        v.base.as_mut().unwrap().state = ActivityState::Failed;
        assert!(p.observe(&v, switched + Duration::from_secs(1)));
        assert_eq!(p.deadline(), Some(switched + LIFETIME));
        let quiet = switched + LIFETIME;
        assert!(p.expire(quiet));
        v.following.as_mut().unwrap().instance = "session-2".into();
        assert!(p.observe(&v, quiet));
        assert_eq!(p.deadline(), Some(quiet + LIFETIME));

        let mut p = policy();
        assert!(p.reveal(now));
        assert!(p.expire(now + LIFETIME));
        // A first following target is a selection, not an automatic message.
        assert!(p.observe(&view(), now + LIFETIME));
    }

    #[test]
    fn unavailable_context_notifies_once_and_keeps_identity() {
        let now = Instant::now();
        let mut p = policy();
        let mut v = view();
        assert!(p.observe(&v, now));
        let next = now + LIFETIME + QUIET_PERIOD;
        v.following.as_mut().unwrap().available = false;
        v.base = None;
        assert!(p.observe(&v, next));
        assert_eq!(p.last_text.as_ref().unwrap().status, "Unavailable");
        assert_eq!(
            p.last_text.as_ref().unwrap().identity,
            bubble::text(&view(), None).identity
        );
        assert!(!p.observe(&v, next + Duration::from_secs(1)));
        assert_eq!(p.deadline(), Some(next + LIFETIME));
        assert!(p.expire(next + LIFETIME));
        assert!(!p.observe(&v, next + LIFETIME + QUIET_PERIOD));
        assert!(!p.visible(next + LIFETIME + QUIET_PERIOD));
    }

    #[test]
    fn hard_disable_overrides_always_and_reveal() {
        let now = Instant::now();
        for always in [false, true] {
            let mut p = BubbleVisibility::new(false, always, LIFETIME);
            assert!(!p.visible(now));
            assert!(!p.observe(&view(), now));
            assert!(!p.reveal(now));
            assert!(!p.expire(now + LIFETIME));
            assert_eq!(p.deadline(), None);
        }
    }

    #[test]
    fn always_visible_has_no_mandatory_deadline() {
        let now = Instant::now();
        let mut p = BubbleVisibility::new(true, true, LIFETIME);
        assert!(p.visible(now));
        assert!(!p.observe(&Presentation::default(), now));
        assert!(p.observe(&view(), now));
        assert!(p.reveal(now));
        assert_eq!(p.deadline(), None);
        assert!(!p.expire(now + LIFETIME));
        assert!(p.visible(now + LIFETIME));
    }
}
