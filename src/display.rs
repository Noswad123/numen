//! Translate semantic activity into a personality without coupling the wire API to a character.
use crate::activity::{Presentation, Selection};
use crate::animation::Animation;
use crate::protocol::ActivityState;
use std::sync::Mutex;

/// One pending GUI update, regardless of publisher rate. `post` returns whether
/// the caller should wake the event loop; later posts replace the pending view.
pub struct Mailbox<T = Presentation>(Mutex<Option<T>>);

impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<T> Mailbox<T> {
    pub fn post(&self, view: T) -> bool {
        let mut pending = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let wake = pending.is_none();
        *pending = Some(view);
        wake
    }

    pub fn take(&self) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }
}

#[derive(Default)]
pub struct LiveDisplay {
    view: Presentation,
    manual: bool,
    last_cue: Option<u64>,
    playing_cue: bool,
    observed: Option<Animation>,
}

fn expression(selection: Option<&Selection>) -> Animation {
    match selection.map(|s| (s.state, s.attention)) {
        Some((ActivityState::Working, _)) => Animation::Working,
        Some((ActivityState::Waiting, true)) => Animation::Waiting,
        Some((ActivityState::Waiting, false)) => Animation::Review,
        Some((ActivityState::Succeeded, _)) => Animation::Succeeded,
        Some((ActivityState::Failed, _)) => Animation::Failed,
        _ => Animation::Idle,
    }
}

fn base_expression(selection: Option<&Selection>) -> Animation {
    // A historical terminal record identifies the selected conversation, but
    // only a fresh cue may animate its outcome. Selection is not completion.
    if selection.is_some_and(|s| {
        matches!(
            s.state,
            ActivityState::Succeeded | ActivityState::Failed | ActivityState::Cancelled
        )
    }) {
        Animation::Idle
    } else {
        expression(selection)
    }
}

impl LiveDisplay {
    pub fn is_manual(&self) -> bool {
        self.manual
    }

    pub fn manual(&mut self, animation: Animation) {
        self.manual = true;
        self.playing_cue = false;
        self.observed = Some(animation);
    }

    pub fn resume(&mut self) -> Animation {
        self.manual = false;
        self.playing_cue = false;
        self.last_cue = self.view.cue.as_ref().map(|cue| cue.id).or(self.last_cue);
        let animation = base_expression(self.view.base.as_ref());
        self.observed = Some(animation);
        animation
    }

    pub fn update(&mut self, view: Presentation) -> Option<Animation> {
        self.view = view;
        if self.manual {
            self.last_cue = self.view.cue.as_ref().map(|cue| cue.id).or(self.last_cue);
            return None;
        }
        if let Some(cue) = &self.view.cue {
            if self.last_cue != Some(cue.id) {
                self.last_cue = Some(cue.id);
                self.playing_cue = true;
                let animation = expression(Some(&cue.selection));
                self.observed = Some(animation);
                return Some(animation);
            }
            if self.playing_cue {
                return None;
            }
        }
        self.playing_cue = false;
        let animation = base_expression(self.view.base.as_ref());
        if self.observed == Some(animation) {
            return None;
        }
        self.observed = Some(animation);
        Some(animation)
    }

