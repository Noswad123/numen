//! Bounded, process-local runtime facts, separate from service capabilities.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::{
    build_info::{BuildInfo, current},
    hotkey::Shortcut,
};

#[derive(Clone)]
pub struct RuntimeDiagnostics {
    snapshot: Arc<Mutex<Snapshot>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub executable: Option<String>,
    pub build: BuildInfo,
    pub hotkeys: Hotkeys,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferences: Option<crate::preferences::Status>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkeys {
    pub option_i: ShortcutStatus,
    pub option_m: ShortcutStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_c: Option<ShortcutStatus>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutStatus {
    pub state: ShortcutState,
    pub reason: Option<ShortcutReason>,
}

impl ShortcutStatus {
    pub fn valid_for(&self, shortcut: Shortcut) -> bool {
        use ShortcutReason::*;
        use ShortcutState::*;
        match (self.state, self.reason) {
            (Registered, None)
            | (Disabled, Some(NoHotkey))
            | (Unsupported, Some(UnsupportedPlatform))
            | (Unavailable, Some(RegistrationFailed | Headless))
            | (Uninitialized, Some(RegistrationPending | NotReported)) => true,
            (Disabled, Some(NoBubble)) => shortcut == Shortcut::Reveal,
            (Unsupported, Some(UnsupportedMode)) => {
                matches!(shortcut, Shortcut::GoToMouse | Shortcut::GoToCorner)
            }
            (Unavailable, Some(PointerInitializationFailed)) => shortcut == Shortcut::GoToMouse,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutState {
    Registered,
    Disabled,
    Unsupported,
    Unavailable,
    Uninitialized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutReason {
    NoHotkey,
    NoBubble,
    UnsupportedPlatform,
    PointerInitializationFailed,
    RegistrationFailed,
    RegistrationPending,
    NotReported,
    Headless,
    UnsupportedMode,
}

fn executable(path: PathBuf) -> Option<String> {
    let path = path.into_os_string().into_string().ok()?;
    (path.len() <= 1024
        && !path.chars().any(|c| c.is_control() || matches!(c,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')))
        .then_some(path)
}

impl Snapshot {
    pub fn new(graphical: bool) -> Self {
        let status = if graphical {
            ShortcutStatus {
                state: ShortcutState::Uninitialized,
                reason: Some(ShortcutReason::NotReported),
            }
        } else {
            ShortcutStatus {
                state: ShortcutState::Unavailable,
                reason: Some(ShortcutReason::Headless),
            }
        };
        Self {
            version: 1,
            executable: std::env::current_exe().ok().and_then(executable),
            build: current(),
            hotkeys: Hotkeys {
                option_i: status,
                option_m: status,
                option_c: Some(status),
            },
            preferences: Some(crate::preferences::Status::disabled()),
        }
    }
}

impl RuntimeDiagnostics {
    pub fn set_preferences_status(&self, status: crate::preferences::Status) {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .preferences = Some(status);
    }

    pub fn new(graphical: bool) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(Snapshot::new(graphical))),
        }
    }

    /// Return an owned snapshot; callers serialize only after releasing the lock.
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_hotkey(&self, shortcut: Shortcut, status: ShortcutStatus) {
        let mut snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        match shortcut {
            Shortcut::Reveal => snapshot.hotkeys.option_i = status,
            Shortcut::GoToMouse => snapshot.hotkeys.option_m = status,
            Shortcut::GoToCorner => snapshot.hotkeys.option_c = Some(status),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_status_validity_matrix() {
        use ShortcutReason::*;
        use ShortcutState::*;
        let reasons = [
            None,
            Some(NoHotkey),
            Some(NoBubble),
            Some(UnsupportedPlatform),
            Some(PointerInitializationFailed),
            Some(RegistrationFailed),
            Some(RegistrationPending),
            Some(NotReported),
            Some(Headless),
            Some(UnsupportedMode),
        ];
        for shortcut in [Shortcut::Reveal, Shortcut::GoToMouse, Shortcut::GoToCorner] {
            let valid = [
                vec![None],
                if shortcut == Shortcut::Reveal {
                    vec![Some(NoHotkey), Some(NoBubble)]
                } else {
                    vec![Some(NoHotkey)]
                },
                if matches!(shortcut, Shortcut::GoToMouse | Shortcut::GoToCorner) {
                    vec![Some(UnsupportedPlatform), Some(UnsupportedMode)]
                } else {
                    vec![Some(UnsupportedPlatform)]
                },
                if shortcut == Shortcut::GoToMouse {
                    vec![
                        Some(RegistrationFailed),
                        Some(Headless),
                        Some(PointerInitializationFailed),
                    ]
                } else {
                    vec![Some(RegistrationFailed), Some(Headless)]
                },
                vec![Some(RegistrationPending), Some(NotReported)],
            ];
            for (state, allowed) in [
                Registered,
                Disabled,
                Unsupported,
                Unavailable,
                Uninitialized,
            ]
            .into_iter()
            .zip(valid)
            {
                for reason in reasons {
                    assert_eq!(
                        (ShortcutStatus { state, reason }).valid_for(shortcut),
                        allowed.contains(&reason),
                        "{shortcut:?}: {state:?}/{reason:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn defaults_report_headless_and_unreported_graphical() {
        for graphical in [false, true] {
            let snapshot = RuntimeDiagnostics::new(graphical).snapshot();
            assert_eq!(snapshot.version, 1);
            assert_eq!(snapshot.build, current());
            assert_eq!(snapshot.hotkeys.option_i, snapshot.hotkeys.option_m);
            assert_eq!(snapshot.hotkeys.option_c, Some(snapshot.hotkeys.option_i));
            assert_eq!(
                snapshot.hotkeys.option_i.reason,
                Some(if graphical {
                    ShortcutReason::NotReported
                } else {
                    ShortcutReason::Headless
                })
            );
            assert_eq!(
                snapshot.hotkeys.option_i.state,
                if graphical {
                    ShortcutState::Uninitialized
                } else {
                    ShortcutState::Unavailable
                }
            );
            let bytes = serde_json::to_vec(&snapshot).unwrap();
            assert_eq!(
                serde_json::from_slice::<Snapshot>(&bytes).unwrap(),
                snapshot
            );
        }
    }

    #[test]
    fn cloned_handle_updates_independent_keys_without_changing_old_snapshots() {
        let diagnostics = RuntimeDiagnostics::new(true);
        let handle = diagnostics.clone();
        let before = diagnostics.snapshot();
        let pending = ShortcutStatus {
            state: ShortcutState::Uninitialized,
            reason: Some(ShortcutReason::RegistrationPending),
        };
        handle.set_hotkey(Shortcut::Reveal, pending);
        assert_eq!(diagnostics.snapshot().hotkeys.option_i, pending);
        assert_eq!(
            diagnostics.snapshot().hotkeys.option_m,
            before.hotkeys.option_m
        );
        let registered = ShortcutStatus {
            state: ShortcutState::Registered,
            reason: None,
        };
        std::thread::spawn(move || handle.set_hotkey(Shortcut::Reveal, registered))
            .join()
            .unwrap();
        assert_eq!(diagnostics.snapshot().hotkeys.option_i, registered);
        assert_eq!(
            before.hotkeys.option_i.reason,
            Some(ShortcutReason::NotReported)
        );
        diagnostics.set_hotkey(Shortcut::GoToMouse, pending);
        assert_eq!(diagnostics.snapshot().hotkeys.option_i, registered);
        assert_eq!(diagnostics.snapshot().hotkeys.option_m, pending);
        diagnostics.set_hotkey(Shortcut::GoToCorner, registered);
        assert_eq!(diagnostics.snapshot().hotkeys.option_c, Some(registered));
        assert_eq!(diagnostics.snapshot().hotkeys.option_i, registered);
        assert_eq!(diagnostics.snapshot().hotkeys.option_m, pending);
        assert_eq!(before.hotkeys.option_c, Some(before.hotkeys.option_i));
    }

    #[test]
    fn older_v1_without_corner_status_round_trips_without_adding_it() {
        let mut value = serde_json::to_value(Snapshot::new(true)).unwrap();
        value["hotkeys"].as_object_mut().unwrap().remove("option_c");
        let snapshot: Snapshot = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(snapshot.hotkeys.option_c, None);
        assert_eq!(serde_json::to_value(snapshot).unwrap(), value);
    }

    #[test]
    fn executable_is_owned_utf8_bounded_and_control_and_bidi_free() {
        use std::os::unix::ffi::OsStringExt;
        assert_eq!(
            executable(PathBuf::from("/safe/path")),
            Some("/safe/path".into())
        );
        assert!(executable(PathBuf::from("x".repeat(1024))).is_some());
        assert!(executable(PathBuf::from("x".repeat(1025))).is_none());
        assert!(executable(PathBuf::from("\u{e9}".repeat(513))).is_none());
        assert!(executable(PathBuf::from(std::ffi::OsString::from_vec(vec![255]))).is_none());
        for c in [
            '\0', '\n', '\u{7f}', '\u{85}', '\u{061c}', '\u{200e}', '\u{200f}', '\u{202a}',
            '\u{202e}', '\u{2066}', '\u{2069}',
        ] {
            assert!(executable(PathBuf::from(format!("/path/{c}"))).is_none());
        }
    }
}
