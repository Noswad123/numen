//! Declarative familiar controls. Native callbacks only record a selection; the
//! caller revalidates lifecycle/capabilities and executes it after tracking ends.
use std::sync::{Arc, atomic::AtomicBool};

use crate::activity::FocusTarget;
use crate::protocol::{FamiliarSettings, SettingsPatch};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatChoice {
    pub target: FocusTarget,
    pub incarnation: u64,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentContext {
    pub current: Option<FocusTarget>,
    pub available: bool,
    pub choices: Vec<ChatChoice>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    RevealStatus,
    CopySessionId,
    OpenChat,
    GoToMouse,
    SetPaused(bool),
    Settings(SettingsPatch),
    ForgetPreferences,
    Attach(usize),
    OpenFamiliar,
    CloseFamiliar,
    QuitNumen,
}

impl Action {
    pub fn is_service(self) -> bool {
        matches!(self, Self::OpenFamiliar | Self::QuitNumen)
    }
}

pub struct Context {
    pub paused: bool,
    pub bubbles: bool,
    pub copy_session_id: bool,
    pub open_chat: bool,
    pub navigation: bool,
    pub settings: Option<FamiliarSettings>,
    pub preferences: bool,
    pub attachments: Option<AttachmentContext>,
    pub open_familiar: Option<bool>,
}

/// The only menu catalog: add future controls here, not in the native renderer.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Item {
        label: String,
        action: Option<Action>,
        enabled: bool,
        checked: bool,
        tooltip: Option<String>,
    },
    Submenu {
        label: String,
        entries: Vec<Entry>,
    },
    Separator,
}

pub fn entries(context: Context) -> Vec<Entry> {
    let mut entries = vec![
        Entry::Item {
            label: "Show status".into(),
            action: Some(Action::RevealStatus),
            enabled: context.bubbles,
            checked: false,
            tooltip: None,
        },
        Entry::Item {
            label: "Copy session ID".into(),
            action: Some(Action::CopySessionId),
            enabled: context.copy_session_id,
            checked: false,
            tooltip: None,
        },
        Entry::Item {
            label: "Open chat".into(),
            action: Some(Action::OpenChat),
            enabled: context.open_chat,
            checked: false,
            tooltip: Some("Open or focus this live Djinn chat in a compact Wisp window".into()),
        },
        Entry::Item {
            label: "Go to mouse".into(),
            action: Some(Action::GoToMouse),
            enabled: context.navigation,
            checked: false,
            tooltip: None,
        },
        Entry::Item {
            label: if context.paused { "Resume" } else { "Pause" }.into(),
            action: Some(Action::SetPaused(!context.paused)),
            enabled: true,
            checked: context.paused,
            tooltip: None,
        },
    ];
    if let Some(settings) = context.settings.filter(|s| s.validate().is_ok()) {
        let fields = crate::menu::SETTINGS
            .iter()
            .map(|(field, label, presets)| {
                let current = match *field {
                    "sprite_scale" => settings.sprite_scale,
                    "font_size" => settings.font_size,
                    "bubble_duration" => settings.bubble_duration,
                    _ => unreachable!("trusted settings catalog"),
                };
                let mut values: Vec<_> = presets
                    .iter()
                    .map(|(name, value)| {
                        let mut patch = SettingsPatch::default();
                        match *field {
                            "sprite_scale" => patch.sprite_scale = Some(*value),
                            "font_size" => patch.font_size = Some(*value),
                            "bubble_duration" => patch.bubble_duration = Some(*value),
                            _ => unreachable!("trusted settings catalog"),
                        }
                        Entry::Item {
                            label: format!("{name} ({value})"),
                            action: Some(Action::Settings(patch)),
                            enabled: true,
                            checked: current == *value,
                            tooltip: None,
                        }
                    })
                    .collect();
                if !presets.iter().any(|(_, value)| current == *value) {
                    values.push(Entry::Item {
                        label: format!("Custom ({current})"),
                        action: None,
                        enabled: false,
                        checked: true,
                        tooltip: None,
                    });
                }
                Entry::Submenu {
                    label: (*label).into(),
                    entries: values,
                }
            })
            .collect();
        entries.push(Entry::Submenu {
            label: "Settings".into(),
            entries: fields,
        });
    }
    if context.preferences {
        entries.push(Entry::Item {
            label: "Forget saved preferences".into(),
            action: Some(Action::ForgetPreferences),
            enabled: true,
            checked: false,
            tooltip: None,
        });
    }
    if let Some(attachments) = context.attachments {
        entries.push(attachment_entries(attachments));
    }
    if let Some(enabled) = context.open_familiar {
        entries.push(Entry::Item {
            label: "Open another familiar".into(),
            action: Some(Action::OpenFamiliar),
            enabled,
            checked: false,
            tooltip: None,
        });
    }
    entries.extend([
        Entry::Separator,
        Entry::Item {
            label: "Close familiar".into(),
            action: Some(Action::CloseFamiliar),
            enabled: true,
            checked: false,
            tooltip: None,
        },
        Entry::Separator,
        Entry::Item {
            label: "Quit Numen (all familiars)".into(),
            action: Some(Action::QuitNumen),
            enabled: true,
            checked: false,
            tooltip: None,
        },
    ]);
    entries
}