    /// One-shot art settles to idle internally; restore the live base immediately.
    pub fn on_advance(&mut self, animation: Animation) -> Option<Animation> {
        self.observed = Some(animation);
        if !self.manual && self.playing_cue && animation == Animation::Idle {
            self.playing_cue = false;
            let base = base_expression(self.view.base.as_ref());
            self.observed = Some(base);
            if base != Animation::Idle {
                return Some(base);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Cue;

    fn selection(state: ActivityState, attention: bool) -> Selection {
        Selection {
            source: "test".into(),
            instance: "one".into(),
            activity: "task".into(),
            state,
            attention,
            reason: None,
            summary: None,
        }
    }

    #[test]
    fn waiting_attention_and_external_dependencies_are_distinct() {
        assert_eq!(
            expression(Some(&selection(ActivityState::Waiting, true))),
            Animation::Waiting
        );
        assert_eq!(
            expression(Some(&selection(ActivityState::Waiting, false))),
            Animation::Review
        );
        assert_eq!(
            expression(Some(&selection(ActivityState::Cancelled, false))),
            Animation::Idle
        );
    }

    #[test]
    fn mailbox_coalesces_updates_and_requests_only_one_wake() {
        let mailbox = Mailbox::default();
        assert!(mailbox.post(Presentation::default()));
        let latest = Presentation {
            base: Some(selection(ActivityState::Working, false)),
            cue: None,
            following: None,
        };
        assert!(!mailbox.post(latest.clone()));
        assert_eq!(mailbox.take(), Some(latest));
        assert_eq!(mailbox.take(), None);
        assert!(mailbox.post(Presentation::default()));
    }

    #[test]
    fn registry_mailbox_replaces_whole_snapshots_including_empty_registry() {
        let mailbox = Mailbox::<Vec<crate::activity::FamiliarPresentation>>::default();
        assert!(mailbox.post(vec![crate::activity::FamiliarPresentation {
            id: "main".into(),
            preset: "djinn".into(),
            presentation: Presentation::default(),
        }]));
        assert!(!mailbox.post(vec![]));
        assert_eq!(mailbox.take(), Some(vec![]));
        assert!(mailbox.take().is_none());
    }

    #[test]
    fn one_shot_restores_base_and_never_replays_same_cue() {
        let mut display = LiveDisplay::default();
        let view = Presentation {
            base: Some(selection(ActivityState::Working, false)),
            cue: Some(Cue {
                id: 1,
                selection: selection(ActivityState::Succeeded, false),
            }),
            following: None,
        };
        assert_eq!(display.update(view.clone()), Some(Animation::Succeeded));
        assert_eq!(display.update(view.clone()), None);
        assert_eq!(
            display.on_advance(Animation::Idle),
            Some(Animation::Working)
        );
        assert_eq!(display.update(view), None);
        assert_eq!(
            display.update(Presentation::default()),
            Some(Animation::Idle)
        );
    }

    #[test]
    fn selecting_terminal_conversations_never_replays_outcomes() {
        for state in [
            ActivityState::Succeeded,
            ActivityState::Failed,
            ActivityState::Cancelled,
        ] {
            let mut display = LiveDisplay::default();
            let historical = Presentation {
                base: Some(selection(state, false)),
                cue: None,
                following: None,
            };
            assert_eq!(display.update(historical.clone()), Some(Animation::Idle));
            assert_eq!(display.update(historical), None);
            assert_eq!(display.resume(), Animation::Idle);
        }
    }

    #[test]
    fn focused_terminal_cue_finishes_once_then_rests() {
        let mut display = LiveDisplay::default();
        let terminal = selection(ActivityState::Failed, false);
        let view = Presentation {
            base: Some(terminal.clone()),
            cue: Some(Cue {
                id: 1,
                selection: terminal,
            }),
            following: None,
        };
        assert_eq!(display.update(view.clone()), Some(Animation::Failed));
        assert_eq!(display.on_advance(Animation::Idle), None);
        assert_eq!(display.update(view), None);
        assert_eq!(display.resume(), Animation::Idle);
    }

    #[test]
    fn summary_only_updates_do_not_restart_the_expression() {
        let mut display = LiveDisplay::default();
        let mut selected = selection(ActivityState::Working, false);
        selected.summary = Some("Investigating retries".into());
        let mut view = Presentation {
            base: Some(selected),
            cue: None,
            following: None,
        };
        assert_eq!(display.update(view.clone()), Some(Animation::Working));
        view.base.as_mut().unwrap().summary = Some("Fixing retry backoff".into());
        assert_eq!(display.update(view), None);
    }

    #[test]
    fn attention_interrupts_cue_and_manual_override_waits_for_resume() {
        let mut display = LiveDisplay::default();
        let attention = Presentation {
            base: Some(selection(ActivityState::Waiting, true)),
            cue: None,
            following: None,
        };
        assert_eq!(
            display.update(Presentation {
                base: None,
                cue: Some(Cue {
                    id: 1,
                    selection: selection(ActivityState::Failed, false)
                }),
                following: None
            }),
            Some(Animation::Failed)
        );
        assert_eq!(display.update(attention.clone()), Some(Animation::Waiting));
        display.manual(Animation::Review);
        assert_eq!(display.update(attention), None);
        assert_eq!(display.on_advance(Animation::Review), None);
        assert_eq!(display.resume(), Animation::Waiting);
    }
}