fn unsafe_menu_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn short_label(value: &str, limit: usize) -> String {
    let clean: String = value
        .chars()
        .map(|c| if unsafe_menu_char(c) { ' ' } else { c })
        .collect();
    let clean = clean.trim();
    let mut label: String = clean.chars().take(limit).collect();
    if clean.chars().count() > limit {
        label.push_str("...");
    }
    label
}

fn target_details(target: &FocusTarget) -> String {
    // Escaping (rather than removing) controls keeps the exact tuple recoverable.
    fn exact(value: &str) -> String {
        value
            .chars()
            .flat_map(|c| {
                if unsafe_menu_char(c) || matches!(c, '\\' | '"') {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            })
            .collect()
    }
    format!(
        "source=\"{}\"\ninstance=\"{}\"\nactivity=\"{}\"",
        exact(&target.source),
        exact(&target.instance),
        exact(&target.activity)
    )
}

fn attachment_entries(context: AttachmentContext) -> Entry {
    let mut rows = Vec::new();
    let disabled = |label: &str, tooltip| Entry::Item {
        label: label.into(),
        action: None,
        enabled: false,
        checked: false,
        tooltip,
    };
    if !context.available {
        rows.push(disabled("Chat list updating—reopen menu", None));
    } else {
        if let Some(current) = &context.current
            && !context
                .choices
                .iter()
                .any(|choice| &choice.target == current)
        {
            rows.push(disabled(
                "Current chat unavailable",
                Some(target_details(current)),
            ));
        }
        if context.choices.is_empty() {
            rows.push(disabled("No compatible live chats", None));
        }
        let choices: Vec<_> = context
            .choices
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let target = &choice.target;
                let title = choice
                    .summary
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("Chat");
                Entry::Item {
                    label: format!(
                        "{} | {}/{}/{} #{}",
                        short_label(title, 32),
                        short_label(&target.source, 12),
                        short_label(&target.activity, 12),
                        short_label(&target.instance, 12),
                        index + 1
                    ),
                    action: Some(Action::Attach(index)),
                    enabled: true,
                    checked: context.current.as_ref() == Some(target),
                    tooltip: Some(target_details(target)),
                }
            })
            .collect();
        if choices.len() <= 16 {
            rows.extend(choices);
        } else {
            // Keep original indices in each page; tags never depend on later memo order.
            rows.extend(
                choices
                    .chunks(16)
                    .enumerate()
                    .map(|(page, choices)| Entry::Submenu {
                        label: format!("Chats {}-{}", page * 16 + 1, page * 16 + choices.len()),
                        entries: choices.to_vec(),
                    }),
            );
        }
    }
    Entry::Submenu {
        label: if context.current.is_some() {
            "Change attachment"
        } else {
            "Attach to app / chat"
        }
        .into(),
        entries: rows,
    }
}

pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// Writes exact plain text only in response to an explicit menu action.
pub fn copy_text(text: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return native::copy_text(text);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = text;
        anyhow::bail!("Familiar clipboard actions are supported only on macOS")
    }
}

/// An original mouse-down captured on the main thread, not the later drag event.
/// Keep the caller's window alive until `start` returns.
pub struct DragStart {
    #[cfg(target_os = "macos")]
    event: objc2::rc::Retained<objc2_app_kit::NSEvent>,
    #[cfg(target_os = "macos")]
    native_window: objc2::rc::Retained<objc2_app_kit::NSWindow>,
    #[cfg(target_os = "macos")]
    window_id: winit::window::WindowId,
    _main_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

/// Call synchronously during the actual left MouseInput::Pressed callback.
pub fn capture_drag(window: &winit::window::Window) -> anyhow::Result<DragStart> {
    #[cfg(target_os = "macos")]
    return native::capture_drag(window);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        anyhow::bail!("Native drag capture is supported only on macOS")
    }
}

impl DragStart {
    /// Returning does not imply mouse-up; track `left_button_down()` separately.
    pub fn start(&self, window: &winit::window::Window) -> anyhow::Result<()> {
        #[cfg(target_os = "macos")]
        return native::start_drag(self, window);
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            anyhow::bail!("Native drag capture is supported only on macOS")
        }
    }
}

/// Read-only button state; no event taps, input posting, or permissions.
pub fn left_button_down() -> bool {
    #[cfg(target_os = "macos")]
    {
        if objc2_foundation::MainThreadMarker::new().is_none() {
            return false;
        }
        // SAFETY: A main-thread, read-only AppKit class query.
        unsafe { objc2_app_kit::NSEvent::pressedMouseButtons() & 1 != 0 }
    }
    #[cfg(not(target_os = "macos"))]
    false
}

#[cfg(any(target_os = "macos", test))]
fn validate_drag_origin(
    is_left_down: bool,
    event_window: isize,
    window: isize,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_left_down,
        "Native drag requires the original left mouse-down"
    );
    anyhow::ensure!(
        window > 0 && event_window == window,
        "Mouse-down belongs to a different native window"
    );
    Ok(())
}

pub fn show(
    window: &winit::window::Window,
    position: winit::dpi::PhysicalPosition<f64>,
    context: Context,
    cancelled: Arc<AtomicBool>,
) -> anyhow::Result<Option<Action>> {
    #[cfg(target_os = "macos")]
    return native::show(window, position, context, cancelled);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (window, position, context, cancelled);
        anyhow::bail!("Native familiar context menus are supported only on macOS")
    }
}

#[cfg(any(target_os = "macos", test))]
fn logical_position(
    position: winit::dpi::PhysicalPosition<f64>,
    size: winit::dpi::PhysicalSize<u32>,
    scale: f64,
) -> anyhow::Result<(f64, f64)> {
    anyhow::ensure!(
        scale.is_finite()
            && scale > 0.0
            && position.x.is_finite()
            && position.y.is_finite()
            && (0.0..=f64::from(size.width)).contains(&position.x)
            && (0.0..=f64::from(size.height)).contains(&position.y),
        "Context menu position must be finite and inside the window"
    );
    let point = (position.x / scale, position.y / scale);
    anyhow::ensure!(
        point.0.is_finite() && point.1.is_finite(),
        "Invalid window scale"
    );
    Ok(point)
}

#[cfg(any(target_os = "macos", test))]
fn action_for_tag(actions: &[Option<Action>], tag: isize) -> Option<Action> {
    usize::try_from(tag)
        .ok()
        .and_then(|tag| tag.checked_sub(1))
        .and_then(|index| actions.get(index))
        .copied()
        .flatten()
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use std::{cell::Cell, rc::Rc, sync::atomic::Ordering};

    use anyhow::{Context as _, Result};
    use objc2::{
        ClassType, DeclaredClass, declare_class, msg_send_id, mutability, rc::Retained, sel,
    };
    use objc2_app_kit::{
        NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventTrackingRunLoopMode,
        NSEventType, NSMenu, NSMenuItem, NSView, NSWindow,
    };
    use objc2_foundation::{
        MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRunLoop, NSRunLoopCommonModes,
        NSString, NSTimer, ns_string,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub(super) fn copy_text(text: &str) -> Result<()> {
        MainThreadMarker::new().context("Clipboard actions require the main thread")?;
        let text = NSString::from_str(text);
        // SAFETY: AppKit pasteboard access runs on the main thread. Both objects
        // remain retained throughout the synchronous plain-text write.
        unsafe {
            let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
            pasteboard.clearContents();
            anyhow::ensure!(
                pasteboard.setString_forType(&text, objc2_app_kit::NSPasteboardTypeString),
                "Could not write session ID to clipboard"
            );
        }
        Ok(())
    }

    fn drag_window(
        window: &winit::window::Window,
        _mtm: MainThreadMarker,
    ) -> Result<Retained<NSWindow>> {
        let handle = window.window_handle()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            anyhow::bail!("Native drag requires an AppKit window");
        };
        // SAFETY: Winit's borrowed window keeps its NSView alive; main thread only.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        view.window().context("Native drag view has no window")
    }

    pub(super) fn capture_drag(window: &winit::window::Window) -> Result<DragStart> {
        let mtm =
            MainThreadMarker::new().context("Native drag capture requires the main thread")?;
        let native_window = drag_window(window, mtm)?;
        let event = NSApplication::sharedApplication(mtm)
            .currentEvent()
            .context("No current native mouse-down event")?;
        // SAFETY: Retained AppKit objects, queried only on the main thread.
        unsafe {
            validate_drag_origin(
                event.r#type() == NSEventType::LeftMouseDown,
                event.windowNumber(),
                native_window.windowNumber(),
            )?;
        }
        Ok(DragStart {
            event,
            native_window,
            window_id: window.id(),
            _main_thread: std::marker::PhantomData,
        })
    }

    pub(super) fn start_drag(start: &DragStart, window: &winit::window::Window) -> Result<()> {
        let mtm = MainThreadMarker::new().context("Native drag requires the main thread")?;
        anyhow::ensure!(
            start.window_id == window.id(),
            "Drag window identity changed"
        );
        let native_window = drag_window(window, mtm)?;
        anyhow::ensure!(
            std::ptr::eq(&*start.native_window, &*native_window),
            "Native drag window identity changed"
        );
        // SAFETY: The original event and its native window remain retained.
        unsafe {
            validate_drag_origin(
                start.event.r#type() == NSEventType::LeftMouseDown,
                start.event.windowNumber(),
                native_window.windowNumber(),
            )?;
        }
        anyhow::ensure!(left_button_down(), "Left mouse button is no longer pressed");
        native_window.performWindowDragWithEvent(&start.event);
        Ok(())
    }

    struct TargetState {
        actions: Vec<Option<Action>>,
        selected: Rc<Cell<Option<Action>>>,
        cancelled: Arc<AtomicBool>,
        root: Retained<NSMenu>,
    }

    declare_class!(
        struct MenuTarget;

        // SAFETY: NSObject has no subclass requirements. All callbacks run on
        // the main run loop, and the class has no custom Drop implementation.
        unsafe impl ClassType for MenuTarget {
            type Super = NSObject;
            type Mutability = mutability::MainThreadOnly;
            const NAME: &'static str = "NumenContextMenuTarget";
        }
        impl DeclaredClass for MenuTarget {
            type Ivars = TargetState;
        }
        unsafe impl NSObjectProtocol for MenuTarget {}

        unsafe impl MenuTarget {
            #[method(selectItem:)]
            fn select_item(&self, item: Option<&NSMenuItem>) {
                // SAFETY: This selector is assigned only to our live menu items.
                // A nil sender or invalid tag clears, rather than reuses, selection.
                let action = item.and_then(|item| {
                    action_for_tag(&self.ivars().actions, unsafe { item.tag() })
                });
                self.ivars().selected.set(action);
            }

            #[method(pollCancellation:)]
            fn poll_cancellation(&self, timer: &NSTimer) {
                if self.ivars().cancelled.load(Ordering::Acquire) {
                    let root = self.ivars().root.clone();
                    // SAFETY: The root is retained through synchronous tracking
                    // and this timer executes only on the main run loop. Stop
                    // polling before cancellation can enter nested AppKit work.
                    unsafe {
                        timer.invalidate();
                        root.cancelTrackingWithoutAnimation();
                    }
                }
            }
        }
    );

    // Invalidation removes the run loop's ownership and releases its target.
    struct TrackingTimer(Retained<NSTimer>);
    impl Drop for TrackingTimer {
        fn drop(&mut self) {
            // SAFETY: Constructed and destroyed only on the main thread.
            unsafe { self.0.invalidate() };
        }
    }

    fn build(
        catalog: &[Entry],
        mtm: MainThreadMarker,
        actions: &mut Vec<Option<Action>>,
        items: &mut Vec<Retained<NSMenuItem>>,
    ) -> Retained<NSMenu> {
        // SAFETY: All AppKit objects are created/used on the main thread; the
        // menu owns its items/submenus, and targets are assigned before popup.
        unsafe {
            let menu = NSMenu::initWithTitle(mtm.alloc(), ns_string!(""));
            menu.setAutoenablesItems(false);
            for entry in catalog {
                let (label, action, enabled, checked, tooltip, children) = match entry {
                    Entry::Separator => {
                        menu.addItem(&NSMenuItem::separatorItem(mtm));
                        continue;
                    }
                    Entry::Item {
                        label,
                        action,
                        enabled,
                        checked,
                        tooltip,
                    } => (label, *action, *enabled, *checked, tooltip.as_deref(), None),
                    Entry::Submenu { label, entries } => {
                        (label, None, true, false, None, Some(entries))
                    }
                };
                let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    mtm.alloc(),
                    &NSString::from_str(label),
                    action.filter(|_| enabled).map(|_| sel!(selectItem:)),
                    ns_string!(""),
                );
                item.setEnabled(enabled);
                if let Some(tooltip) = tooltip {
                    item.setToolTip(Some(&NSString::from_str(tooltip)));
                }
                item.setState(if checked {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                actions.push(action.filter(|_| enabled));
                item.setTag(actions.len() as isize);
                if let Some(children) = children {
                    item.setSubmenu(Some(&build(children, mtm, actions, items)));
                }
                menu.addItem(&item);
                items.push(item);
            }
            menu
        }
    }

    pub(super) fn show(
        window: &winit::window::Window,
        position: winit::dpi::PhysicalPosition<f64>,
        context: Context,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<Action>> {
        let mtm = MainThreadMarker::new().context("Context menus require the main thread")?;
        let (x, y) = logical_position(position, window.inner_size(), window.scale_factor())?;
        if cancelled.load(Ordering::Acquire) {
            return Ok(None);
        }
        let handle = window.window_handle()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            anyhow::bail!("Context menu requires an AppKit window");
        };
        // SAFETY: Winit supplies an NSView pointer. The caller's Window borrow
        // keeps it alive through tracking; no ownership is taken or transferred.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        let mut actions = Vec::new();
        let mut items = Vec::new();
        let menu = build(&entries(context), mtm, &mut actions, &mut items);
        let selected = Rc::new(Cell::new(None));
        let target = mtm.alloc::<MenuTarget>().set_ivars(TargetState {
            actions,
            selected: selected.clone(),
            cancelled: cancelled.clone(),
            root: menu.clone(),
        });
        // SAFETY: NSObject init, target selectors, and sender signatures match
        // the declarations above. Local ownership keeps the non-retained item
        // target alive until tracking ends. No callback borrows application state.
        unsafe {
            let target: Retained<MenuTarget> = msg_send_id![super(target), init];
            for item in &items {
                item.setTarget(Some(&target));
            }
            let timer = TrackingTimer(
                NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                    0.025,
                    &target,
                    sel!(pollCancellation:),
                    None,
                    true,
                ),
            );
            let run_loop = NSRunLoop::mainRunLoop();
            run_loop.addTimer_forMode(&timer.0, NSEventTrackingRunLoopMode);
            run_loop.addTimer_forMode(&timer.0, NSRunLoopCommonModes);
            // Winit's view is flipped (top-left origin): do not flip Y again.
            if !cancelled.load(Ordering::Acquire) {
                menu.popUpMenuPositioningItem_atLocation_inView(
                    None,
                    NSPoint::new(x, y),
                    Some(view),
                );
            }
            drop(timer);
            for item in &items {
                item.setTarget(None);
            }
        }
        Ok(if cancelled.load(Ordering::Acquire) {
            None
        } else {
            selected.get()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_origin_requires_original_down_and_matching_live_window() {
        assert!(validate_drag_origin(true, 42, 42).is_ok());
        for (down, event_window, window) in [
            (false, 42, 42), // Dragged, mouse-up, and other event types.
            (true, 41, 42),
            (true, 0, 42),
            (true, 0, 0),
            (true, -1, -1),
        ] {
            assert!(validate_drag_origin(down, event_window, window).is_err());
        }
    }

    fn context(settings: Option<FamiliarSettings>) -> Context {
        Context {
            paused: false,
            bubbles: true,
            copy_session_id: false,
            open_chat: false,
            navigation: true,
            settings,
            preferences: false,
            attachments: None,
            open_familiar: None,
        }
    }

    fn chat(index: usize) -> ChatChoice {
        ChatChoice {
            target: FocusTarget {
                source: "same-source".into(),
                instance: format!("instance-{index}"),
                activity: "same-activity".into(),
            },
            incarnation: index as u64 + 1,
            summary: Some("Duplicate title".into()),
        }
    }

    #[test]
    fn chat_pages_reach_all_512_choices_with_bounded_unique_rows_and_exact_details() {
        let choices: Vec<_> = (0..512).map(chat).collect();
        let catalog = attachment_entries(AttachmentContext {
            current: Some(choices[511].target.clone()),
            available: true,
            choices: choices.clone(),
        });
        let Entry::Submenu {
            label,
            entries: pages,
        } = catalog
        else {
            panic!()
        };
        assert_eq!(label, "Change attachment");
        assert_eq!(pages.len(), 32);
        let mut labels = std::collections::BTreeSet::new();
        let mut indices = Vec::new();
        for page in pages {
            let Entry::Submenu { entries: rows, .. } = page else {
                panic!()
            };
            assert_eq!(rows.len(), 16);
            for row in rows {
                let Entry::Item {
                    label,
                    action: Some(Action::Attach(index)),
                    enabled,
                    checked,
                    tooltip,
                } = row
                else {
                    panic!()
                };
                assert!(enabled);
                assert_eq!(checked, index == 511);
                assert!(label.chars().count() < 100);
                assert!(labels.insert(label));
                assert_eq!(tooltip, Some(target_details(&choices[index].target)));
                assert!(tooltip.unwrap().contains(&choices[index].target.instance));
                indices.push(index);
            }
        }
        assert_eq!(indices, (0..512).collect::<Vec<_>>());
    }

    #[test]
    fn attachment_gates_empty_current_unavailable_and_current_reselect() {
        for available in [false, true] {
            let catalog = attachment_entries(AttachmentContext {
                current: Some(chat(0).target),
                available,
                choices: Vec::new(),
            });
            let Entry::Submenu { entries: rows, .. } = catalog else {
                panic!()
            };
            assert!(rows.iter().all(|row| matches!(
                row,
                Entry::Item {
                    enabled: false,
                    action: None,
                    ..
                }
            )));
            assert!(rows.iter().any(|row| matches!(row, Entry::Item { label, .. } if label == if available { "No compatible live chats" } else { "Chat list updating—reopen menu" })));
        }
        for count in [1, 16, 17] {
            let choices: Vec<_> = (0..count).map(chat).collect();
            let catalog = attachment_entries(AttachmentContext {
                current: Some(choices[0].target.clone()),
                available: true,
                choices,
            });
            let Entry::Submenu { entries: rows, .. } = catalog else {
                panic!()
            };
            if count <= 16 {
                assert_eq!(rows.len(), count);
                assert!(matches!(
                    rows[0],
                    Entry::Item {
                        enabled: true,
                        checked: true,
                        action: Some(Action::Attach(0)),
                        ..
                    }
                ));
            } else {
                assert_eq!(rows.len(), 2);
            }
        }
        let mut context = context(None);
        context.attachments = Some(AttachmentContext {
            current: None,
            available: true,
            choices: vec![chat(0)],
        });
        let catalog = entries(context);
        assert!(catalog.iter().any(
            |row| matches!(row, Entry::Submenu { label, .. } if label == "Attach to app / chat")
        ));
        assert!(!catalog.iter().any(|row| matches!(
            row,
            Entry::Item {
                action: Some(Action::ForgetPreferences),
                ..
            }
        )));
        assert!(!entries(self::context(None)).iter().any(
            |row| matches!(row, Entry::Submenu { label, .. } if label == "Attach to app / chat")
        ));
    }

    #[test]
    fn chat_labels_sanitize_controls_and_bidi_without_changing_payload() {
        let hostile = "x\n\r\t\u{061c}\u{200e}\u{200f}\u{202a}\u{202e}\u{2066}\u{2069}";
        let mut choice = chat(0);
        choice.summary = Some(format!("{hostile}{}", "long".repeat(100)));
        choice.target.instance = hostile.into();
        let original = choice.clone();
        let catalog = attachment_entries(AttachmentContext {
            current: None,
            available: true,
            choices: vec![choice.clone()],
        });
        let Entry::Submenu { entries, .. } = catalog else {
            panic!()
        };
        let Entry::Item {
            label,
            tooltip: Some(tooltip),
            ..
        } = &entries[0]
        else {
            panic!()
        };
        assert!(!label.chars().any(unsafe_menu_char));
        assert!(label.chars().count() < 100);
        assert!(tooltip.contains("\\u{61c}"));
        assert!(tooltip.contains("\\n\\r\\t"));
        assert_eq!(choice, original);
        assert_eq!(
            action_for_tag(&[Some(Action::Attach(511))], 1),
            Some(Action::Attach(511))
        );
        assert_eq!(action_for_tag(&[Some(Action::Attach(511))], 2), None);
    }

    #[test]
    fn forget_preferences_is_only_offered_for_writable_binding() {
        for writable in [false, true] {
            let mut context = context(None);
            context.preferences = writable;
            let catalog = entries(context);
            let forget: Vec<_> = catalog
                .iter()
                .filter(|entry| {
                    matches!(
                        entry,
                        Entry::Item {
                            action: Some(Action::ForgetPreferences),
                            ..
                        }
                    )
                })
                .collect();
            assert_eq!(forget.len(), usize::from(writable));
            if writable {
                assert!(matches!(forget[0], Entry::Item { label, enabled: true, .. }
                    if label == "Forget saved preferences"));
            }
        }
        assert!(!Action::ForgetPreferences.is_service());
    }

    #[test]
    fn open_familiar_is_optional_capacity_gated_and_separate_from_quit() {
        for available in [None, Some(false), Some(true)] {
            let mut context = context(None);
            context.open_familiar = available;
            let catalog = entries(context);
            let open = catalog.iter().position(|entry| {
                matches!(
                    entry,
                    Entry::Item {
                        action: Some(Action::OpenFamiliar),
                        ..
                    }
                )
            });
            match available {
                None => assert!(open.is_none()),
                Some(enabled) => {
                    let index = open.unwrap();
                    assert!(matches!(&catalog[index], Entry::Item {
                        label, enabled: actual, checked: false, ..
                    } if label == "Open another familiar" && *actual == enabled));
                    assert_eq!(catalog[index + 1], Entry::Separator);
                    // Native rendering must not assign a selector to the full-capacity row.
                    assert_eq!(
                        action_for_tag(&[Some(Action::OpenFamiliar).filter(|_| enabled)], 1),
                        enabled.then_some(Action::OpenFamiliar)
                    );
                }
            }
            assert!(matches!(
                catalog.last(),
                Some(Entry::Item {
                    action: Some(Action::QuitNumen),
                    enabled: true,
                    ..
                })
            ));
        }
        assert!(Action::OpenFamiliar.is_service());
        assert_ne!(Action::OpenFamiliar, Action::QuitNumen);
    }

    #[test]
    fn catalog_capabilities_pause_and_distinct_scopes() {
        let catalog = entries(Context {
            paused: true,
            bubbles: false,
            copy_session_id: false,
            open_chat: false,
            navigation: false,
            settings: None,
            preferences: false,
            attachments: None,
            open_familiar: None,
        });
        assert!(matches!(
            &catalog[0],
            Entry::Item {
                action: Some(Action::RevealStatus),
                enabled: false,
                ..
            }
        ));
        assert!(matches!(
            &catalog[3],
            Entry::Item {
                action: Some(Action::GoToMouse),
                enabled: false,
                ..
            }
        ));
        assert!(
            matches!(&catalog[4], Entry::Item { label, action: Some(Action::SetPaused(false)), checked: true, .. } if label == "Resume")
        );
        assert_eq!(catalog[5], Entry::Separator);
        assert!(
            matches!(&catalog[6], Entry::Item { label, action: Some(Action::CloseFamiliar), .. } if label == "Close familiar")
        );
        assert_eq!(catalog[7], Entry::Separator);
        assert!(
            matches!(&catalog[8], Entry::Item { label, action: Some(Action::QuitNumen), .. } if label == "Quit Numen (all familiars)")
        );
        assert!(Action::QuitNumen.is_service());
        assert!(Action::OpenFamiliar.is_service());
        for action in [
            Action::CloseFamiliar,
            Action::RevealStatus,
            Action::CopySessionId,
            Action::OpenChat,
            Action::GoToMouse,
            Action::SetPaused(true),
            Action::Settings(SettingsPatch {
                font_size: Some(12.0),
                ..Default::default()
            }),
        ] {
            assert!(!action.is_service());
        }
        assert!(
            matches!(&entries(context(None))[4], Entry::Item { label, action: Some(Action::SetPaused(true)), checked: false, .. } if label == "Pause")
        );
    }

    #[test]
    fn copy_session_id_is_attachment_gated_independently_of_bubble_visibility() {
        for attached in [false, true] {
            let mut context = context(None);
            context.bubbles = false;
            context.copy_session_id = attached;
            let catalog = entries(context);
            assert!(matches!(&catalog[1], Entry::Item {
                label, action: Some(Action::CopySessionId), enabled, checked: false, ..
            } if label == "Copy session ID" && *enabled == attached));
            assert_eq!(
                action_for_tag(&[Some(Action::CopySessionId).filter(|_| attached)], 1),
                attached.then_some(Action::CopySessionId)
            );
        }
    }

    #[test]
    fn open_chat_is_live_handoff_gated_not_bubble_gated() {
        for available in [false, true] {
            let mut context = context(None);
            context.bubbles = false;
            context.open_chat = available;
            let catalog = entries(context);
            assert!(matches!(&catalog[2], Entry::Item {
                label, action: Some(Action::OpenChat), enabled, checked: false, ..
            } if label == "Open chat" && *enabled == available));
            assert_eq!(
                action_for_tag(&[Some(Action::OpenChat).filter(|_| available)], 1),
                available.then_some(Action::OpenChat)
            );
        }
    }

    #[test]
    fn presets_are_exact_checked_and_protocol_valid() {
        let catalog = entries(context(Some(FamiliarSettings::default())));
        let Entry::Submenu {
            entries: fields, ..
        } = &catalog[5]
        else {
            panic!("settings missing")
        };
        assert_eq!(fields.len(), 3);
        for (index, field) in fields.iter().enumerate() {
            let Entry::Submenu {
                entries: values, ..
            } = field
            else {
                panic!("field missing")
            };
            assert_eq!(values.len(), 4);
            for (preset, entry) in values.iter().enumerate() {
                let Entry::Item {
                    action: Some(Action::Settings(patch)),
                    checked,
                    enabled,
                    ..
                } = entry
                else {
                    panic!("preset missing")
                };
                assert!(*enabled);
                assert_eq!(*checked, preset == 1);
                patch.validate().unwrap();
                let json = serde_json::to_value(patch).unwrap();
                assert_eq!(json.as_object().unwrap().len(), 1);
                let (key, _, presets) = crate::menu::SETTINGS[index];
                assert_eq!(json[key], presets[preset].1);
                let request = serde_json::json!({"version":1,"type":"familiar.settings","ids":["test"],"patch":patch});
                crate::protocol::Request::parse(&serde_json::to_vec(&request).unwrap()).unwrap();
            }
        }
    }

    #[test]
    fn custom_values_never_check_or_invent_a_default() {
        let catalog = entries(context(Some(FamiliarSettings {
            sprite_scale: 0.7,
            font_size: 13.0,
            bubble_duration: 7.0,
        })));
        let Entry::Submenu {
            entries: fields, ..
        } = &catalog[5]
        else {
            panic!("settings missing")
        };
        for (field, value) in fields.iter().zip(["0.7", "13", "7"]) {
            let Entry::Submenu { entries, .. } = field else {
                panic!("field missing")
            };
            assert_eq!(entries.len(), 5);
            assert!(
                entries[..4]
                    .iter()
                    .all(|entry| matches!(entry, Entry::Item { checked: false, .. }))
            );
            assert!(
                matches!(&entries[4], Entry::Item { label, action: None, checked: true, enabled: false, .. } if label == &format!("Custom ({value})"))
            );
        }
        for value in [f64::NAN, f64::INFINITY, -1.0, 99.0] {
            assert!(
                entries(context(Some(FamiliarSettings {
                    sprite_scale: value,
                    ..Default::default()
                })))
                .iter()
                .all(|entry| !matches!(entry, Entry::Submenu { .. }))
            );
        }
    }

    #[test]
    fn tags_fail_closed_and_disabled_slots_are_not_actions() {
        let actions = [Some(Action::CloseFamiliar), None, Some(Action::QuitNumen)];
        assert_eq!(action_for_tag(&actions, 1), Some(Action::CloseFamiliar));
        assert_eq!(action_for_tag(&actions, 3), Some(Action::QuitNumen));
        assert_eq!(action_for_tag(&[], 1), None);
        for tag in [isize::MIN, -1, 0, 2, 4, isize::MAX] {
            assert_eq!(action_for_tag(&actions, tag), None);
        }
    }

    #[test]
    fn position_is_top_left_scaled_finite_and_bounded() {
        use winit::dpi::{PhysicalPosition, PhysicalSize};
        let size = PhysicalSize::new(200, 100);
        assert_eq!(
            logical_position(PhysicalPosition::new(40.0, 30.0), size, 2.0).unwrap(),
            (20.0, 15.0)
        );
        for (x, y, scale) in [
            (f64::NAN, 0.0, 1.0),
            (0.0, f64::INFINITY, 1.0),
            (-1.0, 0.0, 1.0),
            (201.0, 0.0, 1.0),
            (0.0, 101.0, 1.0),
            (0.0, 0.0, 0.0),
            (0.0, 0.0, -1.0),
            (0.0, 0.0, f64::NAN),
            (0.0, 0.0, f64::INFINITY),
            (1.0, 1.0, f64::from_bits(1)),
        ] {
            assert!(logical_position(PhysicalPosition::new(x, y), size, scale).is_err());
        }
    }
}
