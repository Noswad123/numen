use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pixels::wgpu::{BlendState, Color, CompositeAlphaMode};
use pixels::{Pixels, PixelsBuilder, ScalingMode, SurfaceTexture};
use tracing::{debug, info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId, WindowLevel};

use crate::RunOptions;
use crate::activity::{AttachmentBinding, FamiliarPresentation, FocusTarget, Presentation};
use crate::activity::{PreferenceBinding, PreferencePresentation};
use crate::animation::{Animation, Player};
use crate::assets::{DEFAULT_CHARACTER_ID, DEFAULT_CHARACTER_NAME, SpriteAtlas};
use crate::bubble;
use crate::context_menu::{self, Action, AttachmentContext, ChatChoice};
use crate::diagnostics::{RuntimeDiagnostics, ShortcutReason, ShortcutState, ShortcutStatus};
use crate::display::{LiveDisplay, Mailbox};
use crate::hotkey::{GlobalHotkey, Shortcut};
use crate::ipc::{FamiliarControl, FamiliarTargetSnapshot, RuntimeBinding, Server};
#[cfg(test)]
use crate::motion::{FRAME_INTERVAL, SpriteMask};
use crate::motion::{
    Facing, MotionClock, NativeMotion, Navigator, Point, PursuitStep, RasterMask, SpriteGeometry,
};
use crate::pointer::{self, PointerReader};
use crate::preferences::{self, Placement};
use crate::protocol::FamiliarSettings;
use crate::typography::Typeface;
use crate::visibility::BubbleVisibility;

const DEMO_INTERVAL: Duration = Duration::from_secs(3);

#[path = "corner.rs"]
mod corner;

#[derive(Default)]
struct ClickGesture {
    cursor: Option<PhysicalPosition<f64>>,
    pressed: Option<(PhysicalPosition<f64>, f64)>,
    held: bool,
    native_drag_active: bool,
    click_eligible: bool,
}

impl ClickGesture {
    fn press(&mut self, scale: f64) {
        self.held = true;
        self.native_drag_active = false;
        self.pressed = self
            .cursor
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .filter(|_| scale.is_finite() && scale > 0.0)
            .map(|p| (p, scale));
        self.click_eligible = self.pressed.is_some();
    }

    fn moved(&mut self, position: PhysicalPosition<f64>, button_down: bool) -> bool {
        if !position.x.is_finite() || !position.y.is_finite() {
            self.cancel_click();
            return false;
        }
        self.cursor = Some(position);
        if let Some((start, scale)) = self.pressed
            && (position.x - start.x).hypot(position.y - start.y) / scale >= 5.0
        {
            self.pressed = None;
            self.click_eligible = false;
            return self.held && button_down && !self.native_drag_active;
        }
        false
    }

    fn release(&mut self) -> Option<PhysicalPosition<f64>> {
        let position = (self.held && self.click_eligible && !self.native_drag_active)
            .then_some(self.cursor)
            .flatten();
        self.finish();
        position
    }

    fn cancel_click(&mut self) {
        self.click_eligible = false;
        self.cursor = None;
    }

    fn poll_release(&mut self, button_down: bool) -> bool {
        // Physical mouse-up can precede delivery of the queued click release.
        if self.held && !button_down && (self.native_drag_active || !self.click_eligible) {
            self.finish();
            return true;
        }
        false
    }

    fn finish(&mut self) {
        self.pressed = None;
        self.held = false;
        self.native_drag_active = false;
        self.click_eligible = false;
    }

    fn clear(&mut self) {
        self.finish();
        self.cursor = None;
    }
}

#[derive(Default, Clone)]
struct PopupLifecycle {
    cancelled: Arc<AtomicBool>,
    target: Arc<Mutex<Option<String>>>,
}

impl PopupLifecycle {
    fn post(
        &self,
        snapshot: Vec<FamiliarPresentation>,
        mailbox: &Mailbox<Vec<FamiliarPresentation>>,
    ) -> bool {
        let target = self.target.lock().unwrap_or_else(|e| e.into_inner());
        if target
            .as_ref()
            .is_some_and(|id| !snapshot.iter().any(|v| &v.id == id))
        {
            self.cancelled.store(true, Ordering::Release);
        }
        // Serialize publication with begin so a removal cannot fall between the
        // root's latest-snapshot drain and registration of the tracked target.
        mailbox.post(snapshot)
    }

    fn begin(&self, id: &str) {
        let mut target = self.target.lock().unwrap_or_else(|e| e.into_inner());
        self.cancelled.store(false, Ordering::Release);
        *target = Some(id.to_owned());
    }

    fn end(&self) {
        *self.target.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

struct MenuRequest {
    familiar: String,
    window_id: WindowId,
    window: Arc<Window>,
    position: PhysicalPosition<f64>,
}

struct MenuAttachment {
    binding: RuntimeBinding,
    choices: Vec<ChatChoice>,
}

struct MenuBindings {
    preference: Option<PreferenceBinding>,
    attachment: Option<MenuAttachment>,
    session: Option<AttachmentBinding>,
}

/// A menu must never copy a newly attached chat's ID under the old menu's action.
/// Retained unavailable pins are valid; title/status/liveness are not identity.
fn menu_session_id<'a>(
    frozen: Option<&'a AttachmentBinding>,
    applied: &AttachmentBinding,
) -> Option<&'a str> {
    frozen
        .filter(|binding| *binding == applied)
        .and_then(|binding| binding.target.as_ref())
        .map(|target| target.activity.as_str())
}

fn attachment_context(
    id: &str,
    applied: &AttachmentBinding,
    snapshot: Option<FamiliarTargetSnapshot>,
) -> (AttachmentContext, Option<MenuAttachment>) {
    let Some(snapshot) = snapshot.filter(|s| &s.binding.attachment == applied) else {
        return (
            AttachmentContext {
                current: applied.target.clone(),
                available: false,
                choices: Vec::new(),
            },
            None,
        );
    };
    let choices: Vec<_> = snapshot
        .targets
        .iter()
        .filter(|target| target.claimed_by.as_deref().is_none_or(|owner| owner == id))
        .map(|target| ChatChoice {
            target: FocusTarget::from(&target.selection),
            incarnation: target.incarnation,
            summary: target.selection.summary.clone(),
        })
        .collect();
    (
        AttachmentContext {
            current: applied.target.clone(),
            available: true,
            choices: choices.clone(),
        },
        Some(MenuAttachment {
            binding: snapshot.binding,
            choices,
        }),
    )
}

impl MenuAttachment {
    fn open_chat_index(&self, applied: &AttachmentBinding) -> Option<usize> {
        if &self.binding.attachment != applied {
            return None;
        }
        let target = applied
            .target
            .as_ref()
            .filter(|target| target.source == "djinn")?;
        self.choices
            .iter()
            .position(|choice| &choice.target == target)
    }

    fn queue(
        &self,
        id: &str,
        index: usize,
        applied: &AttachmentBinding,
        fresh: Option<&FamiliarTargetSnapshot>,
        submit: impl FnOnce(&RuntimeBinding, &ChatChoice) -> Result<()>,
    ) -> Result<()> {
        let choice = self
            .choice(index, applied)
            .context("chat attachment changed")?;
        anyhow::ensure!(
            fresh.is_none_or(|fresh| self.still_eligible(id, choice, fresh)),
            "selected chat is no longer available"
        );
        submit(&self.binding, choice)
    }

    fn choice(&self, index: usize, applied: &AttachmentBinding) -> Option<&ChatChoice> {
        (&self.binding.attachment == applied)
            .then(|| self.choices.get(index))
            .flatten()
    }

    fn still_eligible(
        &self,
        id: &str,
        choice: &ChatChoice,
        fresh: &FamiliarTargetSnapshot,
    ) -> bool {
        fresh.binding == self.binding
            && fresh.targets.iter().any(|target| {
                target.incarnation == choice.incarnation
                    && FocusTarget::from(&target.selection) == choice.target
                    && target.claimed_by.as_deref().is_none_or(|owner| owner == id)
            })
    }
}

fn unbound_attachment() -> AttachmentBinding {
    AttachmentBinding {
        target: None,
        generation: 1,
    }
}

fn familiar_settings(options: &RunOptions) -> FamiliarSettings {
    FamiliarSettings {
        sprite_scale: f64::from(options.sprite_scale),
        font_size: f64::from(options.font_size),
        bubble_duration: options.bubble_duration.as_secs_f64(),
    }
}

#[derive(Debug, Clone, Copy)]
enum AppEvent {
    ActivityChanged,
    RevealBubbles,
    GoToMouse,
    QuitService,
}

#[derive(Clone)]
struct RevealNotifier {
    pending: Arc<RevealMailbox>,
    proxy: EventLoopProxy<AppEvent>,
}

impl RevealNotifier {
    fn request(&self, targets: Option<Vec<String>>) {
        if self.pending.post(targets) && self.proxy.send_event(AppEvent::RevealBubbles).is_err() {
            self.pending.take();
        }
    }
}

#[derive(Clone)]
struct NavigationNotifier {
    pending: Arc<NavigationMailbox>,
    proxy: EventLoopProxy<AppEvent>,
}

impl NavigationNotifier {
    fn request(&self, targets: Option<Vec<String>>) {
        if self.pending.post(targets) && self.proxy.send_event(AppEvent::GoToMouse).is_err() {
            self.pending.take();
        }
    }

    fn corner(&self) {
        if self.pending.post_corner() && self.proxy.send_event(AppEvent::GoToMouse).is_err() {
            self.pending.take();
        }
    }
}

#[derive(Default)]
struct NavigationMailbox(Mutex<VecDeque<(bool, Option<Vec<String>>)>>);

impl NavigationMailbox {
    fn post(&self, targets: Option<Vec<String>>) -> bool {
        self.push(false, targets)
    }

    fn post_corner(&self) -> bool {
        self.push(true, None)
    }

    fn push(&self, corner: bool, targets: Option<Vec<String>>) -> bool {
        let mut pending = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let wake = pending.is_empty();
        if pending.len() == 32 {
            pending.pop_front();
        }
        let targets = targets.map(|mut ids| {
            ids.truncate(16);
            ids
        });
        pending.push_back((corner, targets));
        wake
    }

    fn take(&self) -> VecDeque<(bool, Option<Vec<String>>)> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

fn register_shortcuts<T>(
    options: &RunOptions,
    pointer_available: bool,
    live: bool,
    platform_supported: bool,
    reporter: Option<&RuntimeDiagnostics>,
    mut register: impl FnMut(Shortcut) -> Result<T>,
) -> [Option<T>; 3] {
    let shortcuts = [Shortcut::Reveal, Shortcut::GoToMouse, Shortcut::GoToCorner];
    let statuses = shortcuts.map(|shortcut| {
        let (state, reason) = if options.no_hotkey {
            (ShortcutState::Disabled, ShortcutReason::NoHotkey)
        } else if shortcut == Shortcut::Reveal && options.no_bubble {
            (ShortcutState::Disabled, ShortcutReason::NoBubble)
        } else if !platform_supported {
            (
                ShortcutState::Unsupported,
                ShortcutReason::UnsupportedPlatform,
            )
        } else if shortcut == Shortcut::GoToCorner && (options.manual || options.demo || !live) {
            (ShortcutState::Unsupported, ShortcutReason::UnsupportedMode)
        } else if shortcut == Shortcut::GoToMouse && !pointer_available {
            if options.manual || options.demo || !live {
                (ShortcutState::Unsupported, ShortcutReason::UnsupportedMode)
            } else {
                (
                    ShortcutState::Unavailable,
                    ShortcutReason::PointerInitializationFailed,
                )
            }
        } else {
            (
                ShortcutState::Uninitialized,
                ShortcutReason::RegistrationPending,
            )
        };
        let status = ShortcutStatus {
            state,
            reason: Some(reason),
        };
        if let Some(reporter) = reporter {
            reporter.set_hotkey(shortcut, status);
        }
        status
    });
    // Publish all gates before entering OS registration, without retaining a lock.
    shortcuts.map(|shortcut| {
        let index = match shortcut {
            Shortcut::Reveal => 0,
            Shortcut::GoToMouse => 1,
            Shortcut::GoToCorner => 2,
        };
        if statuses[index].reason != Some(ShortcutReason::RegistrationPending) {
            return None;
        }
        let (guard, status) = match register(shortcut) {
            Ok(guard) => {
                info!(?shortcut, "Global shortcut registered");
                (
                    Some(guard),
                    ShortcutStatus {
                        state: ShortcutState::Registered,
                        reason: None,
                    },
                )
            }
            Err(error) => {
                warn!(?shortcut, %error, "Global shortcut unavailable; CLI and local shortcut remain available");
                (
                    None,
                    ShortcutStatus {
                        state: ShortcutState::Unavailable,
                        reason: Some(ShortcutReason::RegistrationFailed),
                    },
                )
            }
        };
        if let Some(reporter) = reporter {
            reporter.set_hotkey(shortcut, status);
        }
        guard
    })
}

#[derive(Default)]
struct RevealMailbox(Mutex<VecDeque<Option<Vec<String>>>>);

impl RevealMailbox {
    fn post(&self, targets: Option<Vec<String>>) -> bool {
        let mut pending = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let wake = pending.is_empty();
        if pending.iter().any(Option::is_none) {
            return false;
        }
        if targets.is_none() {
            pending.clear();
        } else if pending.len() == 32 {
            pending.pop_front();
            warn!("Reveal queue full; discarding oldest request");
        }
        let targets = targets.map(|mut ids| {
            if ids.len() > 16 {
                warn!("Reveal target limit exceeded; discarding excess targets");
                ids.truncate(16);
            }
            ids
        });
        pending.push_back(targets);
        wake
    }

    fn take(&self) -> VecDeque<Option<Vec<String>>> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

fn reveal_targets(
    requests: VecDeque<Option<Vec<String>>>,
    ids: impl Iterator<Item = String>,
) -> BTreeSet<String> {
    if requests.iter().any(Option::is_none) {
        ids.collect()
    } else {
        requests.into_iter().flatten().flatten().collect()
    }
}

fn resolve_preferences_path(
    options: &RunOptions,
    resolve: impl FnOnce() -> Result<PathBuf>,
) -> (Option<PathBuf>, Option<preferences::Reason>) {
    if options.no_preferences {
        (None, None)
    } else if let Some(path) = &options.preferences_file {
        (Some(path.clone()), None)
    } else {
        match resolve() {
            Ok(path) => (Some(path), None),
            Err(error) => {
                warn!(%error, "Preferences path unavailable");
                (None, Some(preferences::error_reason(&error)))
            }
        }
    }
}

pub fn run(atlas: SpriteAtlas, options: RunOptions, socket: Option<&Path>) -> Result<()> {
    let socket = socket.filter(|_| !options.manual && !options.demo);
    let mut builder = EventLoop::<AppEvent>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_activate_ignoring_other_apps(false);
    }
    let event_loop = builder
        .build()
        .context("cannot connect to desktop event loop")?;
    let mut app = App::new(atlas, options, socket.is_some());
    if app.live && pointer::supported() {
        match PointerReader::new() {
            Ok(reader) => app.pointer = Some(reader),
            Err(error) => warn!(%error, "Live pointer unavailable; mouse navigation disabled"),
        }
    }
    let mailbox = app.mailbox.clone();
    let proxy = event_loop.create_proxy();
    let reveal = RevealNotifier {
        pending: app.reveal_pending.clone(),
        proxy: proxy.clone(),
    };
    let ipc_reveal = reveal.clone();
    let navigation = NavigationNotifier {
        pending: app.navigation_pending.clone(),
        proxy: proxy.clone(),
    };
    let ipc_navigation = navigation.clone();
    let quit_proxy = proxy.clone();
    let quit_pending = app.quit_pending.clone();
    let popup_views = app.popup.clone();
    let popup_quit = app.popup.clone();
    let server = socket
        .map(|path| {
            let on_views = move |view| {
                if popup_views.post(view, &mailbox) {
                    let _ = proxy.send_event(AppEvent::ActivityChanged);
                }
            };
            let on_reveal = move |ids| ipc_reveal.request(ids);
            let on_go_to_mouse = if app.pointer.is_some() {
                Some(Box::new(move |ids| ipc_navigation.request(Some(ids)))
                    as Box<dyn Fn(Vec<String>) + Send>)
            } else {
                None
            };
            let (preferences_path, preferences_reason) =
                resolve_preferences_path(&app.options, || {
                    preferences::default_path(
                        std::env::var_os("XDG_CONFIG_HOME"),
                        std::env::var_os("HOME"),
                    )
                });
            let server = Server::start_graphical_with_preferences(
                path,
                on_views,
                on_reveal,
                on_go_to_mouse,
                move || {
                    popup_quit.cancelled.store(true, Ordering::Release);
                    if !quit_pending.swap(true, Ordering::AcqRel) {
                        let _ = quit_proxy.send_event(AppEvent::QuitService);
                    }
                },
                !app.options.empty,
                familiar_settings(&app.options),
                preferences_path,
            )?;
            if let Some(reason) = preferences_reason {
                server
                    .diagnostics()
                    .set_preferences_status(preferences::Status::unavailable(reason));
            }
            info!(socket = %path.display(), "Local activity listener ready");
            Ok::<_, anyhow::Error>(server)
        })
        .transpose()?;
    app.control = server.as_ref().map(Server::familiar_control);
    let diagnostics = server.as_ref().map(Server::diagnostics);
    let hotkeys = register_shortcuts(
        &app.options,
        app.pointer.is_some(),
        app.live,
        cfg!(target_os = "macos"),
        diagnostics.as_ref(),
        |shortcut| {
            let reveal = reveal.clone();
            let navigation = navigation.clone();
            GlobalHotkey::register(shortcut, move || match shortcut {
                Shortcut::Reveal => reveal.request(None),
                Shortcut::GoToMouse => navigation.request(None),
                Shortcut::GoToCorner => navigation.corner(),
            })
        },
    );
    app.global_hotkey_registered = hotkeys[0].is_some();
    app.global_navigation_registered = hotkeys[1].is_some();
    app.global_corner_registered = hotkeys[2].is_some();
    let result = event_loop.run_app(&mut app);
    drop(hotkeys);
    drop(server);
    result?;
    info!("Numen event loop closed");
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

enum Navigation {
    Seeking(Navigator),
    Corner {
        navigator: Navigator,
        display: corner::DisplayArea,
        target: Point,
        size: Point,
    },
    Celebrating,
}

#[derive(Clone, Copy, PartialEq)]
struct RasterKey {
    animation: Animation,
    frame: usize,
    size: (u32, u32),
    show: bool,
    sprite_scale: u32,
}

struct CachedRaster {
    key: RasterKey,
    mask: RasterMask,
}

impl CachedRaster {
    fn new(key: RasterKey, sprite: &[u8]) -> Self {
        let projection =
            bubble::SpriteProjection::new(key.size, key.show, f32::from_bits(key.sprite_scale));
        let (x0, y0, x1, y1) = projection.bounds;
        let mask = RasterMask::new(x1 - x0, y1 - y0, projection.alpha(sprite));
        Self { key, mask }
    }
}

struct FamiliarView {
    id: String,
    // Drop the GPU surface before the window (the surface also holds its own Arc).
    pixels: Option<Pixels<'static>>,
    window: Option<Arc<Window>>,
    atlas: Arc<SpriteAtlas>,
    player: Player,
    selection_index: usize,
    options: RunOptions,
    paused: bool,
    occluded: bool,
    last_tick: Instant,
    next_demo: Instant,
    draws: u64,
    error: Option<anyhow::Error>,
    close_requested: bool,
    display: LiveDisplay,
    focus_control: Option<FamiliarControl>,
    latest_view: Presentation,
    renderer: Option<bubble::Renderer>,
    frame_size: (u32, u32),
    visibility: BubbleVisibility,
    bubble_visible: bool,
    modifiers: ModifiersState,
    hidden_position: Option<PhysicalPosition<i32>>,
    global_hotkey_registered: bool,
    global_navigation_registered: bool,
    global_corner_registered: bool,
    corner_available: bool,
    navigation_available: bool,
    navigation: Option<Navigation>,
    corner_return: Option<Animation>,
    motion_clock: MotionClock,
    native_motion: NativeMotion,
    raster: Option<CachedRaster>,
    scene_dirty: bool,
    gesture: ClickGesture,
    drag_start: Option<context_menu::DragStart>,
    menu_open: bool,
    settings: FamiliarSettings,
    deferred_settings: Option<FamiliarSettings>,
    preference: Option<PreferencePresentation>,
    runtime_binding: AttachmentBinding,
    drag_binding: Option<PreferenceBinding>,
    placement_sequence: u64,
    pending_placement: Option<PendingPlacement>,
    placement_retry: Option<Instant>,
    pending_restore: Option<PendingRestore>,
    ever_bound: bool,
    bootstrap_unbound: bool,
    first_binding_restore: bool,
}

#[derive(Clone)]
struct PendingPlacement {
    binding: PreferenceBinding,
    sequence: u64,
    placement: Placement,
}

struct PendingRestore {
    placement: Placement,
    attempts: u8,
    feedback: Option<((u32, u32), u64)>,
    deadline: Instant,
    poll_at: Instant,
}

impl PendingRestore {
    fn finished(&self, now: Instant) -> bool {
        now >= self.deadline || self.attempts >= 5
    }

    fn correction(
        &mut self,
        now: Instant,
        geometry: ((u32, u32), u64),
        origin: Option<Point>,
    ) -> Option<Point> {
        if self.finished(now) || now < self.poll_at {
            return None;
        }
        self.poll_at = now + Duration::from_millis(20);
        let origin = origin?;
        if self.feedback == Some(geometry) {
            return None;
        }
        self.feedback = Some(geometry);
        self.attempts += 1;
        Some(origin)
    }
}

impl FamiliarView {
    fn validated_forget_binding<'a>(
        &self,
        captured: Option<&'a PreferenceBinding>,
    ) -> Option<&'a PreferenceBinding> {
        captured.filter(|binding| {
            self.preference
                .as_ref()
                .is_some_and(|p| p.writable && &p.binding == *binding)
        })
    }

    fn begin_initial_placement(&mut self) -> bool {
        self.first_binding_restore = false;
        let now = Instant::now();
        self.pending_restore = self
            .preference
            .as_ref()
            .and_then(|p| p.placement)
            .filter(|p| valid_placement(*p))
            .map(|placement| PendingRestore {
                placement,
                attempts: 0,
                feedback: None,
                deadline: now + Duration::from_secs(1),
                poll_at: now,
            });
        self.pending_restore.is_some()
    }

    fn clear_placement_intent(&mut self) {
        self.drag_binding = None;
        self.pending_placement = None;
        self.placement_retry = None;
        self.pending_restore = None;
        self.first_binding_restore = false;
    }

    fn cancel_restore_for_user_intent(&mut self) {
        self.pending_restore = None;
        self.first_binding_restore = false;
        self.bootstrap_unbound = false;
    }

    fn set_preference(&mut self, preference: Option<PreferencePresentation>) {
        let first_binding = !self.ever_bound && preference.is_some();
        if self.preference.as_ref().map(|p| &p.binding) != preference.as_ref().map(|p| &p.binding) {
            self.clear_placement_intent();
        }
        if !preference.as_ref().is_some_and(|p| p.writable) {
            self.drag_binding = None;
            self.pending_placement = None;
            self.placement_retry = None;
        }
        self.preference = preference;
        if first_binding {
            self.ever_bound = true;
            self.first_binding_restore = self.bootstrap_unbound
                && self
                    .preference
                    .as_ref()
                    .is_some_and(|p| p.placement.is_some());
            self.bootstrap_unbound = false;
        }
    }

    fn capture_placement(&mut self, now: Instant) {
        let window = self.window.clone();
        let show = self.bubble_visible;
        let scale = self.options.sprite_scale;
        self.capture_placement_with(now, || {
            let window = window?;
            let backing = window.scale_factor();
            if !backing.is_finite() || backing <= 0.0 {
                return None;
            }
            let origin = window.outer_position().ok()?.to_logical::<f64>(backing);
            let size = window.inner_size();
            placement_from_geometry(
                Point {
                    x: origin.x,
                    y: origin.y,
                },
                (size.width, size.height),
                backing,
                show,
                scale,
            )
        });
    }

    fn capture_placement_with(
        &mut self,
        now: Instant,
        geometry: impl FnOnce() -> Option<Placement>,
    ) {
        if !self.gesture.held || !self.gesture.native_drag_active {
            return;
        }
        let Some(binding) = self.drag_binding.take() else {
            return;
        };
        if !self
            .preference
            .as_ref()
            .is_some_and(|p| p.writable && p.binding == binding)
        {
            return;
        }
        let Some(placement) = geometry().filter(|p| valid_placement(*p)) else {
            return;
        };
        let Some(sequence) = self.placement_sequence.checked_add(1) else {
            return;
        };
        self.placement_sequence = sequence;
        self.pending_placement = Some(PendingPlacement {
            binding,
            sequence,
            placement,
        });
        self.placement_retry = Some(now);
    }

    fn retry_placement(&mut self, now: Instant) -> Option<Instant> {
        self.pending_placement.as_ref()?;
        let control = self.focus_control.clone();
        let id = self.id.clone();
        self.retry_placement_with(now, |pending| {
            if let Some(control) = control {
                control.placement(&id, &pending.binding, pending.sequence, pending.placement)
            } else {
                Ok(())
            }
        })
    }

    fn retry_placement_with(
        &mut self,
        now: Instant,
        submit: impl FnOnce(&PendingPlacement) -> Result<()>,
    ) -> Option<Instant> {
        if self.close_requested || self.error.is_some() {
            self.clear_placement_intent();
            return None;
        }
        let pending = self.pending_placement.as_ref()?;
        if !self
            .preference
            .as_ref()
            .is_some_and(|p| p.writable && p.binding == pending.binding)
        {
            self.pending_placement = None;
            self.placement_retry = None;
            return None;
        }
        let retry = self.placement_retry.unwrap_or(now);
        if now < retry {
            return Some(retry);
        }
        // Queue acceptance is not a durability acknowledgement. Never resample
        // geometry here: navigation or layout may have moved the live window.
        if submit(pending).is_err() {
            let retry = now + Duration::from_millis(100);
            self.placement_retry = Some(retry);
            return Some(retry);
        }
        self.pending_placement = None;
        self.placement_retry = None;
        None
    }

    fn restore_placement(&mut self, now: Instant) {
        let Some(mut restore) = self.pending_restore.take() else {
            return;
        };
        if self.geometry_held()
            || self.navigation.is_some()
            || self.close_requested
            || self.error.is_some()
            || restore.finished(now)
        {
            return;
        }
        if now < restore.poll_at {
            self.pending_restore = Some(restore);
            return;
        }
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        let backing = window.scale_factor();
        let geometry = ((size.width, size.height), backing.to_bits());
        let monitors: Vec<_> = window
            .available_monitors()
            .filter_map(|monitor| {
                logical_monitor(monitor.position(), monitor.size(), monitor.scale_factor())
            })
            .collect();
        let origin = restored_origin(
            restore.placement,
            geometry.0,
            backing,
            self.bubble_visible,
            self.options.sprite_scale,
            &monitors,
        );
        if let Some(origin) = restore.correction(now, geometry, origin) {
            // Only initial creation and bounded native size/DPI feedback use this.
            // Position changes never focus, activate, or become a saved user drag.
            window.set_outer_position(LogicalPosition::new(origin.x, origin.y));
            self.hidden_position = if self.bubble_visible {
                let hidden_size = layout_size(
                    false,
                    self.options.sprite_scale,
                    self.options.scale * backing,
                );
                Some(resized_position(
                    LogicalPosition::new(origin.x, origin.y).to_physical::<i32>(backing),
                    (geometry.0, true, self.options.sprite_scale),
                    (hidden_size, false, self.options.sprite_scale),
                ))
            } else {
                None
            };
        }
        if !restore.finished(now) {
            self.pending_restore = Some(restore);
        }
    }

    fn new(id: String, atlas: Arc<SpriteAtlas>, options: RunOptions) -> Self {
        let now = Instant::now();
        let settings = familiar_settings(&options);
        let mut display = LiveDisplay::default();
        let mut visibility = BubbleVisibility::new(
            !options.no_bubble,
            options.always_bubble,
            options.bubble_duration,
        );
        visibility.observe(&Presentation::default(), now);
        let bubble_visible = visibility.visible(now);
        if options.manual || options.demo {
            display.manual(options.animation);
        } else {
            display.on_advance(options.animation);
        }
        Self {
            id,
            pixels: None,
            window: None,
            player: Player::new(options.animation),
            selection_index: Animation::ALL
                .iter()
                .position(|&a| a == options.animation)
                .unwrap_or(0),
            atlas,
            paused: options.paused,
            options,
            occluded: false,
            last_tick: now,
            next_demo: now + DEMO_INTERVAL,
            draws: 0,
            error: None,
            close_requested: false,
            display,
            focus_control: None,
            latest_view: Presentation::default(),
            renderer: None,
            frame_size: (0, 0),
            visibility,
            bubble_visible,
            modifiers: ModifiersState::empty(),
            hidden_position: None,
            global_hotkey_registered: false,
            global_navigation_registered: false,
            global_corner_registered: false,
            corner_available: false,
            navigation_available: false,
            navigation: None,
            corner_return: None,
            motion_clock: MotionClock::default(),
            native_motion: NativeMotion::default(),
            raster: None,
            scene_dirty: true,
            gesture: ClickGesture::default(),
            drag_start: None,
            menu_open: false,
            settings,
            deferred_settings: None,
            preference: None,
            runtime_binding: unbound_attachment(),
            drag_binding: None,
            placement_sequence: 0,
            pending_placement: None,
            placement_retry: None,
            pending_restore: None,
            ever_bound: false,
            bootstrap_unbound: false,
            first_binding_restore: false,
        }
    }

    fn apply_settings(&mut self, settings: FamiliarSettings) -> Result<()> {
        self.settings = settings;
        if self.geometry_held() {
            self.deferred_settings = Some(settings);
            return Ok(());
        }
        self.deferred_settings = None;
        let sprite_scale = settings.sprite_scale as f32;
        let font_size = settings.font_size as f32;
        let duration = Duration::from_secs_f64(settings.bubble_duration);
        let old_scale = self.options.sprite_scale;
        let layout_changed = old_scale != sprite_scale;
        let paint_changed = layout_changed || self.options.font_size != font_size;
        let changed = paint_changed || self.options.bubble_duration != duration;
        self.options.sprite_scale = sprite_scale;
        self.options.font_size = font_size;
        self.options.bubble_duration = duration;
        self.visibility.set_duration(duration);
        if let Some(renderer) = &mut self.renderer {
            renderer.set_sprite_scale(sprite_scale);
            renderer.set_font_size(font_size);
        }
        if paint_changed {
            self.scene_dirty = true;
            if layout_changed {
                self.raster = None;
                self.layout(self.bubble_visible, old_scale)?;
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        if changed {
            debug!(familiar = %self.id, sprite_scale, font_size,
                bubble_duration = duration.as_secs_f64(), "Familiar settings applied");
        }
        Ok(())
    }

    fn initialize(
        &mut self,
        event_loop: &ActiveEventLoop,
        typeface: Option<Typeface>,
    ) -> Result<()> {
        self.renderer = Some(
            bubble::Renderer::new(typeface, self.options.font_size)
                .with_sprite_scale(self.options.sprite_scale),
        );
        self.visibility.expire(Instant::now());
        self.bubble_visible = self.visibility.visible(Instant::now());
        let (canvas_width, canvas_height) =
            bubble::dimensions(self.bubble_visible, self.options.sprite_scale);
        let attributes = Window::default_attributes()
            .with_title(format!("Numen — {DEFAULT_CHARACTER_NAME}"))
            .with_inner_size(LogicalSize::new(
                f64::from(canvas_width) * self.options.scale,
                f64::from(canvas_height) * self.options.scale,
            ))
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(!self.options.opaque)
            .with_active(self.options.focus)
            // On macOS, set_visible(true) later would make this a key window
            // regardless of with_active(false). Create visible to honor focus.
            .with_visible(true)
            .with_window_level(if self.options.normal_window_level {
                WindowLevel::Normal
            } else {
                WindowLevel::AlwaysOnTop
            });
        #[cfg(target_os = "macos")]
        let attributes = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes
                .with_has_shadow(false)
                .with_accepts_first_mouse(true)
        };
        let window = Arc::new(event_loop.create_window(attributes)?);
        let size = window.inner_size();
        let surface = SurfaceTexture::new(size.width.max(1), size.height.max(1), window.clone());
        // Rasterize into actual display pixels, not a low-resolution canvas
        // subsequently enlarged by the GPU. Only the sprite uses nearest scaling.
        let mut pixels = PixelsBuilder::new(size.width.max(1), size.height.max(1), surface)
            .clear_color(if self.options.opaque { Color::BLACK } else { Color::TRANSPARENT })
            // Preserve straight-alpha PNG bytes in the presentation surface.
            // Nearest-neighbor scaling avoids filtering straight-alpha edges.
            .blend_state(if self.options.opaque {
                BlendState::ALPHA_BLENDING
            } else {
                BlendState::REPLACE
            })
            .alpha_mode(if self.options.opaque {
                CompositeAlphaMode::Opaque
            } else {
                CompositeAlphaMode::PostMultiplied
            })
            .build()
            .context("cannot create graphics surface; try `numen run --opaque` to diagnose alpha support")?;
        pixels.set_scaling_mode(ScalingMode::PixelPerfect);
        info!(familiar = %self.id, adapter = ?pixels.adapter().get_info(),
            framebuffer_width = size.width, framebuffer_height = size.height,
            display_scale = window.scale_factor(), "Graphics initialized");
        self.pixels = Some(pixels);
        self.frame_size = (size.width.max(1), size.height.max(1));
        self.last_tick = Instant::now();
        self.next_demo = self.last_tick + DEMO_INTERVAL;
        if self.options.focus {
            window.focus_window();
        }
        window.request_redraw();
        self.window = Some(window);
        info!(
            familiar = %self.id,
            character = DEFAULT_CHARACTER_ID,
            animation = self.player.animation().label(),
            navigation_shortcut = self.navigation_available.then_some("Option+M/go to mouse"),
            corner_shortcut = self.corner_available.then_some("Option+C/random corner"),
            "{DEFAULT_CHARACTER_NAME} ready; click/menu (macOS), drag/move, Option+I/bubbles, Tab/next conversation, 1–9/manual, L/live, Space/pause, Esc/Q/Close familiar"
        );
        Ok(())
    }

    fn fail(&mut self, _event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.cancel_restore_for_user_intent();
        self.clear_placement_intent();
        self.gesture.clear();
        self.drag_start = None;
        self.menu_open = false;
        self.cancel_navigation("view failed");
        self.error = Some(error);
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
        self.cancel_restore_for_user_intent();
        self.clear_placement_intent();
        info!(familiar = %self.id, "Close familiar requested");
        self.gesture.clear();
        self.drag_start = None;
        self.cancel_navigation("close requested");
        if self.focus_control.is_some() {
            self.close_requested = true;
        } else {
            event_loop.exit();
        }
    }

    fn select(&mut self, animation: Animation) {
        self.scene_dirty = true;
        self.player.select(animation);
        self.selection_index = Animation::ALL
            .iter()
            .position(|&a| a == animation)
            .unwrap_or(0);
        self.last_tick = Instant::now();
        self.next_demo = self.last_tick + DEMO_INTERVAL;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        info!(familiar = %self.id, animation = animation.label(), "Expression selected");
    }

    fn cycle(&mut self) {
        self.cancel_navigation("manual cycle");
        let animation = Animation::ALL[(self.selection_index + 1) % Animation::ALL.len()];
        self.display.manual(animation);
        self.select(animation);
    }

    fn keyboard(&mut self, event_loop: &ActiveEventLoop, code: KeyCode) {
        if self.local_navigation(
            code,
            self.window
                .as_ref()
                .is_some_and(|window| window.has_focus()),
        ) {
            return;
        }
        match code {
            KeyCode::KeyC if self.modifiers.control_key() => event_loop.exit(),
            KeyCode::Escape | KeyCode::KeyQ => self.close(event_loop),
            code if local_reveal_shortcut(code, self.modifiers, self.global_hotkey_registered) => {
                self.reveal_bubbles(event_loop)
            }
            KeyCode::Tab => {
                self.cancel_navigation("conversation switch");
                if let Some(control) = &self.focus_control {
                    match control.next(&self.id) {
                        Ok(()) => {
                            let animation = self.display.resume();
                            self.select(animation);
                            info!("Requested next focused conversation");
                            self.reveal_bubbles(event_loop);
                        }
                        Err(error) => warn!(%error, "Could not switch conversation"),
                    }
                }
            }
            KeyCode::KeyL if !self.options.demo && !self.options.manual => {
                self.cancel_navigation("live control requested");
                let animation = self.display.resume();
                self.select(animation);
                info!("Live activity control resumed");
                self.reveal_bubbles(event_loop);
            }
            KeyCode::Space => {
                self.set_paused(!self.paused);
            }
            _ => {
                if let Some(animation) = animation_for_key(code) {
                    self.cancel_navigation("manual expression");
                    self.display.manual(animation);
                    self.select(animation);
                    self.reveal_bubbles(event_loop);
                }
            }
        }
    }

    fn local_navigation(&mut self, code: KeyCode, focused: bool) -> bool {
        if focused
            && local_navigation_shortcut(
                code,
                self.modifiers,
                self.global_navigation_registered,
                self.navigation_available,
            )
        {
            self.start_navigation(Instant::now());
            return true;
        }
        false
    }

    fn local_corner(&self, code: KeyCode, focused: bool) -> bool {
        focused
            && self.corner_available
            && !self.global_corner_registered
            && code == KeyCode::KeyC
            && self.modifiers == ModifiersState::ALT
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        self.last_tick = Instant::now();
        self.resume_navigation(self.last_tick);
        self.next_demo = self.last_tick + DEMO_INTERVAL;
        info!(paused, "Animation pause changed");
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn menu_request(&mut self, position: PhysicalPosition<f64>) -> Option<MenuRequest> {
        self.cancel_restore_for_user_intent();
        let window = self.window.clone()?;
        Some(MenuRequest {
            familiar: self.id.clone(),
            window_id: window.id(),
            window,
            position,
        })
    }

    fn end_interaction(&mut self, now: Instant) {
        self.drag_binding = None;
        self.gesture.finish();
        self.drag_start = None;
        self.last_tick = now;
        self.resume_navigation(now);
    }

    fn release_click(&mut self, now: Instant) -> Option<PhysicalPosition<f64>> {
        self.capture_placement(now);
        let position = self.gesture.release();
        // Preserve the captured local coordinate across the root's snapshot drain.
        self.menu_open = position.is_some();
        self.end_interaction(now);
        position
    }

    fn finish_menu(&mut self, now: Instant) -> Result<()> {
        self.menu_open = false;
        self.end_interaction(now);
        self.flush_geometry(now)
    }

    fn geometry_held(&self) -> bool {
        self.gesture.held || self.menu_open
    }

    fn flush_geometry(&mut self, now: Instant) -> Result<()> {
        if self.geometry_held() {
            return Ok(());
        }
        if let Some(settings) = self.deferred_settings.take() {
            self.apply_settings(settings)?;
        }
        self.sync_visibility(now)
    }

    fn poll_interaction(&mut self, now: Instant, button_down: bool) {
        if self.gesture.held && !button_down && self.gesture.native_drag_active {
            self.capture_placement(now);
        }
        if self.gesture.poll_release(button_down) {
            self.end_interaction(now);
        }
    }

    fn commit_drag(&mut self) {
        self.cancel_restore_for_user_intent();
        self.gesture.native_drag_active = true;
        self.cancel_navigation("native drag");
        self.hidden_position = None;
    }

    fn drawable(&self) -> bool {
        !self.occluded && self.native_geometry_valid()
    }

    fn native_geometry_valid(&self) -> bool {
        self.window.as_ref().is_some_and(|window| {
            let size = window.inner_size();
            size.width > 0 && size.height > 0
        })
    }

    fn motion_eligible(&self) -> bool {
        !self.geometry_held()
            && self.seeking()
            && self.animation_eligible(self.native_geometry_valid())
    }

    fn animation_eligible(&self, native_valid: bool) -> bool {
        native_valid && !self.paused && (self.navigation.is_some() || !self.occluded)
    }

    fn start_navigation(&mut self, now: Instant) {
        if self.close_requested || self.error.is_some() {
            return;
        }
        self.cancel_restore_for_user_intent();
        self.corner_return = None;
        let base = self.display.resume();
        self.select(base);
        self.navigation = Some(Navigation::Seeking(Navigator::new(
            self.options.move_speed,
            now,
        )));
        self.hidden_position = None;
        self.motion_clock.reset(now);
        self.native_motion = NativeMotion::default();
        self.raster = None;
        self.last_tick = now;
        info!(familiar = %self.id, "Pointer pursuit started");
    }

    fn seeking(&self) -> bool {
        matches!(
            self.navigation,
            Some(Navigation::Seeking(_) | Navigation::Corner { .. })
        ) && !self.close_requested
            && self.error.is_none()
    }

    fn mouse_seeking(&self) -> bool {
        self.seeking() && matches!(self.navigation, Some(Navigation::Seeking(_)))
    }

    fn resume_navigation(&mut self, now: Instant) {
        if let Some(Navigation::Seeking(navigator) | Navigation::Corner { navigator, .. }) =
            &mut self.navigation
        {
            navigator.resume(now);
            self.motion_clock.reset(now);
            self.native_motion = NativeMotion::default();
        }
    }

    fn cancel_navigation(&mut self, reason: &str) {
        if self.navigation.take().is_some() {
            self.motion_clock.clear();
            let base = self
                .corner_return
                .take()
                .unwrap_or_else(|| self.display.resume());
            self.select(base);
            info!(familiar = %self.id, reason, "Navigation stopped");
        }
    }

    fn source_animation(&mut self, view: Presentation) {
        if let Some(animation) = self.display.update(view) {
            if self.corner_return.is_some() {
                self.corner_return = Some(animation);
            } else if self.navigation.is_none() {
                self.select(animation);
            }
        }
    }

    fn advance_animation(&mut self, elapsed: Duration) -> bool {
        if self.paused || (self.occluded && self.navigation.is_none()) {
            return false;
        }
        let changed = self.player.advance(elapsed, &self.atlas);
        self.scene_dirty |= changed;
        if matches!(self.navigation, Some(Navigation::Celebrating)) {
            if self.player.animation() == Animation::Idle {
                self.navigation = None;
                let base = self.display.resume();
                self.select(base);
                info!(familiar = %self.id, "Pointer pursuit completed");
            }
        } else if self.navigation.is_none()
            && let Some(animation) = self.display.on_advance(self.player.animation())
        {
            self.select(animation);
        }
        changed
    }

    fn apply_pursuit_step(&mut self, step: PursuitStep) -> Option<Point> {
        if !self.seeking() {
            return None;
        }
        match step {
            PursuitStep::Move { delta, .. } => {
                // Navigation owns the live foot now, not a pre-bubble anchor.
                self.hidden_position = None;
                Some(delta)
            }
            PursuitStep::Arrived => {
                if matches!(self.navigation, Some(Navigation::Corner { .. })) {
                    self.cancel_navigation("corner arrived");
                    return None;
                }
                self.navigation = Some(Navigation::Celebrating);
                self.motion_clock.clear();
                self.select(Animation::Succeeded);
                info!(familiar = %self.id, "Pointer pursuit arrived");
                None
            }
            PursuitStep::Blocked(reason) => {
                warn!(familiar = %self.id, reason, "Navigation blocked");
                self.cancel_navigation(reason);
                None
            }
        }
    }

    fn prepare_running_pose(&mut self, facing: Facing, elapsed: Duration) {
        let animation = match facing {
            Facing::Left => Animation::RunningLeft,
            Facing::Right => Animation::RunningRight,
        };
        if self.player.animation() != animation {
            self.select(animation);
            info!(familiar = %self.id, ?facing, "Navigation direction changed");
        } else {
            self.advance_animation(elapsed);
        }
    }

    fn raster_key(&self, size: (u32, u32)) -> RasterKey {
        RasterKey {
            animation: self.player.animation(),
            frame: self.player.frame_index(),
            size,
            show: self.bubble_visible,
            sprite_scale: self.options.sprite_scale.to_bits(),
        }
    }

    fn cache_raster(&mut self, size: (u32, u32)) {
        let key = self.raster_key(size);
        if self.raster.as_ref().is_none_or(|cached| cached.key != key) {
            self.raster = Some(CachedRaster::new(
                key,
                self.atlas
                    .frame(self.player.animation(), self.player.frame_index()),
            ));
        }
    }

    fn plan_pursuit(
        &mut self,
        now: Instant,
        cursor: Point,
        origin: Point,
        size: (u32, u32),
        backing: f64,
        elapsed: Duration,
    ) -> PursuitStep {
        let Some(geometry) = sprite_geometry(
            origin,
            size,
            backing,
            self.bubble_visible,
            self.options.sprite_scale,
        ) else {
            return PursuitStep::Blocked("invalid window geometry");
        };
        // Confirm contact with the pose held since the previous motion tick,
        // including the actual result of its requested native move. A changed
        // scene must first get its own render opportunity, not reuse old alpha.
        if let Some(raster) = &self.raster
            && raster.key == self.raster_key(size)
            && raster.mask.contact(cursor, geometry)
        {
            return PursuitStep::Arrived;
        }
        let projection =
            bubble::SpriteProjection::new(size, self.bubble_visible, self.options.sprite_scale);
        let (x0, _, x1, _) = projection.bounds;
        let facing = running_facing(
            self.player.animation(),
            cursor.x,
            origin.x + (x0 + x1) as f64 / (2.0 * backing),
        );
        // Freeze this exact pose after planning until the next gated motion tick.
        self.prepare_running_pose(facing, elapsed);
        self.cache_raster(size);
        let raster = self
            .raster
            .as_ref()
            .expect("raster cached for current pose");
        let Some(Navigation::Seeking(navigator)) = &mut self.navigation else {
            return PursuitStep::Blocked("pursuit is no longer seeking");
        };
        let step = navigator.step_raster(now, cursor, geometry, &raster.mask);
        if step == PursuitStep::Arrived {
            // Newly selected contact art must be held, not skipped straight to
            // success. Stop here and confirm fresh feedback on the next tick.
            PursuitStep::Move {
                delta: Point::default(),
                facing,
            }
        } else {
            step
        }
    }

    fn pursue(&mut self, now: Instant, cursor: Point, elapsed: Duration) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let backing = window.scale_factor();
        let Ok(position) = window.outer_position() else {
            self.apply_pursuit_step(PursuitStep::Blocked("window position unavailable"));
            return;
        };
        let origin = position.to_logical::<f64>(backing);
        let size = window.inner_size();
        let step = self.plan_pursuit(
            now,
            cursor,
            Point {
                x: origin.x,
                y: origin.y,
            },
            (size.width, size.height),
            backing,
            elapsed,
        );
        if let Some(delta) = self.apply_pursuit_step(step) {
            let target = self.native_motion.request(
                Point {
                    x: origin.x,
                    y: origin.y,
                },
                delta,
                backing,
            );
            if target.x != origin.x || target.y != origin.y {
                window.set_outer_position(LogicalPosition::new(target.x, target.y));
            }
        }
    }

    fn start_corner(
        &mut self,
        now: Instant,
        display: corner::DisplayArea,
        target: Point,
        size: Point,
    ) {
        if self.close_requested || self.error.is_some() {
            return;
        }
        // Unlike M, C does not change manual/live source selection or attachment.
        self.cancel_restore_for_user_intent();
        if self.corner_return.is_none() {
            self.corner_return = Some(if self.navigation.is_some() && !self.display.is_manual() {
                self.display.resume()
            } else {
                self.player.animation()
            });
        }
        self.navigation = Some(Navigation::Corner {
            navigator: Navigator::new(self.options.move_speed, now),
            display,
            target,
            size,
        });
        self.hidden_position = None;
        self.motion_clock.reset(now);
        self.native_motion = NativeMotion::default();
        self.last_tick = now;
    }

    fn request_corner(&mut self, now: Instant) {
        let Some(window) = &self.window else {
            return;
        };
        let Some(display) = window.current_monitor().as_ref().and_then(corner::display) else {
            self.cancel_navigation("current display unavailable");
            return;
        };
        let size = window.inner_size();
        let backing = window.scale_factor();
        if !backing.is_finite() || backing <= 0.0 || size.width == 0 || size.height == 0 {
            self.cancel_navigation("invalid corner geometry");
            return;
        }
        let (x0, y0, x1, y1) = bubble::SpriteProjection::new(
            (size.width, size.height),
            self.bubble_visible,
            self.options.sprite_scale,
        )
        .bounds;
        let Some(target) = corner::Corner::random().target(
            display.work,
            (x1 - x0) as f64 / backing,
            (y1 - y0) as f64 / backing,
        ) else {
            self.cancel_navigation("invalid corner geometry");
            return;
        };
        self.start_corner(
            now,
            display,
            target,
            Point {
                x: (x1 - x0) as f64 / backing,
                y: (y1 - y0) as f64 / backing,
            },
        );
    }

    fn pursue_corner(&mut self, now: Instant, elapsed: Duration) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let Some(Navigation::Corner {
            display,
            target,
            size: sprite_size,
            ..
        }) = &self.navigation
        else {
            return;
        };
        let display = *display;
        let target = *target;
        let sprite_size = *sprite_size;
        // Keep the original monitor, even if the moving window crosses an edge.
        let current = window
            .available_monitors()
            .filter_map(|m| corner::display(&m))
            .find(|d| d.id == display.id);
        if current != Some(display) {
            self.cancel_navigation("corner display removed or work area changed");
            return;
        }
        let backing = window.scale_factor();
        let size = window.inner_size();
        let (x0, y0, x1, y1) = bubble::SpriteProjection::new(
            (size.width, size.height),
            self.bubble_visible,
            self.options.sprite_scale,
        )
        .bounds;
        // A scale/layout change must not make a previously safe destination clip
        // a larger frame. Cancel, rather than silently choose another corner.
        if !backing.is_finite()
            || backing <= 0.0
            || ((x1 - x0) as f64 / backing - sprite_size.x).abs() > 1.0 / backing
            || ((y1 - y0) as f64 / backing - sprite_size.y).abs() > 1.0 / backing
        {
            self.cancel_navigation("corner sprite dimensions changed");
            return;
        }
        let geometry = window.outer_position().ok().and_then(|position| {
            let position = position.to_logical::<f64>(backing);
            let origin = Point {
                x: position.x,
                y: position.y,
            };
            let foot = foot_offset(
                (size.width, size.height),
                backing,
                self.bubble_visible,
                self.options.sprite_scale,
            )?;
            Some((
                origin,
                Point {
                    x: origin.x + foot.x,
                    y: origin.y + foot.y,
                },
            ))
        });
        let Some((origin, foot)) = geometry else {
            self.cancel_navigation("corner position unavailable");
            return;
        };
        let Some(Navigation::Corner { navigator, .. }) = &mut self.navigation else {
            return;
        };
        let step = navigator.step_fixed(now, foot, target, backing);
        if let PursuitStep::Move { facing, .. } = step {
            self.prepare_running_pose(facing, elapsed);
        }
        if let Some(delta) = self.apply_pursuit_step(step) {
            let position = self.native_motion.request(origin, delta, backing);
            if position != origin {
                window.set_outer_position(LogicalPosition::new(position.x, position.y));
            }
        }
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        if let Some(pixels) = &mut self.pixels {
            self.scene_dirty = true;
            pixels.resize_surface(width, height)?;
            pixels.resize_buffer(width, height)?;
            self.frame_size = (width, height);
        }
        Ok(())
    }

    fn reveal_bubbles(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.visibility.reveal(now) {
            info!(familiar = %self.id, "Familiar bubbles requested");
        }
        if let Err(error) = self.sync_visibility(now) {
            self.fail(event_loop, error);
        }
    }

    fn sync_visibility(&mut self, now: Instant) -> Result<()> {
        if self.geometry_held() {
            return Ok(());
        }
        let show = self.visibility.visible(now);
        if show == self.bubble_visible {
            return Ok(());
        }
        let old_show = self.bubble_visible;
        self.scene_dirty = true;
        self.raster = None;
        self.bubble_visible = show;
        self.layout(old_show, self.options.sprite_scale)?;
        info!(familiar = %self.id, visible = show, "Bubble visibility changed");
        Ok(())
    }

    fn layout(&mut self, old_show: bool, old_scale: f32) -> Result<()> {
        let show = self.bubble_visible;
        if let Some(window) = self.window.clone() {
            let position = window.outer_position().ok();
            let physical_scale = self.options.scale * window.scale_factor();
            let old_size = window.inner_size().into();
            let new_size = layout_size(show, self.options.sprite_scale, physical_scale);
            let anchored = position.map(|origin| {
                resized_position(
                    origin,
                    (old_size, old_show, old_scale),
                    (new_size, show, self.options.sprite_scale),
                )
            });
            if old_scale != self.options.sprite_scale {
                self.hidden_position = self.hidden_position.map(|origin| {
                    resized_position(
                        origin,
                        (
                            layout_size(false, old_scale, physical_scale),
                            false,
                            old_scale,
                        ),
                        (
                            layout_size(false, self.options.sprite_scale, physical_scale),
                            false,
                            self.options.sprite_scale,
                        ),
                    )
                });
            }
            // Restore the saved position, rather than interpreting a delayed
            // window-manager move as a user drag. A real drag clears the saved
            // anchor and commits the new sprite location instead.
            let desired = if show {
                anchored
            } else {
                self.hidden_position.or(anchored)
            };
            let desired = desired.map(|desired| {
                if (show || old_scale != self.options.sprite_scale)
                    && let Some(monitor) = window.current_monitor()
                {
                    return clamp_to_monitor(
                        desired,
                        monitor.position(),
                        monitor.size().into(),
                        (f64::from(new_size.0), f64::from(new_size.1)),
                    );
                }
                desired
            });
            // Resize/reposition only; never activate or focus on a notification.
            let _ = window.request_inner_size(PhysicalSize::new(new_size.0, new_size.1));
            if let Some(desired) = desired {
                window.set_outer_position(desired);
            }
            if show && !old_show {
                self.hidden_position = position;
            } else if !show {
                self.hidden_position = None;
            }
            let size = window.inner_size();
            self.resize(size.width, size.height)?;
            window.request_redraw();
            debug!(familiar = %self.id, position = ?window.outer_position().ok(),
                framebuffer_width = self.frame_size.0, framebuffer_height = self.frame_size.1,
                "Bubble layout applied");
        }
        Ok(())
    }
}

impl FamiliarView {
    fn update(&mut self, event_loop: &ActiveEventLoop, view: Presentation) {
        if self.latest_view != view {
            self.visibility.observe(&view, Instant::now());
            let represented = view
                .cue
                .as_ref()
                .map(|cue| &cue.selection)
                .or(view.base.as_ref());
            if let Some(selection) = represented {
                info!(familiar = %self.id, source = %selection.source, activity = %selection.activity,
                    state = selection.state.label(), attention = selection.attention,
                    "Live activity changed");
                if let Some(window) = &self.window {
                    window.set_title(&format!(
                        "Numen — {DEFAULT_CHARACTER_NAME} [{} / {}: {}]",
                        selection.source,
                        selection.activity,
                        selection.state.label()
                    ));
                }
            } else {
                info!(familiar = %self.id, "No live activity; resting");
                if let Some(window) = &self.window {
                    window.set_title(&format!("Numen — {DEFAULT_CHARACTER_NAME}"));
                }
            }
            self.latest_view = view.clone();
            self.scene_dirty = true;
            self.source_animation(view);
            if !self.options.no_bubble {
                let text = bubble::text(
                    &self.latest_view,
                    self.display.is_manual().then_some(self.player.animation()),
                );
                // Do not log summary text; it may contain private conversation details.
                debug!(identity = %text.identity, status = %text.status, "Thought bubble updated");
            }
            // Metadata changes must repaint even if the animation is unchanged
            // or paused. They must never reset the player or steal focus.
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            if let Err(error) = self.sync_visibility(Instant::now()) {
                self.fail(event_loop, error);
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) -> Option<MenuRequest> {
        if self
            .window
            .as_ref()
            .is_none_or(|window| window.id() != window_id)
        {
            return None;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                if self.gesture.moved(
                    position,
                    context_menu::supported() && context_menu::left_button_down(),
                ) && let Some(drag_start) = self.drag_start.take()
                    && let Some(window) = self.window.clone()
                {
                    match drag_start.start(&window) {
                        Ok(()) => self.commit_drag(),
                        Err(error) => {
                            warn!(%error, "Desktop does not support native window dragging");
                        }
                    }
                }
            }
            WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. } => {
                self.gesture.cancel_click();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if context_menu::supported() => {
                let position = self.release_click(Instant::now());
                if let Some(position) = position {
                    return self.menu_request(position);
                }
            }
            WindowEvent::CloseRequested | WindowEvent::Destroyed => self.close(event_loop),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                if let PhysicalKey::Code(code) = event.physical_key {
                    self.keyboard(event_loop, code);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                match button {
                    MouseButton::Left => {
                        self.cancel_restore_for_user_intent();
                        if context_menu::supported() {
                            if let Some(window) = &self.window {
                                self.gesture.press(window.scale_factor());
                                self.drag_binding = self
                                    .preference
                                    .as_ref()
                                    .filter(|preference| preference.writable)
                                    .map(|preference| preference.binding.clone());
                                self.drag_start = match context_menu::capture_drag(window) {
                                    Ok(drag_start) => Some(drag_start),
                                    Err(error) => {
                                        warn!(%error, "Could not capture native drag press");
                                        None
                                    }
                                };
                                self.resume_navigation(Instant::now());
                            }
                            return None;
                        }
                        self.cancel_navigation("native drag");
                        // Explicit dragging commits the current expanded
                        // location; do not snap back to its pre-notification anchor.
                        self.hidden_position = None;
                        if let Some(window) = &self.window {
                            // Focus only after an explicit user interaction, never on a timer.
                            window.focus_window();
                            if let Err(error) = window.drag_window() {
                                warn!(%error, "Desktop does not support native window dragging");
                            }
                        }
                    }
                    MouseButton::Right => {
                        self.cancel_restore_for_user_intent();
                        if context_menu::supported() {
                            if self.gesture.held {
                                return None;
                            }
                            let position = self.gesture.cursor;
                            self.menu_open = position.is_some();
                            self.end_interaction(Instant::now());
                            return position.and_then(|position| self.menu_request(position));
                        }
                        self.cycle();
                        self.reveal_bubbles(event_loop);
                    }
                    _ => {}
                }
            }
            WindowEvent::Resized(size) => {
                // Do not catch up hours of animation after a zero-sized window restores.
                // Ordinary layout edits must not reset a running pursuit or clip.
                if size.width == 0
                    || size.height == 0
                    || self.frame_size.0 == 0
                    || self.frame_size.1 == 0
                {
                    self.last_tick = Instant::now();
                    self.resume_navigation(self.last_tick);
                    self.next_demo = self.last_tick + DEMO_INTERVAL;
                }
                if size.width == 0 || size.height == 0 {
                    self.frame_size = (0, 0);
                    self.raster = None;
                    return None;
                }
                if let Err(error) = self.resize(size.width, size.height) {
                    self.fail(event_loop, error);
                    return None;
                }
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                if self.navigation.is_none() {
                    self.last_tick = Instant::now();
                    self.next_demo = self.last_tick + DEMO_INTERVAL;
                }
                if !occluded && let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                // Reconcile physical dimensions in redraw, after the windowing
                // system has applied the monitor's new scale factor.
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested if self.drawable() => {
                if let Some(window) = &self.window {
                    let size = window.inner_size();
                    if (size.width, size.height) != self.frame_size
                        && let Err(error) = self.resize(size.width, size.height)
                    {
                        self.fail(event_loop, error);
                        return None;
                    }
                }
                if let (Some(pixels), Some(renderer)) = (&mut self.pixels, &mut self.renderer) {
                    if self.scene_dirty {
                        renderer.compose(
                            pixels.frame_mut(),
                            self.frame_size,
                            self.atlas
                                .frame(self.player.animation(), self.player.frame_index()),
                            &self.latest_view,
                            self.display.is_manual().then_some(self.player.animation()),
                            self.bubble_visible,
                        );
                        self.scene_dirty = false;
                    }
                    if let Err(error) = pixels.render() {
                        self.fail(event_loop, error.into());
                    } else {
                        self.draws += 1;
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn tick(
        &mut self,
        event_loop: &ActiveEventLoop,
        now: Instant,
        cursor: Option<Point>,
        motion_due: bool,
    ) -> Option<Instant> {
        if context_menu::supported() && self.gesture.held {
            // AppKit can finish dragging without delivering a winit release.
            self.poll_interaction(now, context_menu::left_button_down());
        }
        self.restore_placement(now);
        let mut preference_wake = self.retry_placement(now);
        if let Some(restore) = &self.pending_restore {
            let deadline = restore.poll_at.min(restore.deadline);
            preference_wake = Some(preference_wake.map_or(deadline, |wake| wake.min(deadline)));
        }
        if self.geometry_held() {
            self.last_tick = now;
            self.resume_navigation(now);
            return Some(now + Duration::from_millis(20));
        }
        self.visibility.expire(now);
        if let Err(error) = self.flush_geometry(now) {
            self.fail(event_loop, error);
            return None;
        }
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let mut wake_at: Option<Instant> = preference_wake;
        if let Some(deadline) = self.visibility.deadline() {
            wake_at = Some(wake_at.map_or(deadline, |wake| wake.min(deadline)));
        }
        let was_seeking = self.seeking();
        if was_seeking {
            if self.motion_eligible() {
                if self.motion_clock.deadline().is_none() {
                    // A zero-sized native view may restore without a resize wake
                    // reaching this handler first. Discard its suspended time.
                    self.resume_navigation(now);
                }
                if motion_due
                    && (cursor.is_some()
                        || matches!(self.navigation, Some(Navigation::Corner { .. })))
                    && let Some(motion_elapsed) = self.motion_clock.take(now)
                {
                    if matches!(self.navigation, Some(Navigation::Corner { .. })) {
                        self.pursue_corner(now, motion_elapsed);
                    } else if let Some(cursor) = cursor {
                        self.pursue(now, cursor, motion_elapsed);
                    }
                    if let Some(window) = &self.window {
                        // Moving an unchanged local image does not dirty its pixels.
                        if self.scene_dirty {
                            window.request_redraw();
                        }
                    }
                }
            } else {
                self.motion_clock.clear();
                if let Some(Navigation::Seeking(navigator) | Navigation::Corner { navigator, .. }) =
                    &mut self.navigation
                {
                    navigator.resume(now);
                }
            }
            // The root's hard motion deadline owns seeking, never clip frames.
            // A just-selected success clip starts advancing on a subsequent tick.
            if self.seeking() {
                return wake_at;
            }
        }
        let animate = self.animation_eligible(self.native_geometry_valid());
        if animate {
            let changed =
                self.advance_animation(if was_seeking { Duration::ZERO } else { elapsed });
            if self.options.demo && self.navigation.is_none() && now >= self.next_demo {
                // Use a separate cursor: one-shot clips return to idle before
                // their demo interval ends, so current animation is not a cursor.
                self.cycle();
            } else if changed && let Some(window) = &self.window {
                window.request_redraw();
            }
            let frame_at = now + self.player.until_next_frame(&self.atlas);
            wake_at = Some(wake_at.map_or(frame_at, |deadline| deadline.min(frame_at)));
            if self.options.demo && self.navigation.is_none() {
                wake_at = wake_at.map(|deadline| deadline.min(self.next_demo));
            }
        }
        wake_at
    }
}

struct App {
    views: BTreeMap<String, FamiliarView>,
    atlas: Arc<SpriteAtlas>,
    options: RunOptions,
    mailbox: Arc<Mailbox<Vec<FamiliarPresentation>>>,
    reveal_pending: Arc<RevealMailbox>,
    navigation_pending: Arc<NavigationMailbox>,
    pointer: Option<PointerReader>,
    motion_clock: MotionClock,
    control: Option<FamiliarControl>,
    typeface: Option<Typeface>,
    initialized: bool,
    live: bool,
    initial_snapshot: bool,
    global_hotkey_registered: bool,
    global_navigation_registered: bool,
    global_corner_registered: bool,
    exit_at: Option<Instant>,
    error: Option<anyhow::Error>,
    // Failed native views stay suppressed until the registry acknowledges removal.
    failed: BTreeSet<String>,
    pending_close: BTreeSet<String>,
    submitted_close: BTreeSet<String>,
    popup: PopupLifecycle,
    quit_pending: Arc<AtomicBool>,
}

impl App {
    fn new(atlas: SpriteAtlas, options: RunOptions, live: bool) -> Self {
        Self {
            views: BTreeMap::new(),
            atlas: Arc::new(atlas),
            options,
            mailbox: Arc::new(Mailbox::default()),
            reveal_pending: Arc::new(RevealMailbox::default()),
            navigation_pending: Arc::new(NavigationMailbox::default()),
            pointer: None,
            motion_clock: MotionClock::default(),
            control: None,
            typeface: None,
            initialized: false,
            live,
            initial_snapshot: true,
            global_hotkey_registered: false,
            global_navigation_registered: false,
            global_corner_registered: false,
            exit_at: None,
            error: None,
            failed: BTreeSet::new(),
            pending_close: BTreeSet::new(),
            submitted_close: BTreeSet::new(),
            popup: PopupLifecycle::default(),
            quit_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    fn initialize(&mut self) -> Result<()> {
        if !self.options.no_bubble {
            let face = Typeface::load(
                self.options.font_family.as_deref(),
                self.options.font_file.as_deref(),
            )
            .context("cannot load conversation font; choose --font-family or --font-file")?;
            info!(
                face = face.description(),
                font_size = self.options.font_size,
                "Conversation font selected"
            );
            self.typeface = Some(face);
        }
        // Font discovery is common initialization; GPU creation counts toward the deadline.
        self.exit_at = self
            .options
            .exit_after
            .map(|duration| Instant::now() + duration);
        self.initialized = true;
        Ok(())
    }

    // Apply complete registry settings before creating any new native windows.
    fn stage_snapshot(
        &mut self,
        snapshot: Vec<FamiliarPresentation>,
    ) -> Vec<(String, Presentation)> {
        let ids: BTreeSet<_> = snapshot.iter().map(|view| view.id.clone()).collect();
        self.views.retain(|id, _| {
            let retain = ids.contains(id);
            if !retain {
                info!(familiar = %id, "Familiar view closed");
            }
            retain
        });
        self.failed.retain(|id| ids.contains(id));
        self.pending_close.retain(|id| ids.contains(id));
        self.submitted_close.retain(|id| ids.contains(id));
        let mut updates = Vec::new();
        for familiar in snapshot {
            let id = familiar.id;
            if self.failed.contains(&id) {
                continue;
            }
            if familiar.preset != DEFAULT_CHARACTER_ID {
                warn!(%id, preset = %familiar.preset, "Unsupported familiar preset");
                self.views.remove(&id);
                self.pending_close.insert(id.clone());
                self.failed.insert(id);
                continue;
            }
            if !self.views.contains_key(&id) {
                let mut options = self.options.clone();
                options.focus = self.initial_snapshot && id == "main" && options.focus;
                let mut view = FamiliarView::new(id.clone(), self.atlas.clone(), options);
                view.focus_control = self.control.clone();
                view.global_hotkey_registered = self.global_hotkey_registered;
                view.global_navigation_registered = self.global_navigation_registered;
                view.global_corner_registered = self.global_corner_registered;
                view.corner_available = self.live
                    && !self.options.manual
                    && !self.options.demo
                    && cfg!(target_os = "macos");
                view.navigation_available = self.pointer.is_some();
                self.views.insert(id.clone(), view);
            }
            if let Some(view) = self.views.get_mut(&id) {
                view.runtime_binding = familiar.runtime_binding;
                view.set_preference(familiar.preference);
                if let Err(error) = view.apply_settings(familiar.settings) {
                    view.error = Some(error);
                }
            }
            updates.push((id, familiar.presentation));
        }
        self.initial_snapshot = false;
        updates
    }

    fn reconcile(&mut self, event_loop: &ActiveEventLoop, snapshot: Vec<FamiliarPresentation>) {
        let updates = self.stage_snapshot(snapshot);
        let next_position = |position: PhysicalPosition<i32>| {
            PhysicalPosition::new(position.x.saturating_add(48), position.y.saturating_add(48))
        };
        let mut cascade = self
            .views
            .values()
            .filter_map(|view| view.window.as_ref())
            .last()
            .and_then(|window| window.outer_position().ok())
            .map(next_position);
        for (id, presentation) in updates {
            if let Some(view) = self.views.get_mut(&id) {
                if view.error.is_some() {
                    continue;
                }
                let created = view.window.is_none();
                if view.window.is_none()
                    && let Err(error) =
                        view.initialize(event_loop, self.typeface.as_ref().map(Typeface::fork))
                {
                    view.error = Some(error);
                    continue;
                }
                if created && id == "main" && !view.ever_bound {
                    view.bootstrap_unbound = true;
                }
                let restore =
                    (created || view.first_binding_restore) && view.begin_initial_placement();
                if created
                    && !restore
                    && let (Some(position), Some(window)) = (cascade, &view.window)
                {
                    let position = if let Some(monitor) = window.current_monitor() {
                        let size = window.inner_size();
                        clamp_to_monitor(
                            position,
                            monitor.position(),
                            monitor.size().into(),
                            (f64::from(size.width), f64::from(size.height)),
                        )
                    } else {
                        position
                    };
                    window.set_outer_position(position);
                }
                view.update(event_loop, presentation);
                if restore {
                    view.restore_placement(Instant::now());
                }
                if created {
                    cascade = view
                        .window
                        .as_ref()
                        .and_then(|window| window.outer_position().ok())
                        .map(next_position);
                }
            }
        }
        self.handle_errors(event_loop);
    }

    fn handle_errors(&mut self, event_loop: &ActiveEventLoop) {
        let failures: Vec<_> = self
            .views
            .iter_mut()
            .filter_map(|(id, view)| view.error.take().map(|error| (id.clone(), error)))
            .collect();
        for (id, error) in failures {
            warn!(%id, %error, "Familiar view failed");
            self.views.remove(&id);
            if self.live {
                self.pending_close.insert(id.clone());
                self.failed.insert(id);
            } else {
                self.error = Some(error);
                event_loop.exit();
            }
        }
        if let Some(control) = self.control.clone() {
            self.retry_closes(|id| control.close(id));
        }
    }

    fn retry_closes(&mut self, mut submit: impl FnMut(&str) -> Result<()>) {
        self.pending_close.extend(
            self.views
                .iter()
                .filter(|(id, view)| view.close_requested && !self.submitted_close.contains(*id))
                .map(|(id, _)| id.clone()),
        );
        // Retain intent on queue-full, but never resubmit an accepted close while
        // waiting for the complete registry snapshot to acknowledge removal.
        self.pending_close.retain(|id| {
            if self.submitted_close.contains(id) {
                return false;
            }
            match submit(id) {
                Ok(()) => {
                    self.submitted_close.insert(id.clone());
                    false
                }
                Err(error) => {
                    warn!(%id, %error, "Could not close familiar; will retry");
                    true
                }
            }
        });
    }

    fn apply_reveals(&mut self, event_loop: &ActiveEventLoop) {
        let targets = reveal_targets(self.reveal_pending.take(), self.views.keys().cloned());
        for id in targets {
            if let Some(view) = self.views.get_mut(&id) {
                view.reveal_bubbles(event_loop);
            }
        }
        self.handle_errors(event_loop);
    }

    fn apply_navigation(&mut self) {
        self.apply_navigation_with(|view, now| view.request_corner(now));
    }

    fn apply_navigation_with(&mut self, mut start_corner: impl FnMut(&mut FamiliarView, Instant)) {
        // Resolve broadcasts here, after the newest registry snapshot, not in callbacks.
        let now = Instant::now();
        for (corner, request) in self.navigation_pending.take() {
            let targets = reveal_targets(VecDeque::from([request]), self.views.keys().cloned());
            for id in targets {
                if !self.pending_close.contains(&id)
                    && !self.submitted_close.contains(&id)
                    && !self.failed.contains(&id)
                    && let Some(view) = self.views.get_mut(&id)
                {
                    if corner {
                        if view.corner_available {
                            start_corner(view, now);
                        }
                    } else {
                        view.start_navigation(now);
                    }
                }
            }
        }
    }

    fn menu_target(&self, id: &str, window_id: WindowId) -> Option<&FamiliarView> {
        self.views.get(id).filter(|view| {
            menu_target_valid(
                window_id,
                view.window.as_ref().map(|window| window.id()),
                view.close_requested
                    || self.pending_close.contains(id)
                    || self.submitted_close.contains(id),
                view.error.is_some() || self.failed.contains(id),
            )
        })
    }

    fn show_menu(&mut self, event_loop: &ActiveEventLoop, request: MenuRequest) {
        // Publish the target before draining snapshots: removal racing this drain
        // must either invalidate the target here or cancel native tracking.
        self.popup.begin(&request.familiar);
        if let Some(snapshot) = self.mailbox.take() {
            self.reconcile(event_loop, snapshot);
        }
        let menu_binding = self
            .menu_target(&request.familiar, request.window_id)
            .and_then(|view| view.preference.as_ref().filter(|p| p.writable))
            .map(|p| p.binding.clone());
        let menu_session = self
            .menu_target(&request.familiar, request.window_id)
            .map(|view| view.runtime_binding.clone());
        let (attachments, menu_attachment) = match (
            self.control.as_ref(),
            self.menu_target(&request.familiar, request.window_id),
        ) {
            (Some(control), Some(view)) => {
                let snapshot = match control.target_snapshot(&request.familiar) {
                    Ok(snapshot) => Some(snapshot),
                    Err(error) => {
                        warn!(%error, "Chat list unavailable; reopen menu to retry");
                        None
                    }
                };
                let (context, frozen) =
                    attachment_context(&request.familiar, &view.runtime_binding, snapshot);
                (Some(context), frozen)
            }
            _ => (None, None),
        };
        let context = self
            .menu_target(&request.familiar, request.window_id)
            .map(|view| context_menu::Context {
                paused: view.paused,
                bubbles: !view.options.no_bubble,
                copy_session_id: menu_session_id(menu_session.as_ref(), &view.runtime_binding)
                    .is_some(),
                open_chat: crate::chat::supported()
                    && menu_attachment.as_ref().is_some_and(|frozen| {
                        frozen.open_chat_index(&view.runtime_binding).is_some()
                    }),
                navigation: view.navigation_available && self.pointer.is_some(),
                settings: self.control.as_ref().map(|_| view.settings),
                preferences: menu_binding.is_some(),
                attachments,
                open_familiar: self
                    .control
                    .as_ref()
                    .map(|_| self.views.len() < crate::protocol::MAX_FAMILIARS),
            });
        let result = if let Some(context) =
            context.filter(|_| !event_loop.exiting() && !self.quit_pending.load(Ordering::Acquire))
        {
            info!(familiar = %request.familiar, "Familiar menu opened");
            let cancelled = self.popup.cancelled.clone();
            // The helper is bounded by tracking, not the deadline. Waking it on
            // return avoids delaying an explicit Quit Numen selection.
            std::thread::scope(|scope| {
                let (done, receiver) = std::sync::mpsc::channel::<()>();
                if let Some(deadline) = self.exit_at {
                    let cancelled = cancelled.clone();
                    scope.spawn(move || {
                        if matches!(
                            receiver
                                .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                        ) {
                            cancelled.store(true, Ordering::Release);
                        }
                    });
                }
                let result =
                    context_menu::show(&request.window, request.position, context, cancelled);
                let _ = done.send(());
                result
            })
        } else {
            Ok(None)
        };
        self.popup.end();
        // Rebase every pursuit, since native tracking blocked all Rust ticks.
        let now = Instant::now();
        for view in self.views.values_mut() {
            view.last_tick = now;
            view.resume_navigation(now);
        }
        if let Some(snapshot) = self.mailbox.take() {
            self.reconcile(event_loop, snapshot);
        }
        if let Some(view) = self.views.get_mut(&request.familiar)
            && view
                .window
                .as_ref()
                .is_some_and(|window| window.id() == request.window_id)
            && let Err(error) = view.finish_menu(now)
        {
            view.fail(event_loop, error);
        }
        if self.quit_pending.load(Ordering::Acquire)
            || self.exit_at.is_some_and(|deadline| now >= deadline)
        {
            event_loop.exit();
            return;
        }
        match result {
            Ok(Some(action)) => {
                info!(familiar = %request.familiar, ?action, "Familiar menu action selected");
                self.dispatch_menu(
                    event_loop,
                    &request.familiar,
                    request.window_id,
                    action,
                    &MenuBindings {
                        preference: menu_binding,
                        attachment: menu_attachment,
                        session: menu_session,
                    },
                )
            }
            Ok(None) => {}
            Err(error) => warn!(%error, "Could not show familiar menu"),
        }
    }

    fn dispatch_menu(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: &str,
        window_id: WindowId,
        action: Action,
        bindings: &MenuBindings,
    ) {
        let Some(action) =
            validated_menu_action(Some(action), self.menu_target(id, window_id).is_some())
        else {
            return;
        };
        if event_loop.exiting() {
            return;
        }
        // Service-wide actions do not depend on the clicked familiar surviving.
        if self.dispatch_service_menu(action, || event_loop.exit()) {
            return;
        }
        let view = self.views.get_mut(id).expect("validated menu target");
        match action {
            Action::RevealStatus if !view.options.no_bubble => view.reveal_bubbles(event_loop),
            Action::CopySessionId => {
                if let Some(session_id) =
                    menu_session_id(bindings.session.as_ref(), &view.runtime_binding)
                    && let Err(error) = context_menu::copy_text(session_id)
                {
                    warn!(%error, "Could not copy session ID");
                }
            }
            Action::OpenChat if crate::chat::supported() => {
                let Some(frozen) = bindings.attachment.as_ref() else {
                    return;
                };
                let Some(index) = frozen.open_chat_index(&view.runtime_binding) else {
                    return;
                };
                let Some(control) = &self.control else { return };
                let fresh = control.target_snapshot(id).ok();
                if let Err(error) = frozen.queue(
                    id,
                    index,
                    &view.runtime_binding,
                    fresh.as_ref(),
                    |binding, choice| {
                        control.open_chat(id, binding, &choice.target, choice.incarnation)
                    },
                ) {
                    warn!(%error, "Could not queue live chat; reopen menu to retry");
                }
            }
            Action::GoToMouse if view.navigation_available && self.pointer.is_some() => {
                self.navigation_pending.post(Some(vec![id.to_owned()]));
                self.apply_navigation();
            }
            Action::SetPaused(paused) => view.set_paused(paused),
            Action::Attach(index) => {
                let Some(frozen) = bindings.attachment.as_ref() else {
                    return;
                };
                let Some(control) = &self.control else { return };
                // Only the canonical store's committed snapshot changes this view.
                let fresh = control.target_snapshot(id).ok();
                if let Err(error) = frozen.queue(
                    id,
                    index,
                    &view.runtime_binding,
                    fresh.as_ref(),
                    |binding, choice| {
                        control.attach(id, binding, &choice.target, choice.incarnation)
                    },
                ) {
                    warn!(%id, %error, "Could not queue chat attachment; reopen menu to retry");
                }
            }
            Action::Settings(patch) => {
                if let Some(control) = &self.control
                    && let Err(error) = control.settings(id, patch)
                {
                    warn!(%id, %error, "Could not queue familiar settings");
                }
            }
            Action::ForgetPreferences => {
                let Some(binding) = view.validated_forget_binding(bindings.preference.as_ref())
                else {
                    return;
                };
                view.pending_placement = None;
                view.placement_retry = None;
                if let Some(control) = &self.control
                    && let Err(error) = control.forget_preferences(id, binding)
                {
                    warn!(%id, %error, "Could not queue forgetting familiar preferences");
                }
            }
            Action::CloseFamiliar => view.close(event_loop),
            _ => {}
        }
    }

    fn dispatch_service_menu(&self, action: Action, quit: impl FnOnce()) -> bool {
        match action {
            Action::QuitNumen => {
                info!("Quit Numen requested; closing graphical service");
                quit();
            }
            Action::OpenFamiliar => {
                if let Some(control) = &self.control
                    && let Err(error) = control.open_unattached(DEFAULT_CHARACTER_ID)
                {
                    warn!(%error, "Could not queue opening another familiar");
                }
            }
            _ => return false,
        }
        true
    }
}

fn menu_target_valid(
    requested: WindowId,
    current: Option<WindowId>,
    closing: bool,
    failed: bool,
) -> bool {
    current == Some(requested) && !closing && !failed
}

fn validated_menu_action(action: Option<Action>, target_valid: bool) -> Option<Action> {
    action.filter(|action| action.is_service() || target_valid)
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.initialized {
            return;
        }
        if let Err(error) = self.initialize() {
            self.error = Some(error);
            event_loop.exit();
            return;
        }
        // Take after font loading so a newer complete registry always wins.
        if self.live {
            if let Some(snapshot) = self.mailbox.take() {
                self.reconcile(event_loop, snapshot);
            }
        } else {
            self.reconcile(
                event_loop,
                vec![FamiliarPresentation {
                    id: "main".into(),
                    preset: DEFAULT_CHARACTER_ID.into(),
                    settings: familiar_settings(&self.options),
                    presentation: Presentation::default(),
                    preference: None,
                    runtime_binding: unbound_attachment(),
                }],
            );
        }
        self.apply_reveals(event_loop);
        self.apply_navigation();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        if !self.initialized && !matches!(event, AppEvent::QuitService) {
            return;
        }
        match event {
            AppEvent::QuitService => {
                info!("Graphical service shutdown requested");
                event_loop.exit();
            }
            AppEvent::ActivityChanged => {
                if let Some(snapshot) = self.mailbox.take() {
                    self.reconcile(event_loop, snapshot);
                }
            }
            AppEvent::RevealBubbles => {
                // A reveal can race the registry's wake; apply the newest complete
                // registry first so freshly opened targeted views are not missed.
                if let Some(snapshot) = self.mailbox.take() {
                    self.reconcile(event_loop, snapshot);
                }
                self.apply_reveals(event_loop);
            }
            AppEvent::GoToMouse => {
                if let Some(snapshot) = self.mailbox.take() {
                    self.reconcile(event_loop, snapshot);
                }
                self.apply_navigation();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // A local fallback participates in the same ordered mailbox as globals.
        // Reconcile first so broadcasts include the newest service registry.
        if let Some(snapshot) = self.mailbox.take() {
            self.reconcile(event_loop, snapshot);
        }
        let local_corner = matches!(&event, WindowEvent::KeyboardInput { event, .. }
            if event.state == ElementState::Pressed && !event.repeat
                && event.physical_key == PhysicalKey::Code(KeyCode::KeyC))
            && self.views.values().any(|view| {
                view.local_corner(
                    KeyCode::KeyC,
                    view.window
                        .as_ref()
                        .is_some_and(|w| w.id() == window_id && w.has_focus()),
                )
            });
        if local_corner {
            self.navigation_pending.post_corner();
        }
        let local_mouse = if matches!(&event, WindowEvent::KeyboardInput { event, .. }
            if event.state == ElementState::Pressed && !event.repeat
                && event.physical_key == PhysicalKey::Code(KeyCode::KeyM))
        {
            self.views
                .values()
                .find(|view| {
                    view.window
                        .as_ref()
                        .is_some_and(|w| w.id() == window_id && w.has_focus())
                        && local_navigation_shortcut(
                            KeyCode::KeyM,
                            view.modifiers,
                            view.global_navigation_registered,
                            view.navigation_available,
                        )
                })
                .map(|view| view.id.clone())
        } else {
            None
        };
        if let Some(id) = &local_mouse {
            self.navigation_pending.post(Some(vec![id.clone()]));
        }
        self.apply_navigation();
        if local_corner || local_mouse.is_some() {
            return;
        }
        let request = self
            .views
            .values_mut()
            .find(|view| {
                view.window
                    .as_ref()
                    .is_some_and(|window| window.id() == window_id)
            })
            .and_then(|view| view.window_event(event_loop, window_id, event));
        self.handle_errors(event_loop);
        if let Some(request) = request {
            self.show_menu(event_loop, request);
            self.handle_errors(event_loop);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.exit_at.is_some_and(|deadline| now >= deadline) {
            event_loop.exit();
            return;
        }
        let mut wake_at = self.exit_at;
        let needs_motion = self.views.values().any(FamiliarView::motion_eligible);
        let needs_pointer = self
            .views
            .values()
            .any(|v| v.motion_eligible() && v.mouse_seeking());
        if !needs_motion {
            self.motion_clock.clear();
        }
        let motion_due = needs_motion && self.motion_clock.take(now).is_some();
        let cursor = if needs_pointer && motion_due {
            match self
                .pointer
                .as_ref()
                .context("live pointer reader unavailable")
                .and_then(PointerReader::position)
            {
                Ok(point) => Some(point),
                Err(error) => {
                    for view in self.views.values_mut().filter(|view| view.mouse_seeking()) {
                        warn!(familiar = %view.id, %error, "Pointer pursuit query failed");
                        view.cancel_navigation("pointer unavailable");
                    }
                    None
                }
            }
        } else {
            None
        };
        for view in self.views.values_mut() {
            if let Some(deadline) = view.tick(event_loop, now, cursor, motion_due) {
                wake_at = Some(wake_at.map_or(deadline, |wake| wake.min(deadline)));
            }
        }
        self.handle_errors(event_loop);
        if self.views.values().any(FamiliarView::motion_eligible) {
            let deadline = self.motion_clock.deadline().unwrap_or(now);
            wake_at = Some(wake_at.map_or(deadline, |wake| wake.min(deadline)));
        } else {
            self.motion_clock.clear();
        }
        if !self.pending_close.is_empty() {
            let retry = now + Duration::from_millis(100);
            wake_at = Some(wake_at.map_or(retry, |wake| wake.min(retry)));
        }
        event_loop.set_control_flow(wake_at.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
}

// Desktop coordinates stay in Quartz/winit logical points, including negatives.
fn foot_offset(inner: (u32, u32), backing: f64, show: bool, scale: f32) -> Option<Point> {
    if inner.0 == 0
        || inner.1 == 0
        || !backing.is_finite()
        || backing <= 0.0
        || !scale.is_finite()
        || scale <= 0.0
    {
        return None;
    }
    let (x0, _, x1, y1) = bubble::SpriteProjection::new(inner, show, scale).bounds;
    let foot = Point {
        x: ((x0 + x1) as f64 / 2.0).round() / backing,
        y: y1 as f64 / backing,
    };
    (foot.x.is_finite() && foot.y.is_finite()).then_some(foot)
}

fn placement_from_geometry(
    origin: Point,
    inner: (u32, u32),
    backing: f64,
    show: bool,
    scale: f32,
) -> Option<Placement> {
    let foot = foot_offset(inner, backing, show, scale)?;
    let placement = Placement {
        x: origin.x + foot.x,
        y: origin.y + foot.y,
    };
    valid_placement(placement).then_some(placement)
}

fn valid_placement(placement: Placement) -> bool {
    placement.x.is_finite()
        && placement.y.is_finite()
        && placement.x.abs() <= 1_000_000.0
        && placement.y.abs() <= 1_000_000.0
}

#[derive(Clone, Copy, Debug)]
struct LogicalMonitor {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn logical_monitor(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    backing: f64,
) -> Option<LogicalMonitor> {
    if !backing.is_finite() || backing <= 0.0 || size.width == 0 || size.height == 0 {
        return None;
    }
    let position = position.to_logical::<f64>(backing);
    let size = size.to_logical::<f64>(backing);
    let rect = LogicalMonitor {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    };
    [
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        rect.x + rect.width,
        rect.y + rect.height,
    ]
    .iter()
    .all(|v| v.is_finite())
    .then_some(rect)
}

fn restored_origin(
    placement: Placement,
    inner: (u32, u32),
    backing: f64,
    show: bool,
    scale: f32,
    monitors: &[LogicalMonitor],
) -> Option<Point> {
    if !valid_placement(placement) {
        return None;
    }
    let foot = foot_offset(inner, backing, show, scale)?;
    let distance = |rect: &LogicalMonitor| {
        let dx = placement.x - placement.x.clamp(rect.x, rect.x + rect.width);
        let dy = placement.y - placement.y.clamp(rect.y, rect.y + rect.height);
        dx.hypot(dy)
    };
    let monitor = monitors
        .iter()
        .filter(|rect| {
            rect.width > 0.0
                && rect.height > 0.0
                && [
                    rect.x,
                    rect.y,
                    rect.width,
                    rect.height,
                    rect.x + rect.width,
                    rect.y + rect.height,
                ]
                .iter()
                .all(|v| v.is_finite())
        })
        .min_by(|a, b| {
            distance(a)
                .total_cmp(&distance(b))
                .then(a.x.total_cmp(&b.x))
                .then(a.y.total_cmp(&b.y))
                .then(a.width.total_cmp(&b.width))
                .then(a.height.total_cmp(&b.height))
        })?;
    let width = inner.0 as f64 / backing;
    let height = inner.1 as f64 / backing;
    if !width.is_finite() || !height.is_finite() {
        return None;
    }
    Some(Point {
        x: (placement.x - foot.x).clamp(
            monitor.x,
            (monitor.x + monitor.width - width).max(monitor.x),
        ),
        y: (placement.y - foot.y).clamp(
            monitor.y,
            (monitor.y + monitor.height - height).max(monitor.y),
        ),
    })
}

fn sprite_geometry(
    origin: Point,
    inner: (u32, u32),
    backing: f64,
    show: bool,
    sprite_scale: f32,
) -> Option<SpriteGeometry> {
    if !origin.x.is_finite()
        || !origin.y.is_finite()
        || !backing.is_finite()
        || backing <= 0.0
        || !sprite_scale.is_finite()
        || sprite_scale <= 0.0
        || inner.0 == 0
        || inner.1 == 0
    {
        return None;
    }
    let projection = bubble::SpriteProjection::new(inner, show, sprite_scale);
    let (x0, y0, _, _) = projection.bounds;
    let geometry = SpriteGeometry {
        origin: Point {
            x: origin.x + x0 as f64 / backing,
            y: origin.y + y0 as f64 / backing,
        },
        pixel_scale: 1.0 / backing,
    };
    (geometry.origin.x.is_finite()
        && geometry.origin.y.is_finite()
        && geometry.pixel_scale.is_finite()
        && geometry.pixel_scale > 0.0)
        .then_some(geometry)
}

fn running_facing(previous: Animation, cursor_x: f64, center_x: f64) -> Facing {
    if cursor_x < center_x {
        Facing::Left
    } else if cursor_x > center_x {
        Facing::Right
    } else if previous == Animation::RunningLeft {
        Facing::Left
    } else {
        Facing::Right
    }
}

fn animation_for_key(code: KeyCode) -> Option<Animation> {
    match code {
        KeyCode::Digit1 => Some(Animation::Idle),
        KeyCode::Digit2 => Some(Animation::Working),
        KeyCode::Digit3 => Some(Animation::Waiting),
        KeyCode::Digit4 => Some(Animation::Succeeded),
        KeyCode::Digit5 => Some(Animation::Failed),
        KeyCode::Digit6 => Some(Animation::Review),
        KeyCode::Digit7 => Some(Animation::Waving),
        KeyCode::Digit8 => Some(Animation::RunningLeft),
        KeyCode::Digit9 => Some(Animation::RunningRight),
        _ => None,
    }
}

fn local_reveal_shortcut(
    code: KeyCode,
    modifiers: ModifiersState,
    global_registered: bool,
) -> bool {
    // A registered global handler owns the gesture even when this window has
    // focus, avoiding a second reveal from a duplicate local keyboard event.
    !global_registered && code == KeyCode::KeyI && modifiers == ModifiersState::ALT
}

fn local_navigation_shortcut(
    code: KeyCode,
    modifiers: ModifiersState,
    global_registered: bool,
    pointer_available: bool,
) -> bool {
    pointer_available
        && !global_registered
        && code == KeyCode::KeyM
        && modifiers == ModifiersState::ALT
}

fn layout_size(show: bool, sprite_scale: f32, physical_scale: f64) -> (u32, u32) {
    let dimensions = bubble::dimensions(show, sprite_scale);
    (
        (f64::from(dimensions.0) * physical_scale).round().max(1.0) as u32,
        (f64::from(dimensions.1) * physical_scale).round().max(1.0) as u32,
    )
}

fn resized_position(
    origin: PhysicalPosition<i32>,
    old: ((u32, u32), bool, f32),
    new: ((u32, u32), bool, f32),
) -> PhysicalPosition<i32> {
    // Integer physical raster anchors avoid cumulative rounding drift. The feet
    // stay planted when either sprite scale or bubble visibility changes.
    let anchor = |(size, show, scale)| {
        let (x0, _, x1, y1) = bubble::SpriteProjection::new(size, show, scale).bounds;
        (((x0 + x1) as f64 / 2.0).round(), y1 as f64)
    };
    let old = anchor(old);
    let new = anchor(new);
    PhysicalPosition::new(
        (f64::from(origin.x) + old.0 - new.0).round() as i32,
        (f64::from(origin.y) + old.1 - new.1).round() as i32,
    )
}

#[cfg(test)]
fn anchored_position(
    origin: PhysicalPosition<i32>,
    old_show: bool,
    new_show: bool,
    sprite_scale: f32,
    physical_scale: f64,
) -> PhysicalPosition<i32> {
    resized_position(
        origin,
        (
            layout_size(old_show, sprite_scale, physical_scale),
            old_show,
            sprite_scale,
        ),
        (
            layout_size(new_show, sprite_scale, physical_scale),
            new_show,
            sprite_scale,
        ),
    )
}

fn clamp_to_monitor(
    position: PhysicalPosition<i32>,
    origin: PhysicalPosition<i32>,
    size: (u32, u32),
    window: (f64, f64),
) -> PhysicalPosition<i32> {
    let max_x = f64::from(origin.x) + (f64::from(size.0) - window.0).max(0.0);
    let max_y = f64::from(origin.y) + (f64::from(size.1) - window.1).max(0.0);
    PhysicalPosition::new(
        f64::from(position.x)
            .clamp(f64::from(origin.x), max_x)
            .round() as i32,
        f64::from(position.y)
            .clamp(f64::from(origin.y), max_y)
            .round() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_resolution_is_lazy_and_preserves_unavailable_reason() {
        let mut options = options();
        options.no_preferences = true;
        options.preferences_file = Some(PathBuf::from("/unused/preferences.json"));
        assert_eq!(
            resolve_preferences_path(&options, || panic!("disabled resolver")),
            (None, None)
        );
        options.no_preferences = false;
        assert_eq!(
            resolve_preferences_path(&options, || panic!("override resolver")),
            (options.preferences_file.clone(), None)
        );
        options.preferences_file = None;
        for (config, home, reason) in [
            (None, None, preferences::Reason::NoHome),
            (
                Some("relative"),
                Some("/unused"),
                preferences::Reason::RelativePath,
            ),
            (None, Some("relative"), preferences::Reason::RelativePath),
        ] {
            let (path, failure) = resolve_preferences_path(&options, || {
                preferences::default_path(config.map(Into::into), home.map(Into::into))
            });
            assert!(path.is_none());
            assert_eq!(failure, Some(reason));
            let status = preferences::Status::unavailable(failure.unwrap());
            assert_eq!(status.reason, Some(reason));
            assert!(status.valid());
        }
    }

    #[test]
    fn forget_menu_freezes_catalog_binding_across_same_view_reattachment() {
        let mut view = test_view();
        let a = preference(1);
        view.set_preference(Some(a.clone()));
        let frozen = view.preference.as_ref().unwrap().binding.clone();
        assert!(view.validated_forget_binding(Some(&frozen)).is_some());
        let mut b = preference(2);
        b.binding.target.activity = "B".into();
        // Same FamiliarView/native identity, but Attach changes its binding.
        view.set_preference(Some(b));
        assert!(view.validated_forget_binding(Some(&frozen)).is_none());
        assert!(view.validated_forget_binding(None).is_none());
        let mut a_again = a;
        a_again.binding.generation = 3;
        view.set_preference(Some(a_again));
        assert!(view.validated_forget_binding(Some(&frozen)).is_none());
    }

    #[test]
    fn forget_epoch_snapshot_clears_all_shared_view_intent_without_resampling() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        let mut a = familiar("a");
        let mut b = familiar("b");
        a.preference = Some(preference(1));
        let mut other_instance = preference(1);
        other_instance.binding.target.instance = "other-instance".into();
        b.preference = Some(other_instance);
        app.stage_snapshot(vec![a.clone(), b.clone()]);
        let now = Instant::now();
        for view in app.views.values_mut() {
            view.gesture.press(1.0);
            view.drag_binding = Some(view.preference.as_ref().unwrap().binding.clone());
            view.commit_drag();
            view.capture_placement_with(now, || Some(Placement { x: 10.0, y: 20.0 }));
            // A fresh in-progress drag must be invalidated as well as the queued one.
            view.drag_binding = Some(view.preference.as_ref().unwrap().binding.clone());
        }
        for snapshot in [&mut a, &mut b] {
            let p = snapshot.preference.as_mut().unwrap();
            p.binding.generation += 1;
            p.placement = None;
        }
        app.stage_snapshot(vec![a, b]);
        for view in app.views.values_mut() {
            assert!(view.pending_placement.is_none());
            assert!(view.drag_binding.is_none());
            assert!(view.pending_restore.is_none());
            view.capture_placement_with(now, || panic!("forgotten gesture must not resample"));
            view.retry_placement_with(now, |_| panic!("forgotten record must not resurrect"));
        }
    }

    #[test]
    fn restore_feedback_duplicates_and_zero_geometry_do_not_spend_corrections() {
        let now = Instant::now();
        let mut restore = PendingRestore {
            placement: Placement { x: 100.0, y: 200.0 },
            attempts: 0,
            feedback: None,
            deadline: now + Duration::from_secs(1),
            poll_at: now,
        };
        let first = ((200, 300), 1.0_f64.to_bits());
        let dpi = ((400, 600), 2.0_f64.to_bits());
        let origin = Some(Point { x: 50.0, y: 70.0 });
        assert_eq!(restore.correction(now, ((0, 0), first.1), None), None);
        assert_eq!(restore.attempts, 0);
        assert_eq!(
            restore.correction(now + Duration::from_millis(20), first, origin),
            origin
        );
        for i in 0..100 {
            assert!(
                restore
                    .correction(now + Duration::from_millis(21 + i * 4), first, origin)
                    .is_none()
            );
        }
        assert_eq!(restore.attempts, 1);
        assert_eq!(
            restore.correction(now + Duration::from_millis(500), dpi, origin),
            origin
        );
        assert_eq!(restore.attempts, 2);
        assert!(
            restore
                .correction(now + Duration::from_secs(1), first, origin)
                .is_none()
        );
        assert!(restore.finished(now + Duration::from_secs(1)));
        let mut view = test_view();
        view.pending_restore = Some(restore);
        view.restore_placement(now + Duration::from_secs(1));
        assert!(view.pending_restore.is_none());
        assert!(!view.geometry_held());
    }

    #[test]
    fn restore_correction_budget_counts_only_fresh_valid_repositions() {
        let now = Instant::now();
        let mut restore = PendingRestore {
            placement: Placement { x: 100.0, y: 200.0 },
            attempts: 0,
            feedback: None,
            deadline: now + Duration::from_secs(1),
            poll_at: now,
        };
        for i in 0..5 {
            assert!(
                restore
                    .correction(
                        now + Duration::from_millis(i * 20),
                        ((200 + i as u32, 300), 1.0_f64.to_bits()),
                        Some(Point::default())
                    )
                    .is_some()
            );
        }
        assert!(restore.finished(now + Duration::from_millis(100)));
        assert!(
            restore
                .correction(
                    now + Duration::from_millis(100),
                    ((500, 300), 1.0_f64.to_bits()),
                    Some(Point::default())
                )
                .is_none()
        );
    }

    #[test]
    fn initial_restore_cancels_on_interaction_navigation_rebind_and_close() {
        for cancellation in 0..4 {
            let mut view = test_view();
            view.set_preference(Some(preference(1)));
            assert!(view.begin_initial_placement());
            let now = Instant::now();
            match cancellation {
                0 => view.gesture.press(1.0),
                1 => view.start_navigation(now),
                2 => view.set_preference(Some(preference(2))),
                _ => view.close_requested = true,
            }
            view.restore_placement(now);
            assert!(view.pending_restore.is_none());
            assert!(view.pending_placement.is_none());
            if cancellation == 1 {
                assert!(view.seeking());
            }
        }
    }

    #[test]
    fn bootstrap_main_first_binding_restores_once_then_attach_preserves_location() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(vec![familiar("main")]);
        // Native creation marks an unbound bootstrap main as eligible.
        app.views.get_mut("main").unwrap().bootstrap_unbound = true;
        let mut snapshot = familiar("main");
        snapshot.preference = Some(preference(1));
        app.stage_snapshot(vec![snapshot.clone()]);
        let view = app.views.get_mut("main").unwrap();
        assert!(view.first_binding_restore);
        assert!(view.begin_initial_placement());
        assert!(view.pending_restore.is_some());
        view.pending_restore = None;
        for p in [Some(preference(2)), None, Some(preference(3))] {
            snapshot.preference = p;
            app.stage_snapshot(vec![snapshot.clone()]);
            let view = &app.views["main"];
            assert!(view.ever_bound);
            assert!(!view.first_binding_restore);
            assert!(view.pending_restore.is_none());
        }
        let mut other = test_view();
        other.set_preference(Some(preference(1)));
        assert!(!other.first_binding_restore);
    }

    #[test]
    fn unbound_main_user_intent_revokes_future_first_binding_restore() {
        for intent in ["drag", "navigation", "menu"] {
            let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
            app.stage_snapshot(vec![familiar("main")]);
            let now = Instant::now();
            let view = app.views.get_mut("main").unwrap();
            view.bootstrap_unbound = true;
            match intent {
                "drag" => {
                    view.gesture.press(1.0);
                    view.commit_drag();
                    view.release_click(now);
                }
                "navigation" => {
                    view.start_navigation(now);
                    view.cancel_navigation("completed before first producer");
                }
                "menu" => {
                    view.menu_open = true;
                    view.menu_request(PhysicalPosition::new(10.0, 10.0));
                    view.finish_menu(now).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(!view.bootstrap_unbound, "{intent}");
            assert!(!view.ever_bound, "user intent must not fabricate a binding");
            assert!(!view.geometry_held());
            assert!(view.navigation.is_none());
            let mut snapshot = familiar("main");
            snapshot.preference = Some(preference(1));
            app.stage_snapshot(vec![snapshot.clone()]);
            let view = &app.views["main"];
            assert!(view.ever_bound);
            assert!(!view.first_binding_restore, "{intent}");
            assert!(view.pending_restore.is_none(), "{intent}");
            snapshot.preference = Some(preference(2));
            app.stage_snapshot(vec![snapshot]);
            assert!(!app.views["main"].first_binding_restore);
            assert!(app.views["main"].pending_restore.is_none());
        }
    }

    #[test]
    fn quick_menu_dismissal_cancels_late_restore_feedback_only_for_clicked_view() {
        let mut clicked = test_view();
        let mut other = test_view();
        for view in [&mut clicked, &mut other] {
            view.set_preference(Some(preference(1)));
            assert!(view.begin_initial_placement());
        }
        let now = Instant::now();
        clicked.first_binding_restore = true;
        clicked.menu_open = true;
        clicked.menu_request(PhysicalPosition::new(10.0, 10.0));
        // No tick occurs during native tracking or before dismissal.
        clicked.finish_menu(now).unwrap();
        assert!(!clicked.first_binding_restore);
        assert!(clicked.pending_restore.is_none());
        clicked.restore_placement(now + Duration::from_millis(100));
        assert!(clicked.pending_restore.is_none());
        assert!(clicked.pending_placement.is_none());
        let feedback = other.pending_restore.as_mut().unwrap();
        assert!(
            feedback
                .correction(
                    now + Duration::from_millis(100),
                    ((400, 600), 2.0_f64.to_bits()),
                    Some(Point::default()),
                )
                .is_some()
        );
    }

    fn preference(generation: u64) -> PreferencePresentation {
        PreferencePresentation {
            binding: PreferenceBinding {
                target: crate::activity::FocusTarget {
                    source: "source".into(),
                    instance: "instance".into(),
                    activity: "activity".into(),
                },
                generation,
            },
            placement: Some(Placement {
                x: -350.0,
                y: 240.0,
            }),
            writable: true,
        }
    }

    #[test]
    fn saved_foot_uses_actual_raster_geometry_and_round_trips() {
        let monitor = LogicalMonitor {
            x: -2000.0,
            y: -2000.0,
            width: 4000.0,
            height: 4000.0,
        };
        for show in [false, true] {
            for backing in [1.0, 1.5, 2.0, 3.0] {
                for scale in [0.25, 0.500000001_f64 as f32, 1.0, 2.0] {
                    for inner in [
                        (157, 231),
                        (321, 687),
                        layout_size(show, scale, 2.5 * backing),
                    ] {
                        let origin = Point {
                            x: -700.0,
                            y: -600.0,
                        };
                        let saved =
                            placement_from_geometry(origin, inner, backing, show, scale).unwrap();
                        let (x0, _, x1, y1) =
                            bubble::SpriteProjection::new(inner, show, scale).bounds;
                        assert_eq!(
                            saved.x,
                            origin.x + ((x0 + x1) as f64 / 2.0).round() / backing
                        );
                        assert_eq!(saved.y, origin.y + y1 as f64 / backing);
                        let restored =
                            restored_origin(saved, inner, backing, show, scale, &[monitor])
                                .unwrap();
                        assert!((restored.x - origin.x).abs() < 1e-9);
                        assert!((restored.y - origin.y).abs() < 1e-9);
                    }
                }
            }
        }
    }

    #[test]
    fn restore_uses_each_monitor_scale_and_deterministic_nearest_fallback() {
        let left = logical_monitor(
            PhysicalPosition::new(-2400, -400),
            PhysicalSize::new(2400, 1800),
            2.0,
        )
        .unwrap();
        let right = logical_monitor(
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(1920, 1080),
            1.0,
        )
        .unwrap();
        assert_eq!(
            (left.x, left.y, left.width, left.height),
            (-1200.0, -200.0, 1200.0, 900.0)
        );
        let saved = Placement {
            x: -800.0,
            y: 300.0,
        };
        let restored = restored_origin(saved, (200, 200), 2.0, false, 0.5, &[right, left]).unwrap();
        assert!(restored.x >= left.x && restored.x < 0.0);
        let removed = restored_origin(saved, (200, 200), 2.0, false, 0.5, &[right]).unwrap();
        assert_eq!(removed.x, 0.0);
        let offscreen = Placement {
            x: 900_000.0,
            y: -900_000.0,
        };
        let a = restored_origin(offscreen, (200, 200), 2.0, true, 0.5, &[right, left]).unwrap();
        let b = restored_origin(offscreen, (200, 200), 2.0, true, 0.5, &[left, right]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.x, 1820.0);
        assert_eq!(a.y, 0.0);
        let oversize = restored_origin(saved, (10000, 10000), 1.0, true, 0.5, &[left]).unwrap();
        assert_eq!(
            oversize,
            Point {
                x: left.x,
                y: left.y
            }
        );
        assert!(restored_origin(saved, (0, 200), 2.0, false, 0.5, &[left]).is_none());
        assert!(restored_origin(saved, (200, 200), 2.0, false, 0.5, &[]).is_none());
        for backing in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                logical_monitor(
                    PhysicalPosition::new(0, 0),
                    PhysicalSize::new(100, 100),
                    backing
                )
                .is_none()
            );
            assert!(restored_origin(saved, (200, 200), backing, false, 0.5, &[left]).is_none());
        }
        for x in [f64::NAN, f64::INFINITY, 2_000_000.0, -2_000_000.0] {
            assert!(
                placement_from_geometry(Point { x, y: 0.0 }, (200, 200), 2.0, false, 0.5).is_none()
            );
        }
    }

    #[test]
    fn drag_capture_is_once_and_retries_frozen_intent_without_resampling() {
        let mut view = test_view();
        let now = Instant::now();
        let pref = preference(1);
        view.set_preference(Some(pref.clone()));
        view.drag_binding = Some(pref.binding.clone());
        view.gesture.press(2.0);
        view.commit_drag();
        let saved = Placement {
            x: -123.0,
            y: 456.0,
        };
        view.capture_placement_with(now, || Some(saved));
        view.capture_placement_with(now, || panic!("second release must not sample geometry"));
        view.poll_interaction(now, false);
        view.release_click(now);
        assert_eq!(view.placement_sequence, 1);
        assert_eq!(view.pending_placement.as_ref().unwrap().placement, saved);
        let retry = view
            .retry_placement_with(now, |p| {
                assert_eq!(p.sequence, 1);
                assert_eq!(p.placement, saved);
                anyhow::bail!("queue full")
            })
            .unwrap();
        assert_eq!(retry, now + Duration::from_millis(100));
        view.start_navigation(now);
        view.apply_settings(FamiliarSettings {
            sprite_scale: 2.0,
            ..view.settings
        })
        .unwrap();
        view.retry_placement_with(now, |_| panic!("retry must respect bounded wake"));
        assert!(
            view.retry_placement_with(retry, |p| {
                assert_eq!(p.placement, saved);
                assert_eq!(p.binding, pref.binding);
                Ok(())
            })
            .is_none()
        );
        assert!(view.pending_placement.is_none());
        view.retry_placement_with(retry, |_| panic!("accepted placement must not repeat"));
    }

    #[test]
    fn clicks_failed_drags_and_binding_changes_never_save() {
        for native_drag in [false, true] {
            let mut view = test_view();
            let pref = preference(1);
            view.set_preference(Some(pref.clone()));
            view.drag_binding = Some(pref.binding.clone());
            view.gesture.press(1.0);
            view.gesture.native_drag_active = native_drag;
            if native_drag {
                view.set_preference(Some(preference(2)));
            }
            view.capture_placement_with(Instant::now(), || {
                panic!("unsavable gesture sampled geometry")
            });
            assert!(view.pending_placement.is_none());
        }
        for change in 0..4 {
            let mut view = test_view();
            let pref = preference(1);
            view.set_preference(Some(pref.clone()));
            view.drag_binding = Some(pref.binding.clone());
            view.gesture.press(1.0);
            view.commit_drag();
            view.capture_placement_with(Instant::now(), || pref.placement);
            let mut changed = pref.clone();
            match change {
                0 => changed.binding.target.source = "other".into(),
                1 => changed.binding.target.instance = "other".into(),
                2 => changed.binding.target.activity = "other".into(),
                _ => changed.binding.generation += 1,
            }
            view.set_preference(Some(changed));
            assert!(view.pending_placement.is_none());
            assert!(view.drag_binding.is_none());
        }
    }

    #[test]
    fn snapshot_echoes_and_attach_never_schedule_a_restore_or_save() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        let mut snapshot = familiar("main");
        snapshot.preference = Some(preference(1));
        app.stage_snapshot(vec![snapshot.clone()]);
        for generation in [1, 1, 2, 2] {
            snapshot.preference = Some(preference(generation));
            app.stage_snapshot(vec![snapshot.clone()]);
            let view = &app.views["main"];
            assert!(view.pending_restore.is_none());
            assert!(view.pending_placement.is_none());
            assert_eq!(view.placement_sequence, 0);
        }
    }

    #[test]
    fn loaded_initial_placement_wins_cascade_even_when_read_only() {
        let mut view = test_view();
        assert!(!view.begin_initial_placement());
        let mut pref = preference(1);
        pref.writable = false;
        view.set_preference(Some(pref.clone()));
        // This decision is used only by the newly-created native-window branch,
        // before it can choose cascade. Read-only loading still restores.
        assert!(view.begin_initial_placement());
        assert_eq!(
            view.pending_restore.as_ref().unwrap().placement,
            pref.placement.unwrap()
        );
        assert_eq!(view.pending_restore.as_ref().unwrap().attempts, 0);
        view.pending_restore = None;
        view.set_preference(Some(pref));
        assert!(view.pending_restore.is_none());
        assert!(view.pending_placement.is_none());
    }

    #[test]
    fn latest_completed_drag_replaces_pending_and_binding_revocation_discards_it() {
        let mut view = test_view();
        let now = Instant::now();
        let pref = preference(1);
        view.set_preference(Some(pref.clone()));
        for (sequence, x) in [(1, 10.0), (2, 20.0)] {
            view.gesture.press(1.0);
            view.drag_binding = Some(pref.binding.clone());
            view.commit_drag();
            view.capture_placement_with(now, || Some(Placement { x, y: 30.0 }));
            view.end_interaction(now);
            assert_eq!(view.pending_placement.as_ref().unwrap().sequence, sequence);
            assert_eq!(view.pending_placement.as_ref().unwrap().placement.x, x);
        }
        view.retry_placement_with(now, |pending| {
            assert_eq!(pending.sequence, 2);
            assert_eq!(pending.placement.x, 20.0);
            anyhow::bail!("queue full")
        });
        let mut readonly = pref;
        readonly.writable = false;
        view.set_preference(Some(readonly));
        assert!(view.pending_placement.is_none());
        assert!(view.placement_retry.is_none());
        view.retry_placement_with(now + Duration::from_secs(1), |_| {
            panic!("revoked intent must not submit")
        });
    }

    #[test]
    fn click_release_slop_and_drag_are_dpi_independent() {
        for scale in [1.0, 1.5, 2.0, 3.0] {
            let mut gesture = ClickGesture::default();
            assert_eq!(gesture.release(), None);
            gesture.press(scale);
            assert_eq!(gesture.release(), None);
            let origin = PhysicalPosition::new(20.0, 30.0);
            assert!(!gesture.moved(origin, true));
            gesture.press(scale);
            let nearby = PhysicalPosition::new(origin.x + 2.9 * scale, origin.y + 4.0 * scale);
            assert!(!gesture.moved(nearby, true));
            assert_eq!(gesture.release(), Some(nearby));
            assert_eq!(gesture.release(), None);
            // Repeated clicks need no intervening cursor event.
            gesture.press(scale);
            assert_eq!(gesture.release(), Some(nearby));
            gesture.press(scale);
            assert!(gesture.moved(
                PhysicalPosition::new(nearby.x + 5.1 * scale, nearby.y),
                true
            ));
            assert!(gesture.pressed.is_none());
            assert_eq!(gesture.release(), None);
            assert!(!gesture.moved(origin, true));
            assert_eq!(gesture.release(), None);
        }
        let mut exact = ClickGesture::default();
        exact.moved(PhysicalPosition::new(0.0, 0.0), true);
        exact.press(2.0);
        assert!(exact.moved(PhysicalPosition::new(6.0, 8.0), true));
        assert_eq!(exact.release(), None);
    }

    #[test]
    fn focus_loss_cursor_leave_and_invalid_input_cannot_be_clicks() {
        for _reason in [
            "focus loss",
            "cursor leave",
            "view deletion",
            "view failure",
        ] {
            let mut gesture = ClickGesture::default();
            gesture.moved(PhysicalPosition::new(10.0, 10.0), true);
            gesture.press(2.0);
            gesture.clear();
            assert_eq!(gesture.release(), None);
            gesture.press(2.0);
            assert!(gesture.pressed.is_none());
        }
        let mut gesture = ClickGesture::default();
        gesture.moved(PhysicalPosition::new(0.0, 0.0), true);
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            gesture.press(scale);
            assert_eq!(gesture.release(), None);
        }
        gesture.press(1.0);
        assert!(!gesture.moved(PhysicalPosition::new(f64::NAN, 0.0), true));
        assert_eq!(gesture.release(), None);
    }

    #[test]
    fn physical_mouse_up_poll_preserves_click_until_queued_release() {
        let mut view = test_view();
        let now = Instant::now();
        let anchor = PhysicalPosition::new(10.0, 10.0);
        view.gesture.moved(anchor, true);
        view.gesture.press(1.0);
        view.poll_interaction(now, false);
        assert!(view.gesture.held);
        assert!(view.gesture.click_eligible);
        assert!(!view.menu_open);
        assert_eq!(view.release_click(now), Some(anchor));
        assert!(view.menu_open);
        assert!(!view.gesture.held);
    }

    #[test]
    fn mouse_up_position_crossing_slop_never_drags_or_opens_menu() {
        let mut view = test_view();
        let now = Instant::now();
        view.start_navigation(now);
        view.hidden_position = Some(PhysicalPosition::new(42, 51));
        view.gesture.moved(PhysicalPosition::new(10.0, 10.0), true);
        view.gesture.press(1.0);
        // Winit may deliver the mouse-up coordinate before MouseInput::Released.
        assert!(!view.gesture.moved(PhysicalPosition::new(16.0, 10.0), false));
        assert!(!view.gesture.click_eligible);
        assert!(!view.gesture.native_drag_active);
        assert_eq!(view.release_click(now), None);
        assert!(!view.menu_open);
        assert!(view.seeking());
        assert_eq!(view.hidden_position, Some(PhysicalPosition::new(42, 51)));
    }

    #[test]
    fn edge_leave_and_focus_loss_cancel_click_but_keep_drag_origin() {
        for focus_lost in [false, true] {
            let mut gesture = ClickGesture::default();
            gesture.moved(PhysicalPosition::new(1.0, 10.0), true);
            gesture.press(1.0);
            assert!(!gesture.moved(PhysicalPosition::new(-1.0, 10.0), true));
            gesture.cancel_click();
            if focus_lost {
                gesture.cancel_click();
            }
            assert!(gesture.held);
            assert!(gesture.pressed.is_some());
            assert!(gesture.moved(PhysicalPosition::new(-5.0, 10.0), true));
            assert_eq!(gesture.release(), None);
        }
    }

    #[test]
    fn native_drag_holds_layout_until_polled_release_without_release_event() {
        let mut view = test_view();
        let now = Instant::now();
        view.start_navigation(now);
        view.hidden_position = Some(PhysicalPosition::new(42, 51));
        view.gesture.moved(PhysicalPosition::new(10.0, 10.0), true);
        view.gesture.press(1.0);
        assert!(view.gesture.moved(PhysicalPosition::new(16.0, 10.0), true));
        // The native adapter has accepted the original-down token and returned.
        view.commit_drag();
        assert!(view.navigation.is_none());
        assert_eq!(view.hidden_position, None);
        assert!(view.gesture.native_drag_active);
        assert!(view.gesture.pressed.is_none());
        view.gesture.cancel_click();
        assert!(view.geometry_held());
        let old = familiar_settings(&view.options);
        let updated = FamiliarSettings {
            sprite_scale: 1.0,
            font_size: 20.0,
            ..old
        };
        view.apply_settings(updated).unwrap();
        assert!(view.visibility.reveal(now));
        view.poll_interaction(now, true);
        view.flush_geometry(now).unwrap();
        assert_eq!(familiar_settings(&view.options), old);
        assert!(!view.bubble_visible);
        assert_eq!(view.settings, updated);
        view.poll_interaction(now + Duration::from_millis(20), false);
        assert!(!view.geometry_held());
        assert!(!view.gesture.native_drag_active);
        view.flush_geometry(now).unwrap();
        assert_eq!(familiar_settings(&view.options), updated);
        assert!(view.bubble_visible);
        assert_eq!(view.release_click(now), None);
    }

    #[test]
    fn menu_keeps_release_coordinates_valid_across_newest_snapshot_resize() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        let mut snapshot = familiar("main");
        snapshot.settings.sprite_scale = 2.0;
        app.stage_snapshot(vec![snapshot.clone()]);
        let now = Instant::now();
        let view = app.views.get_mut("main").unwrap();
        let old = familiar_settings(&view.options);
        let old_size = layout_size(false, view.options.sprite_scale, 1.0);
        let anchor = PhysicalPosition::new(f64::from(old_size.0 - 1), f64::from(old_size.1 - 1));
        view.gesture.moved(anchor, true);
        view.gesture.press(1.0);
        snapshot.settings.sprite_scale = 0.25;
        view.apply_settings(snapshot.settings).unwrap();
        assert_eq!(view.release_click(now), Some(anchor));
        assert!(view.menu_open);
        assert!(!view.gesture.held);
        // The root drains a newer complete snapshot before native tracking.
        snapshot.settings.font_size = 24.0;
        app.stage_snapshot(vec![snapshot.clone()]);
        let view = app.views.get_mut("main").unwrap();
        view.flush_geometry(now).unwrap();
        assert_eq!(view.settings, snapshot.settings);
        assert_eq!(familiar_settings(&view.options), old);
        assert_eq!(layout_size(false, view.options.sprite_scale, 1.0), old_size);
        // A post-tracking snapshot also stays deferred until finish_menu.
        snapshot.settings.bubble_duration = 9.0;
        app.stage_snapshot(vec![snapshot.clone()]);
        let view = app.views.get_mut("main").unwrap();
        assert_eq!(familiar_settings(&view.options), old);
        view.finish_menu(now).unwrap();
        assert_eq!(familiar_settings(&view.options), snapshot.settings);
        let new_size = layout_size(false, view.options.sprite_scale, 1.0);
        assert!(anchor.x > f64::from(new_size.0));
        assert!(anchor.y > f64::from(new_size.1));
    }

    #[test]
    fn menu_retains_exact_custom_settings_before_and_after_render_conversion() {
        let mut view = test_view();
        view.menu_open = true;
        let canonical = FamiliarSettings {
            sprite_scale: 0.500000001,
            ..view.settings
        };
        view.apply_settings(canonical).unwrap();
        assert_eq!(view.deferred_settings, Some(canonical));
        for tracking in [true, false] {
            if !tracking {
                view.finish_menu(Instant::now()).unwrap();
            }
            assert_eq!(view.options.sprite_scale, 0.5);
            assert_eq!(view.settings, canonical);
            let catalog = context_menu::entries(context_menu::Context {
                paused: view.paused,
                bubbles: true,
                copy_session_id: false,
                open_chat: false,
                navigation: false,
                settings: Some(view.settings),
                preferences: false,
                attachments: None,
                open_familiar: None,
            });
            let context_menu::Entry::Submenu {
                entries: fields, ..
            } = &catalog[5]
            else {
                panic!("settings submenu missing")
            };
            let context_menu::Entry::Submenu {
                entries: values, ..
            } = &fields[0]
            else {
                panic!("sprite settings missing")
            };
            assert_eq!(values.len(), 5);
            assert!(
                values[..4]
                    .iter()
                    .all(|entry| matches!(entry, context_menu::Entry::Item { checked: false, .. }))
            );
            assert!(
                matches!(&values[4], context_menu::Entry::Item { label, checked: true, action: None, .. } if label == "Custom (0.500000001)")
            );
        }
    }

    #[test]
    fn menu_actions_fail_closed_but_open_and_quit_are_service_wide() {
        let current = WindowId::from(1);
        let stale = WindowId::from(2);
        for (window, closing, failed) in [
            (None, false, false),
            (Some(stale), false, false),
            (Some(current), true, false),
            (Some(current), false, true),
        ] {
            let valid = menu_target_valid(current, window, closing, failed);
            assert!(!valid);
            for action in [
                Action::Attach(0),
                Action::CloseFamiliar,
                Action::RevealStatus,
                Action::CopySessionId,
                Action::OpenChat,
                Action::GoToMouse,
                Action::SetPaused(true),
                Action::Settings(crate::protocol::SettingsPatch {
                    font_size: Some(16.0),
                    ..Default::default()
                }),
            ] {
                assert_eq!(validated_menu_action(Some(action), valid), None);
            }
            assert_eq!(
                validated_menu_action(Some(Action::QuitNumen), valid),
                Some(Action::QuitNumen)
            );
            assert_eq!(
                validated_menu_action(Some(Action::OpenFamiliar), valid),
                Some(Action::OpenFamiliar)
            );
        }
        assert!(menu_target_valid(current, Some(current), false, false));
        assert_eq!(
            validated_menu_action(Some(Action::CloseFamiliar), true),
            Some(Action::CloseFamiliar)
        );
        assert_eq!(validated_menu_action(None, true), None);
    }

    #[test]
    fn copy_session_id_uses_exact_frozen_attachment_and_generation() {
        let original = AttachmentBinding {
            target: Some(FocusTarget {
                source: "djinn".into(),
                instance: "backend-1".into(),
                activity: "session-with-an-exact-long-id-1234567890".into(),
            }),
            generation: 7,
        };
        assert_eq!(
            menu_session_id(Some(&original), &original),
            Some("session-with-an-exact-long-id-1234567890")
        );
        assert_eq!(menu_session_id(None, &original), None);
        let unbound = AttachmentBinding {
            target: None,
            generation: 7,
        };
        assert_eq!(menu_session_id(Some(&unbound), &unbound), None);
        for change in 0..5 {
            let mut applied = original.clone();
            match change {
                0 => applied.target.as_mut().unwrap().source = "other".into(),
                1 => applied.target.as_mut().unwrap().instance = "backend-2".into(),
                2 => applied.target.as_mut().unwrap().activity = "other-session".into(),
                3 => applied.generation += 1, // Includes detach/reattach to the same chat.
                _ => applied.target = None,
            }
            assert_eq!(menu_session_id(Some(&original), &applied), None);
        }
        // Availability and titles are absent from the attachment identity: a
        // disconnected pin can still be copied without a live discovery list.
        assert_eq!(
            menu_session_id(Some(&original), &original).unwrap(),
            original.target.as_ref().unwrap().activity
        );
    }

    #[test]
    fn open_chat_requires_current_live_djinn_choice_not_another_available_chat() {
        let target = FocusTarget {
            source: "djinn".into(),
            instance: "backend-1".into(),
            activity: "session-1".into(),
        };
        let applied = AttachmentBinding {
            target: Some(target.clone()),
            generation: 3,
        };
        let mut menu = MenuAttachment {
            binding: RuntimeBinding {
                service_instance: "service-1".into(),
                attachment: applied.clone(),
            },
            choices: vec![ChatChoice {
                target,
                incarnation: 1,
                summary: None,
            }],
        };
        assert_eq!(menu.open_chat_index(&applied), Some(0));
        let mut changed = applied.clone();
        changed.generation += 1;
        assert_eq!(menu.open_chat_index(&changed), None);
        menu.choices[0].target.activity = "session-2".into();
        assert_eq!(menu.open_chat_index(&applied), None);
        menu.choices.clear(); // A disconnected pin remains copyable, not openable.
        assert_eq!(menu.open_chat_index(&applied), None);
        assert!(menu_session_id(Some(&applied), &applied).is_some());
        let mut other = applied.clone();
        other.target.as_mut().unwrap().source = "other-app".into();
        menu.binding.attachment = other.clone();
        menu.choices.push(ChatChoice {
            target: other.target.clone().unwrap(),
            incarnation: 1,
            summary: None,
        });
        assert_eq!(menu.open_chat_index(&other), None);
        let unbound = unbound_attachment();
        menu.binding.attachment = unbound.clone();
        assert_eq!(menu.open_chat_index(&unbound), None);
    }

    fn target_snapshot(binding: AttachmentBinding) -> FamiliarTargetSnapshot {
        FamiliarTargetSnapshot {
            binding: RuntimeBinding {
                service_instance: "service-1".into(),
                attachment: binding,
            },
            targets: (0..3)
                .map(|index| crate::activity::LiveTarget {
                    selection: crate::activity::Selection {
                        source: "source".into(),
                        instance: format!("instance-{index}"),
                        activity: "chat".into(),
                        state: crate::protocol::ActivityState::Working,
                        attention: false,
                        reason: None,
                        summary: Some("Same title".into()),
                    },
                    claimed_by: match index {
                        0 => None,
                        1 => Some("main".into()),
                        _ => Some("other".into()),
                    },
                    incarnation: index + 1,
                })
                .collect::<Vec<_>>()
                .into(),
        }
    }

    #[test]
    fn chat_snapshot_requires_applied_binding_filters_ownership_and_freezes_indices() {
        let applied = unbound_attachment();
        let snapshot = target_snapshot(applied.clone());
        let (context, frozen) = attachment_context("main", &applied, Some(snapshot.clone()));
        assert!(context.available);
        assert_eq!(context.choices.len(), 2);
        assert_eq!(context.choices[0].target.instance, "instance-0");
        assert_eq!(context.choices[1].target.instance, "instance-1");
        let frozen = frozen.unwrap();
        let chosen = frozen.choice(0, &applied).unwrap();
        let mut reordered = snapshot.clone();
        let mut targets = reordered.targets.to_vec();
        targets.reverse();
        reordered.targets = targets.into();
        assert!(frozen.still_eligible("main", chosen, &reordered));
        assert_eq!(
            frozen.choice(0, &applied).unwrap().target.instance,
            "instance-0"
        );
        assert!(frozen.choice(usize::MAX, &applied).is_none());
        assert!(frozen.choice(2, &applied).is_none());
        let changed = AttachmentBinding {
            generation: 3,
            ..applied.clone()
        };
        assert!(frozen.choice(0, &changed).is_none());
        let (unavailable, frozen) = attachment_context("main", &changed, Some(snapshot));
        assert!(!unavailable.available);
        assert!(unavailable.choices.is_empty());
        assert!(frozen.is_none());
        let (busy, frozen) = attachment_context("main", &applied, None);
        assert!(!busy.available);
        assert!(frozen.is_none());
    }

    #[test]
    fn post_popup_validation_rejects_reconnect_claim_disappear_service_and_generation_changes() {
        let applied = unbound_attachment();
        let snapshot = target_snapshot(applied.clone());
        let (_, frozen) = attachment_context("main", &applied, Some(snapshot.clone()));
        let frozen = frozen.unwrap();
        let chosen = frozen.choice(0, &applied).unwrap();
        for change in 0..5 {
            let mut fresh = snapshot.clone();
            let mut targets = fresh.targets.to_vec();
            match change {
                0 => targets[0].incarnation += 1,
                1 => targets[0].claimed_by = Some("other".into()),
                2 => {
                    targets.remove(0);
                }
                3 => fresh.binding.service_instance = "restarted".into(),
                _ => fresh.binding.attachment.generation += 1,
            }
            fresh.targets = targets.into();
            assert!(!frozen.still_eligible("main", chosen, &fresh));
        }
        assert!(frozen.still_eligible("main", chosen, &snapshot));
        // A current/self-owned chat remains selectable.
        assert!(frozen.still_eligible("main", frozen.choice(1, &applied).unwrap(), &snapshot));
    }

    #[test]
    fn attachment_queue_uses_frozen_payload_without_optimistic_view_changes() {
        let mut view = test_view();
        view.paused = true;
        view.display.manual(Animation::Waving);
        view.select(Animation::Waving);
        view.navigation = Some(Navigation::Celebrating);
        view.hidden_position = Some(PhysicalPosition::new(321, 654));
        let settings = view.settings;
        let binding = view.runtime_binding.clone();
        let snapshot = target_snapshot(binding.clone());
        let (_, frozen) = attachment_context("main", &binding, Some(snapshot.clone()));
        let frozen = frozen.unwrap();
        for rejected in [false, true] {
            let mut queued = None;
            let result = frozen.queue(
                "main",
                1,
                &view.runtime_binding,
                None,
                |expected, choice| {
                    queued = Some((expected.clone(), choice.clone()));
                    if rejected {
                        anyhow::bail!("queue full")
                    }
                    Ok(())
                },
            );
            assert_eq!(result.is_err(), rejected);
            let (expected, choice) = queued.unwrap();
            assert_eq!(expected, snapshot.binding);
            assert_eq!(choice.target.instance, "instance-1");
            assert_eq!(choice.incarnation, 2);
            assert_eq!(view.runtime_binding, binding);
            assert_eq!(view.settings, settings);
            assert!(view.paused);
            assert_eq!(view.player.animation(), Animation::Waving);
            assert!(matches!(view.navigation, Some(Navigation::Celebrating)));
            assert_eq!(view.hidden_position, Some(PhysicalPosition::new(321, 654)));
        }
        for index in [2, usize::MAX] {
            assert!(
                frozen
                    .queue("main", index, &binding, None, |_, _| panic!(
                        "invalid index queued"
                    ))
                    .is_err()
            );
        }
        let stale = AttachmentBinding {
            generation: binding.generation + 2,
            ..binding
        };
        assert!(
            frozen
                .queue("main", 0, &stale, None, |_, _| panic!(
                    "stale binding queued"
                ))
                .is_err()
        );
    }

    #[test]
    fn committed_retarget_updates_source_without_resetting_manual_navigation_or_appearance() {
        use crate::protocol::{Activity, ActivityState, AttachRequest, TargetRequest};

        for manual in [false, true] {
            for navigating in [false, true] {
                let now = Instant::now();
                let settings = FamiliarSettings {
                    sprite_scale: 0.75,
                    font_size: 19.0,
                    bubble_duration: 7.0,
                };
                let mut store = crate::activity::Store::with_settings(true, settings);
                for (instance, state) in [
                    ("actor-a", ActivityState::Working),
                    ("actor-b", ActivityState::Waiting),
                ] {
                    store
                        .register(
                            ("source".into(), instance.into()),
                            vec![Activity {
                                activity: "chat".into(),
                                sequence: 1,
                                state,
                                attention: state == ActivityState::Waiting,
                                reason: None,
                                summary: Some("Same title".into()),
                            }],
                            now,
                        )
                        .unwrap();
                }
                let attach = |instance: &str| AttachRequest {
                    id: "main".into(),
                    target: TargetRequest {
                        source: "source".into(),
                        instance: Some(instance.into()),
                        activity: "chat".into(),
                    },
                };
                store.attach(&[attach("actor-a")]).unwrap();
                let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
                for (id, presentation) in app.stage_snapshot(store.presentations(now)) {
                    let view = app.views.get_mut(&id).unwrap();
                    view.latest_view = presentation.clone();
                    view.source_animation(presentation);
                }
                let view = app.views.get_mut("main").unwrap();
                assert_eq!(view.player.animation(), Animation::Working);
                let previous_binding = view.runtime_binding.clone();
                if navigating {
                    view.start_navigation(now);
                    view.select(Animation::RunningLeft);
                }
                if manual {
                    view.display.manual(Animation::Waving);
                    view.select(Animation::Waving);
                }
                view.player.advance(Duration::from_millis(175), &view.atlas);
                view.paused = true;
                view.hidden_position = Some(PhysicalPosition::new(123, 456));
                let animation = view.player.animation();
                let frame = view.player.frame_index();
                let remaining = view.player.until_next_frame(&view.atlas);
                let motion = view.motion_clock.deadline();
                let tick = view.last_tick;
                let demo = view.next_demo;

                store.attach(&[attach("actor-b")]).unwrap();
                let committed = store.presentations(now);
                assert_eq!(committed.len(), 1);
                let expected = committed[0].clone();
                for (id, presentation) in app.stage_snapshot(committed) {
                    let view = app.views.get_mut(&id).unwrap();
                    view.latest_view = presentation.clone();
                    view.source_animation(presentation);
                }
                let view = app.views.get_mut("main").unwrap();
                assert_eq!(view.runtime_binding, expected.runtime_binding);
                assert!(view.runtime_binding.generation > previous_binding.generation);
                let target = view.runtime_binding.target.as_ref().unwrap();
                assert_eq!(
                    (&*target.source, &*target.instance, &*target.activity),
                    ("source", "actor-b", "chat")
                );
                assert_eq!(view.latest_view, expected.presentation);
                assert_eq!(
                    FocusTarget::from(view.latest_view.base.as_ref().unwrap()),
                    *target
                );
                assert!(view.latest_view.cue.is_none());
                assert!(view.paused);
                assert_eq!(view.display.is_manual(), manual);
                assert_eq!(view.seeking(), navigating);
                assert_eq!(view.motion_clock.deadline(), motion);
                assert_eq!(view.settings, settings);
                assert_eq!(familiar_settings(&view.options), settings);
                assert_eq!(view.hidden_position, Some(PhysicalPosition::new(123, 456)));
                if manual || navigating {
                    assert_eq!(view.next_demo, demo);
                    assert_eq!(view.player.animation(), animation);
                    assert_eq!(view.player.frame_index(), frame);
                    assert_eq!(view.player.until_next_frame(&view.atlas), remaining);
                    assert_eq!(view.last_tick, tick);
                } else {
                    assert_eq!(view.player.animation(), Animation::Waiting);
                }
                // Both overrides must retain the newly committed live source for resume.
                assert_eq!(view.display.resume(), Animation::Waiting);
            }
        }
    }

    #[test]
    fn runtime_binding_is_independent_of_preferences_and_does_not_reset_view_state() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        let mut snapshot = familiar("main");
        app.stage_snapshot(vec![snapshot.clone()]);
        let view = app.views.get_mut("main").unwrap();
        view.paused = true;
        view.display.manual(Animation::Waving);
        view.select(Animation::Waving);
        view.hidden_position = Some(PhysicalPosition::new(123, 456));
        view.navigation = Some(Navigation::Celebrating);
        let settings = view.settings;
        let frozen = target_snapshot(view.runtime_binding.clone());
        snapshot.runtime_binding = AttachmentBinding {
            target: Some(FocusTarget {
                source: "source".into(),
                instance: "instance-0".into(),
                activity: "chat".into(),
            }),
            generation: 2,
        };
        app.stage_snapshot(vec![snapshot.clone()]);
        let view = &app.views["main"];
        assert_eq!(view.runtime_binding, snapshot.runtime_binding);
        assert!(view.preference.is_none());
        assert!(view.paused);
        assert_eq!(view.settings, settings);
        assert_eq!(view.player.animation(), Animation::Waving);
        assert_eq!(view.hidden_position, Some(PhysicalPosition::new(123, 456)));
        assert!(matches!(view.navigation, Some(Navigation::Celebrating)));
        assert_ne!(view.runtime_binding, frozen.binding.attachment);
        snapshot.preference = Some(preference(9));
        app.stage_snapshot(vec![snapshot.clone()]);
        assert_eq!(app.views["main"].runtime_binding, snapshot.runtime_binding);
        snapshot.preference = None;
        app.stage_snapshot(vec![snapshot.clone()]);
        assert_eq!(app.views["main"].runtime_binding, snapshot.runtime_binding);
    }

    #[test]
    fn popup_cancels_only_for_target_removal_or_quit_and_resets_between_menus() {
        let popup = PopupLifecycle::default();
        let mailbox = Mailbox::default();
        popup.begin("clicked");
        popup.post(vec![familiar("clicked"), familiar("other")], &mailbox);
        assert!(!popup.cancelled.load(Ordering::Acquire));
        let mut changed = familiar("clicked");
        changed.presentation = source(crate::protocol::ActivityState::Working, Some(7));
        popup.post(vec![changed], &mailbox);
        assert!(!popup.cancelled.load(Ordering::Acquire));
        popup.post(vec![familiar("other")], &mailbox);
        assert!(popup.cancelled.load(Ordering::Acquire));
        popup.end();
        assert!(popup.target.lock().unwrap().is_none());
        popup.begin("other");
        assert!(!popup.cancelled.load(Ordering::Acquire));
        // Same signal written by the server's service-wide quit callback.
        popup.cancelled.store(true, Ordering::Release);
        assert!(popup.cancelled.load(Ordering::Acquire));
        popup.end();
        popup.begin("other");
        assert!(!popup.cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn dismissed_click_preserves_manual_pause_cue_anchor_and_pursuit() {
        let mut view = test_view();
        view.start_navigation(Instant::now());
        view.paused = true;
        view.latest_view = source(crate::protocol::ActivityState::Waiting, Some(19));
        view.display.manual(Animation::Review);
        view.hidden_position = Some(PhysicalPosition::new(42, 51));
        let presentation = view.latest_view.clone();
        let animation = view.player.animation();
        let demo = view.next_demo;
        let visible = view.bubble_visible;
        let settings = view.settings;
        view.gesture.moved(PhysicalPosition::new(10.0, 10.0), true);
        view.gesture.press(2.0);
        assert!(view.release_click(Instant::now()).is_some());
        assert!(view.menu_open);
        view.finish_menu(Instant::now()).unwrap();
        assert_eq!(validated_menu_action(None, true), None);
        assert!(view.seeking());
        assert!(view.paused);
        assert!(view.display.is_manual());
        assert_eq!(view.latest_view, presentation);
        assert_eq!(view.player.animation(), animation);
        assert_eq!(view.next_demo, demo);
        assert_eq!(view.bubble_visible, visible);
        assert_eq!(view.settings, settings);
        assert_eq!(view.hidden_position, Some(PhysicalPosition::new(42, 51)));
        view.set_paused(false);
        assert!(!view.paused);
        assert!(view.seeking());
    }

    #[test]
    fn settings_and_visibility_layout_wait_until_click_anchor_is_released() {
        let mut view = test_view();
        let old = familiar_settings(&view.options);
        view.gesture.moved(PhysicalPosition::new(10.0, 10.0), true);
        view.gesture.press(2.0);
        let updated = FamiliarSettings {
            font_size: 20.0,
            ..old
        };
        view.apply_settings(updated).unwrap();
        assert_eq!(familiar_settings(&view.options), old);
        assert_eq!(view.deferred_settings, Some(updated));
        let visible = view.bubble_visible;
        view.sync_visibility(Instant::now() + Duration::from_secs(60))
            .unwrap();
        assert_eq!(view.bubble_visible, visible);
        view.end_interaction(Instant::now());
        view.apply_settings(view.deferred_settings.unwrap())
            .unwrap();
        assert_eq!(familiar_settings(&view.options), updated);
        assert!(view.deferred_settings.is_none());
    }

    #[test]
    fn service_open_outlives_clicked_view_and_only_commits_new_default_view() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private/open.sock");
        let (send, receive) = std::sync::mpsc::channel();
        let defaults = FamiliarSettings::default();
        let server = Server::start_graphical_with_settings(
            &path,
            move |snapshot| send.send(snapshot).unwrap(),
            |_| {},
            None,
            || {},
            true,
            defaults,
        )
        .unwrap();
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.control = Some(server.familiar_control());
        app.stage_snapshot(receive.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(app.dispatch_service_menu(Action::OpenFamiliar, || panic!("open quit")));
        assert_eq!(app.views.len(), 1, "queueing must not create a view");
        app.stage_snapshot(receive.recv_timeout(Duration::from_secs(2)).unwrap());
        app.control.as_ref().unwrap().close("main").unwrap();
        app.stage_snapshot(receive.recv_timeout(Duration::from_secs(2)).unwrap());
        assert!(!app.views.contains_key("main"));
        let patch = crate::protocol::SettingsPatch {
            font_size: Some(19.0),
            ..Default::default()
        };
        app.control
            .as_ref()
            .unwrap()
            .settings("familiar-1", patch)
            .unwrap();
        app.stage_snapshot(receive.recv_timeout(Duration::from_secs(2)).unwrap());
        let view = app.views.get_mut("familiar-1").unwrap();
        view.paused = true;
        view.display.manual(Animation::Waving);
        view.select(Animation::Waving);
        view.navigation = Some(Navigation::Celebrating);
        view.hidden_position = Some(PhysicalPosition::new(321, 654));
        let settings = view.settings;
        let binding = view.runtime_binding.clone();
        let tick = view.last_tick;
        let selected = validated_menu_action(Some(Action::OpenFamiliar), false).unwrap();
        assert!(app.dispatch_service_menu(selected, || panic!("open quit")));
        assert_eq!(app.views.len(), 1);
        app.stage_snapshot(receive.recv_timeout(Duration::from_secs(2)).unwrap());
        let existing = &app.views["familiar-1"];
        assert_eq!(existing.settings, settings);
        assert_eq!(existing.runtime_binding, binding);
        assert!(existing.paused);
        assert!(existing.display.is_manual());
        assert_eq!(existing.player.animation(), Animation::Waving);
        assert!(matches!(existing.navigation, Some(Navigation::Celebrating)));
        assert_eq!(
            existing.hidden_position,
            Some(PhysicalPosition::new(321, 654))
        );
        assert_eq!(existing.last_tick, tick);
        let new = &app.views["familiar-2"];
        assert_eq!(new.settings, defaults);
        assert_eq!(new.runtime_binding.target, None);
        assert!(new.runtime_binding.generation > 0);
        assert!(new.preference.is_none());
        assert!(!new.paused);
        assert!(!new.display.is_manual());
        assert!(new.navigation.is_none());
        assert!(new.hidden_position.is_none());
        assert_eq!(new.latest_view, Presentation::default());

        // Manual/demo mode has no live control and must neither start a service nor quit.
        app.control = None;
        assert!(app.dispatch_service_menu(Action::OpenFamiliar, || panic!("open quit")));
        assert_eq!(app.views.len(), 2);
        let mut quit = false;
        assert!(app.dispatch_service_menu(Action::QuitNumen, || quit = true));
        assert!(quit);
        assert!(!app.dispatch_service_menu(Action::CloseFamiliar, || panic!("close quit")));
    }

    #[test]
    fn first_manual_attachment_preserves_unbound_state_and_never_loads_saved_destination() {
        use crate::protocol::{Activity, ActivityState, AttachRequest, OpenRequest, TargetRequest};

        for id in ["main", "familiar-1"] {
            for manual in [false, true] {
                for navigating in [false, true] {
                    let now = Instant::now();
                    let settings = FamiliarSettings {
                        font_size: 19.0,
                        ..Default::default()
                    };
                    let saved = FamiliarSettings {
                        sprite_scale: 2.0,
                        font_size: 24.0,
                        bubble_duration: 9.0,
                    };
                    let mut store = crate::activity::Store::with_settings(true, settings);
                    store.set_auto_select_main(false);
                    store.enable_preferences(
                        preferences::Document {
                            version: 1,
                            records: vec![preferences::Record {
                                source: "source".into(),
                                activity: "chat".into(),
                                settings: saved,
                                placement: Some(Placement { x: 900.0, y: 700.0 }),
                            }],
                        },
                        true,
                    );
                    if id != "main" {
                        assert_eq!(
                            store
                                .open(&[OpenRequest {
                                    id: None,
                                    preset: DEFAULT_CHARACTER_ID.into(),
                                    target: None,
                                }])
                                .unwrap(),
                            [id]
                        );
                    }
                    let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
                    app.stage_snapshot(store.presentations(now));
                    let view = app.views.get_mut(id).unwrap();
                    view.bootstrap_unbound = id == "main";
                    view.paused = true;
                    if navigating {
                        view.start_navigation(now);
                    }
                    if manual {
                        view.display.manual(Animation::Waving);
                        view.select(Animation::Waving);
                    }
                    view.hidden_position = Some(PhysicalPosition::new(123, 456));
                    let previous_binding = view.runtime_binding.clone();
                    assert!(previous_binding.target.is_none());
                    assert!(previous_binding.generation > 0);
                    assert!(view.preference.is_none());
                    // A producer appearing after the unbound open must not select it implicitly.
                    store
                        .register(
                            ("source".into(), "actor".into()),
                            vec![Activity {
                                activity: "chat".into(),
                                sequence: 1,
                                state: ActivityState::Waiting,
                                attention: true,
                                reason: None,
                                summary: Some("Latest chat".into()),
                            }],
                            now,
                        )
                        .unwrap();
                    let unbound = store.presentations(now);
                    assert!(unbound.iter().all(|f| f.runtime_binding.target.is_none()));
                    app.stage_snapshot(unbound);
                    store
                        .attach(&[AttachRequest {
                            id: id.into(),
                            target: TargetRequest {
                                source: "source".into(),
                                instance: Some("actor".into()),
                                activity: "chat".into(),
                            },
                        }])
                        .unwrap();
                    for (id, presentation) in app.stage_snapshot(store.presentations(now)) {
                        let view = app.views.get_mut(&id).unwrap();
                        view.latest_view = presentation.clone();
                        view.source_animation(presentation);
                    }
                    let view = &app.views[id];
                    assert!(view.runtime_binding.generation > previous_binding.generation);
                    assert_eq!(
                        view.runtime_binding.target.as_ref().unwrap().instance,
                        "actor"
                    );
                    assert_eq!(view.settings, settings);
                    assert_eq!(familiar_settings(&view.options), settings);
                    let preference = view.preference.as_ref().unwrap();
                    assert!(preference.writable);
                    assert!(preference.placement.is_none());
                    assert!(!view.first_binding_restore);
                    assert!(view.pending_restore.is_none());
                    assert_eq!(view.hidden_position, Some(PhysicalPosition::new(123, 456)));
                    assert!(view.paused);
                    assert_eq!(view.display.is_manual(), manual);
                    assert_eq!(view.seeking(), navigating);
                    let following = view.latest_view.following.as_ref().unwrap();
                    assert_eq!(following.summary.as_deref(), Some("Latest chat"));
                    assert!(following.available);
                    let binding = view.runtime_binding.clone();
                    let animation = view.player.animation();
                    store
                        .apply(
                            &("source".into(), "actor".into()),
                            crate::protocol::Event::State {
                                activity: Activity {
                                    activity: "chat".into(),
                                    sequence: 2,
                                    state: ActivityState::Succeeded,
                                    attention: false,
                                    reason: None,
                                    summary: Some("Completed chat".into()),
                                },
                            },
                            now,
                        )
                        .unwrap();
                    for (id, presentation) in app.stage_snapshot(store.presentations(now)) {
                        let view = app.views.get_mut(&id).unwrap();
                        view.latest_view = presentation.clone();
                        view.source_animation(presentation);
                    }
                    let view = &app.views[id];
                    assert_eq!(view.runtime_binding, binding);
                    assert_eq!(
                        view.latest_view.base.as_ref().unwrap().state,
                        ActivityState::Succeeded
                    );
                    assert_eq!(
                        view.latest_view
                            .following
                            .as_ref()
                            .unwrap()
                            .summary
                            .as_deref(),
                        Some("Completed chat")
                    );
                    assert_eq!(view.display.is_manual(), manual);
                    assert_eq!(view.seeking(), navigating);
                    assert!(view.paused);
                    assert_eq!(view.hidden_position, Some(PhysicalPosition::new(123, 456)));
                    if manual || navigating {
                        assert_eq!(view.player.animation(), animation);
                    }
                    let patch = crate::protocol::SettingsPatch {
                        font_size: Some(20.0),
                        ..Default::default()
                    };
                    store.settings(&[id.into()], patch).unwrap();
                    let (_, document) = store.preference_document().unwrap();
                    assert_eq!(document.records[0].settings.font_size, 20.0);
                }
            }
        }
    }

    #[test]
    fn menu_settings_patch_uses_canonical_snapshot_without_optimistic_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private/menu.sock");
        let (send, receive) = std::sync::mpsc::channel();
        let initial = FamiliarSettings {
            font_size: 13.0,
            bubble_duration: 7.0,
            ..Default::default()
        };
        let server = Server::start_graphical_with_settings(
            &path,
            move |snapshot| {
                send.send(snapshot).unwrap();
            },
            |_| {},
            None,
            || {},
            true,
            initial,
        )
        .unwrap();
        let snapshot = receive.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(snapshot);
        let patch = crate::protocol::SettingsPatch {
            sprite_scale: Some(1.0),
            ..Default::default()
        };
        server.familiar_control().settings("main", patch).unwrap();
        assert_eq!(familiar_settings(&app.views["main"].options), initial);
        let snapshot = receive.recv_timeout(Duration::from_secs(2)).unwrap();
        app.stage_snapshot(snapshot);
        assert_eq!(
            familiar_settings(&app.views["main"].options),
            FamiliarSettings {
                sprite_scale: 1.0,
                ..initial
            }
        );
        drop(server);
    }

    fn test_view() -> FamiliarView {
        FamiliarView::new(
            "test".into(),
            Arc::new(SpriteAtlas::load().unwrap()),
            options(),
        )
    }

    fn source(state: crate::protocol::ActivityState, cue: Option<u64>) -> Presentation {
        let selection = crate::activity::Selection {
            source: "source".into(),
            instance: "one".into(),
            activity: "task".into(),
            state,
            attention: true,
            reason: None,
            summary: None,
        };
        Presentation {
            base: Some(selection.clone()),
            cue: cue.map(|id| crate::activity::Cue { id, selection }),
            following: None,
        }
    }

    fn finish_clip(view: &mut FamiliarView) {
        let total: Duration = view
            .atlas
            .clip(view.player.animation())
            .frame_durations
            .iter()
            .copied()
            .sum();
        view.advance_animation(total);
    }

    #[test]
    fn occlusion_does_not_freeze_pursuit_contact_or_success_completion() {
        let mut view = test_view();
        let now = Instant::now();
        view.options.sprite_scale = 1.0;
        view.bubble_visible = false;
        view.occluded = true;
        view.start_navigation(now);
        view.source_animation(source(crate::protocol::ActivityState::Working, None));
        assert!(view.animation_eligible(true));
        assert!(!view.animation_eligible(false));
        let step = view.plan_pursuit(
            now,
            Point {
                x: 1000.0,
                y: 100.0,
            },
            Point::default(),
            (192, 208),
            1.0,
            Duration::ZERO,
        );
        assert!(matches!(step, PursuitStep::Move { .. }));
        view.apply_pursuit_step(step);
        let frame = view
            .atlas
            .frame(view.player.animation(), view.player.frame_index());
        let index = frame
            .chunks_exact(4)
            .position(|rgba| rgba[3] >= 32)
            .unwrap();
        let cursor = Point {
            x: (index % 192) as f64 + 0.5,
            y: (index / 192) as f64 + 0.5,
        };
        let Some(Navigation::Seeking(nav)) = &mut view.navigation else {
            panic!("seeking")
        };
        let contact = nav.step_raster(
            now,
            cursor,
            SpriteGeometry {
                origin: Point::default(),
                pixel_scale: 1.0,
            },
            &view.raster.as_ref().unwrap().mask,
        );
        assert_eq!(contact, PursuitStep::Arrived);
        view.apply_pursuit_step(contact);
        assert!(view.animation_eligible(true));
        view.paused = true;
        assert!(!view.animation_eligible(true));
        finish_clip(&mut view);
        assert!(matches!(view.navigation, Some(Navigation::Celebrating)));
        view.paused = false;
        finish_clip(&mut view);
        assert!(view.navigation.is_none());
        assert_eq!(view.player.animation(), Animation::Working);
        assert!(!view.animation_eligible(true));
    }

    #[test]
    fn planning_selects_direction_and_frame_before_sampling_and_holds_them_after_move() {
        let mut view = test_view();
        let start = Instant::now();
        view.options.sprite_scale = 1.0;
        view.bubble_visible = false;
        view.start_navigation(start);
        for (tick, cursor, elapsed, animation, frame_index) in [
            (
                0,
                Point {
                    x: 1000.0,
                    y: 100.0,
                },
                Duration::ZERO,
                Animation::RunningRight,
                0,
            ),
            (
                1,
                Point {
                    x: -1000.0,
                    y: 100.0,
                },
                Duration::ZERO,
                Animation::RunningLeft,
                0,
            ),
            (
                2,
                Point {
                    x: -1000.0,
                    y: 100.0,
                },
                view.atlas.clip(Animation::RunningLeft).frame_durations[0],
                Animation::RunningLeft,
                1,
            ),
        ] {
            let now = start + FRAME_INTERVAL * tick;
            let step = view.plan_pursuit(now, cursor, Point::default(), (192, 208), 1.0, elapsed);
            assert_eq!(view.player.animation(), animation);
            assert_eq!(view.player.frame_index(), frame_index);
            let raster = view.raster.as_ref().unwrap();
            assert_eq!(raster.key.animation, animation);
            assert_eq!(raster.key.frame, frame_index);
            let current = view.atlas.frame(animation, frame_index);
            let mut painted = vec![0; 192 * 208 * 4];
            bubble::Renderer::new(None, 12.0).compose(
                &mut painted,
                (192, 208),
                current,
                &view.latest_view,
                None,
                false,
            );
            for (index, pixel) in painted.chunks_exact(4).enumerate() {
                let contact = Navigator::new(40.0, now).step_raster(
                    now,
                    Point {
                        x: (index % 192) as f64 + 0.5,
                        y: (index / 192) as f64 + 0.5,
                    },
                    SpriteGeometry {
                        origin: Point::default(),
                        pixel_scale: 1.0,
                    },
                    &raster.mask,
                );
                assert_eq!(contact == PursuitStep::Arrived, pixel[3] >= 32);
            }
            view.apply_pursuit_step(step);
            // Even a planner heading different from the pose cannot reselect art
            // after that art has been sampled for the sweep.
            view.apply_pursuit_step(PursuitStep::Move {
                delta: Point::default(),
                facing: Facing::Right,
            });
            view.source_animation(source(crate::protocol::ActivityState::Succeeded, Some(99)));
            assert_eq!(view.player.animation(), animation);
            assert_eq!(view.player.frame_index(), frame_index);
        }
        assert_eq!(
            running_facing(Animation::RunningLeft, 50.0, 50.0),
            Facing::Left
        );
        assert_eq!(
            running_facing(Animation::RunningRight, 50.0, 50.0),
            Facing::Right
        );
    }

    #[test]
    fn early_events_do_not_advance_seeking_pose_or_invalidate_the_raster() {
        let mut view = test_view();
        let now = Instant::now();
        view.start_navigation(now);
        let mut root_clock = MotionClock::default();
        assert!(root_clock.take(now).is_some());
        let elapsed = view.motion_clock.take(now).unwrap();
        view.plan_pursuit(
            now,
            Point {
                x: 1000.0,
                y: 1000.0,
            },
            Point::default(),
            (192, 208),
            1.0,
            elapsed,
        );
        let animation = view.player.animation();
        let frame = view.player.frame_index();
        let remaining = view.player.until_next_frame(&view.atlas);
        for index in 1..=100 {
            let early = now + FRAME_INTERVAL * index / 101;
            assert!(root_clock.take(early).is_none());
            assert!(view.motion_clock.take(early).is_none());
            view.source_animation(source(crate::protocol::ActivityState::Working, None));
            assert_eq!(view.player.animation(), animation);
            assert_eq!(view.player.frame_index(), frame);
            assert_eq!(view.player.until_next_frame(&view.atlas), remaining);
            assert_eq!(view.raster.as_ref().unwrap().key.frame, frame);
        }
    }

    #[test]
    fn newly_selected_contact_pose_is_held_before_fresh_feedback_can_celebrate() {
        let mut view = test_view();
        view.options.sprite_scale = 1.0;
        view.bubble_visible = false;
        let start = Instant::now();
        view.start_navigation(start);
        let frame = view.atlas.frame(Animation::RunningRight, 0);
        let index = frame
            .chunks_exact(4)
            .enumerate()
            .find(|(index, rgba)| index % 192 >= 96 && rgba[3] >= 32)
            .unwrap()
            .0;
        let cursor = Point {
            x: (index % 192) as f64 + 0.5,
            y: (index / 192) as f64 + 0.5,
        };
        let step = view.plan_pursuit(
            start,
            cursor,
            Point::default(),
            (192, 208),
            1.0,
            Duration::ZERO,
        );
        assert!(matches!(
            step,
            PursuitStep::Move {
                delta: Point { x: 0.0, y: 0.0 },
                ..
            }
        ));
        view.apply_pursuit_step(step);
        assert!(view.raster.is_some());
        assert_eq!(view.player.animation(), Animation::RunningRight);
        let mut painted = vec![0; 192 * 208 * 4];
        bubble::Renderer::new(None, 12.0).compose(
            &mut painted,
            (192, 208),
            view.atlas
                .frame(view.player.animation(), view.player.frame_index()),
            &view.latest_view,
            None,
            false,
        );
        assert!(painted[index * 4 + 3] >= 32);
        let boundary = view.atlas.clip(Animation::RunningRight).frame_durations[0];
        let step = view.plan_pursuit(
            start + boundary,
            cursor,
            Point::default(),
            (192, 208),
            1.0,
            boundary,
        );
        assert_eq!(step, PursuitStep::Arrived);
        assert_eq!(view.player.animation(), Animation::RunningRight);
        assert_eq!(view.player.frame_index(), 0);
        view.apply_pursuit_step(step);
        assert_eq!(view.player.animation(), Animation::Succeeded);

        // A pointer which leaves between sampling and confirmation must resume
        // live pursuit, not consume a queued success from the old target.
        view.start_navigation(start);
        view.plan_pursuit(
            start,
            cursor,
            Point::default(),
            (192, 208),
            1.0,
            Duration::ZERO,
        );
        let moved = view.plan_pursuit(
            start + FRAME_INTERVAL,
            Point {
                x: 1000.0,
                y: 1000.0,
            },
            Point::default(),
            (192, 208),
            1.0,
            FRAME_INTERVAL,
        );
        assert!(matches!(moved, PursuitStep::Move { .. }));
        assert!(view.seeking());
        assert_ne!(view.player.animation(), Animation::Succeeded);
    }

    #[test]
    fn sprite_geometry_preserves_pet_anchor_at_all_backing_densities() {
        for backing in [1.0, 2.0, 3.0] {
            for sprite_scale in [0.5, 1.0, 2.0] {
                let scene = 1.5;
                let origin = Point {
                    x: -1234.0,
                    y: -580.0,
                };
                let hidden = bubble::dimensions(false, sprite_scale);
                let full = bubble::dimensions(true, sprite_scale);
                let size = |d: (u32, u32)| {
                    (
                        (f64::from(d.0) * scene * backing) as u32,
                        (f64::from(d.1) * scene * backing) as u32,
                    )
                };
                let pet =
                    sprite_geometry(origin, size(hidden), backing, false, sprite_scale).unwrap();
                let expanded_origin = Point {
                    x: origin.x
                        - (f64::from(full.0) - 192.0 * f64::from(sprite_scale)) / 2.0 * scene,
                    y: origin.y - 144.0 * scene,
                };
                let shown =
                    sprite_geometry(expanded_origin, size(full), backing, true, sprite_scale)
                        .unwrap();
                assert_eq!(pet.origin, origin);
                assert_eq!(pet.origin, shown.origin);
                assert_eq!(pet.pixel_scale, shown.pixel_scale);
                assert_eq!(pet.pixel_scale, 1.0 / backing);
            }
        }
    }

    #[test]
    fn sprite_geometry_uses_actual_letterboxed_size_and_rejects_invalid_geometry() {
        let geometry = sprite_geometry(
            Point {
                x: -100.0,
                y: -200.0,
            },
            (400, 208),
            2.0,
            false,
            1.0,
        )
        .unwrap();
        assert_eq!(
            geometry.origin,
            Point {
                x: -48.0,
                y: -200.0
            }
        );
        assert_eq!(geometry.pixel_scale, 0.5);
        assert!(sprite_geometry(Point::default(), (0, 208), 1.0, false, 0.5).is_none());
        assert!(sprite_geometry(Point::default(), (192, 208), f64::NAN, false, 0.5).is_none());
    }

    #[test]
    fn fractional_intent_accumulates_without_per_event_native_pixel_nudges() {
        for backing in [1.0, 2.0, 3.0] {
            let mut motion = NativeMotion::default();
            let mut origin = Point::default();
            assert_eq!(motion.request(origin, Point::default(), backing), origin);
            for index in 1..=100 {
                origin = motion.request(origin, Point { x: 0.01, y: -0.02 }, backing);
                assert!(origin.x <= index as f64 * 0.01 + 1e-12);
                assert!(-origin.y <= index as f64 * 0.02 + 1e-12);
            }
            assert!((origin.x - 1.0).abs() < 1.0 / backing + 1e-12);
            assert!((origin.y + 2.0).abs() < 1.0 / backing + 1e-12);
        }
    }

    #[test]
    fn overlay_ignores_source_cues_and_finishes_at_latest_base_without_replay() {
        use crate::protocol::ActivityState;
        let mut view = test_view();
        view.display.manual(Animation::Waving);
        view.start_navigation(Instant::now());
        assert!(!view.display.is_manual());
        view.prepare_running_pose(Facing::Left, Duration::ZERO);
        view.apply_pursuit_step(PursuitStep::Move {
            delta: Point::default(),
            facing: Facing::Left,
        });
        view.source_animation(source(ActivityState::Succeeded, Some(10)));
        assert_eq!(view.player.animation(), Animation::RunningLeft);
        view.apply_pursuit_step(PursuitStep::Arrived);
        view.advance_animation(Duration::from_millis(1));
        let remaining = view.player.until_next_frame(&view.atlas);
        view.apply_pursuit_step(PursuitStep::Arrived);
        assert_eq!(view.player.until_next_frame(&view.atlas), remaining);
        let latest = source(ActivityState::Waiting, None);
        view.source_animation(latest.clone());
        assert_eq!(view.player.animation(), Animation::Succeeded);
        assert_eq!(view.player.until_next_frame(&view.atlas), remaining);
        finish_clip(&mut view);
        assert!(view.navigation.is_none());
        assert_eq!(view.player.animation(), Animation::Waiting);
        view.source_animation(source(ActivityState::Succeeded, Some(10)));
        assert_eq!(view.player.animation(), Animation::Idle);
    }

    #[test]
    fn same_direction_does_not_reset_running_clip_and_cancellation_never_celebrates() {
        let mut view = test_view();
        view.start_navigation(Instant::now());
        let step = PursuitStep::Move {
            delta: Point { x: 1.0, y: 0.0 },
            facing: Facing::Right,
        };
        view.prepare_running_pose(Facing::Right, Duration::ZERO);
        view.apply_pursuit_step(step);
        view.advance_animation(Duration::from_millis(1));
        let remaining = view.player.until_next_frame(&view.atlas);
        view.apply_pursuit_step(step);
        assert_eq!(view.player.until_next_frame(&view.atlas), remaining);
        view.prepare_running_pose(Facing::Left, Duration::ZERO);
        view.apply_pursuit_step(PursuitStep::Move {
            delta: Point::default(),
            facing: Facing::Left,
        });
        assert_eq!(view.player.animation(), Animation::RunningLeft);
        view.cancel_navigation("test cancel");
        assert!(view.navigation.is_none());
        assert_eq!(view.player.animation(), Animation::Idle);
        view.start_navigation(Instant::now());
        view.apply_pursuit_step(PursuitStep::Blocked("test blocked"));
        assert!(view.navigation.is_none());
        assert_ne!(view.player.animation(), Animation::Succeeded);
    }

    #[test]
    fn pause_freezes_animation_and_resume_resets_pursuit_time() {
        let mut view = test_view();
        view.paused = true;
        let now = Instant::now();
        view.start_navigation(now);
        let frame = view.player.frame_index();
        let remaining = view.player.until_next_frame(&view.atlas);
        assert!(!view.advance_animation(Duration::from_secs(60)));
        assert_eq!(view.player.frame_index(), frame);
        assert_eq!(view.player.until_next_frame(&view.atlas), remaining);
        let restored = now + Duration::from_secs(60);
        view.paused = false;
        view.resume_navigation(restored);
        let Some(Navigation::Seeking(navigator)) = &mut view.navigation else {
            panic!("expected pursuit")
        };
        let step = navigator.step(
            restored,
            Point { x: 100.0, y: 0.0 },
            SpriteGeometry {
                origin: Point::default(),
                pixel_scale: 1.0,
            },
            SpriteMask {
                rgba: &[0, 0, 0, 255],
                width: 1,
                height: 1,
            },
        );
        assert!(matches!(
            step,
            PursuitStep::Move {
                delta: Point { x: 0.0, y: 0.0 },
                ..
            }
        ));
    }

    #[test]
    fn targeted_navigation_coalesces_without_affecting_other_or_closing_views() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(vec![
            familiar("first"),
            familiar("second"),
            familiar("closing"),
        ]);
        app.views.get_mut("closing").unwrap().close_requested = true;
        app.navigation_pending
            .post(Some(vec!["first".into(), "closed-id".into()]));
        app.navigation_pending
            .post(Some(vec!["first".into(), "closing".into()]));
        app.apply_navigation();
        assert!(app.views["first"].seeking());
        assert!(app.views["second"].navigation.is_none());
        assert!(app.views["closing"].navigation.is_none());
        app.views
            .get_mut("first")
            .unwrap()
            .apply_pursuit_step(PursuitStep::Arrived);
        app.navigation_pending.post(Some(vec!["first".into()]));
        app.apply_navigation();
        assert!(app.views["first"].seeking());
        assert_ne!(app.views["first"].player.animation(), Animation::Succeeded);
    }

    fn options() -> RunOptions {
        use clap::Parser;
        let Some(crate::Command::Run(options)) = crate::Cli::parse_from(["numen", "run"]).command
        else {
            unreachable!()
        };
        options
    }

    fn injected_corner(view: &mut FamiliarView, now: Instant) {
        let area = corner::Area {
            x: -1000.0,
            y: -500.0,
            width: 800.0,
            height: 600.0,
        };
        let corner = if view.id == "first" {
            corner::Corner::TopLeft
        } else {
            corner::Corner::BottomRight
        };
        view.start_corner(
            now,
            corner::DisplayArea {
                id: 42,
                frame: area,
                work: area,
            },
            corner.target(area, 100.0, 100.0).unwrap(),
            Point { x: 100.0, y: 100.0 },
        );
    }

    #[test]
    fn corner_broadcast_resolves_latest_views_independently_and_empty_is_not_sticky() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(vec![familiar("removed")]);
        app.navigation_pending.post_corner();
        app.stage_snapshot(vec![
            familiar("first"),
            familiar("second"),
            familiar("closing"),
        ]);
        app.views.get_mut("closing").unwrap().close_requested = true;
        app.views.get_mut("first").unwrap().paused = true;
        app.apply_navigation_with(injected_corner);
        assert!(app.pointer.is_none());
        assert!(!app.views.contains_key("removed"));
        let first = &app.views["first"];
        let second = &app.views["second"];
        assert!(first.paused);
        assert!(!first.mouse_seeking());
        let Some(Navigation::Corner { target: a, .. }) = first.navigation else {
            panic!()
        };
        let Some(Navigation::Corner { target: b, .. }) = second.navigation else {
            panic!()
        };
        assert_ne!(a, b);
        assert!(app.views["closing"].navigation.is_none());
        app.stage_snapshot(vec![]);
        app.navigation_pending.post_corner();
        app.apply_navigation_with(injected_corner);
        app.stage_snapshot(vec![familiar("later")]);
        app.apply_navigation_with(injected_corner);
        assert!(app.views["later"].navigation.is_none());
        app.navigation_pending.post_corner();
        app.apply_navigation_with(injected_corner);
        assert!(app.views["later"].seeking());
    }

    #[test]
    fn mouse_corner_replacement_is_ordered_and_pointer_failure_only_cancels_mouse() {
        for corner_last in [false, true] {
            let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
            app.stage_snapshot(vec![familiar("first"), familiar("second")]);
            if corner_last {
                app.navigation_pending.post(None);
                app.navigation_pending.post_corner();
            } else {
                app.navigation_pending.post_corner();
                app.navigation_pending.post(Some(vec!["first".into()]));
            }
            app.apply_navigation_with(injected_corner);
            assert_eq!(app.views["first"].mouse_seeking(), !corner_last);
            assert!(!app.views["second"].mouse_seeking());
            for view in app.views.values_mut().filter(|v| v.mouse_seeking()) {
                view.cancel_navigation("injected pointer failure");
            }
            assert!(app.views["second"].seeking());
            assert_eq!(app.views["first"].seeking(), corner_last);
            let view = app.views.get_mut("second").unwrap();
            let Some(Navigation::Corner {
                navigator, target, ..
            }) = &mut view.navigation
            else {
                panic!()
            };
            let step = navigator.step_fixed(
                Instant::now() + FRAME_INTERVAL,
                Point::default(),
                *target,
                1.5,
            );
            let delta = view.apply_pursuit_step(step).unwrap();
            assert!(delta.x.hypot(delta.y) > 0.0);
        }
    }

    #[test]
    fn corners_preserve_manual_pause_source_preferences_and_restore_latest_live_source() {
        for manual in [false, true] {
            let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
            let mut snapshot = familiar("first");
            snapshot.preference = Some(preference(3));
            app.stage_snapshot(vec![snapshot]);
            let view = app.views.get_mut("first").unwrap();
            view.paused = true;
            if manual {
                view.display.manual(Animation::Waving);
                view.select(Animation::Waving);
            }
            let binding = view.runtime_binding.clone();
            let pref = view.preference.clone();
            injected_corner(view, Instant::now());
            view.prepare_running_pose(Facing::Left, Duration::ZERO);
            injected_corner(view, Instant::now());
            view.source_animation(source(crate::protocol::ActivityState::Working, None));
            view.set_paused(false);
            assert!(view.seeking());
            assert_eq!(view.display.is_manual(), manual);
            assert_eq!(view.runtime_binding, binding);
            assert_eq!(view.preference, pref);
            assert!(view.pending_placement.is_none());
            assert!(view.pending_restore.is_none());
            view.apply_pursuit_step(PursuitStep::Arrived);
            assert!(view.navigation.is_none());
            assert_eq!(
                view.player.animation(),
                if manual {
                    Animation::Waving
                } else {
                    Animation::Working
                }
            );
            assert_eq!(view.display.is_manual(), manual);
        }
    }

    #[test]
    fn corner_movement_keeps_current_foot_when_midrun_bubbles_expire() {
        let atlas = Arc::new(SpriteAtlas::load().unwrap());
        for backing in [1.0, 1.25, 1.5, 2.0] {
            for after_arrival in [false, true] {
                let mut view = FamiliarView::new("first".into(), atlas.clone(), options());
                let now = Instant::now();
                injected_corner(&mut view, now);
                let scale = view.options.sprite_scale;
                let hidden = (layout_size(false, scale, backing), false, scale);
                let shown = (layout_size(true, scale, backing), true, scale);
                let mut position = PhysicalPosition::new(-500, 300);
                let delta = view
                    .apply_pursuit_step(PursuitStep::Move {
                        delta: Point { x: 24.0, y: 18.0 },
                        facing: Facing::Right,
                    })
                    .unwrap();
                let origin = position.to_logical::<f64>(backing);
                position = LogicalPosition::new(origin.x + delta.x, origin.y + delta.y)
                    .to_physical(backing);

                assert!(view.visibility.reveal(now));
                view.sync_visibility(now).unwrap();
                assert!(view.bubble_visible);
                // Emulate the native show layout's saved hidden origin and resize.
                let show_anchor = position;
                view.hidden_position = Some(show_anchor);
                position = resized_position(position, hidden, shown);
                let delta = view
                    .apply_pursuit_step(PursuitStep::Move {
                        delta: Point { x: 48.0, y: 36.0 },
                        facing: Facing::Right,
                    })
                    .unwrap();
                let origin = position.to_logical::<f64>(backing);
                let moved = view.native_motion.request(
                    Point {
                        x: origin.x,
                        y: origin.y,
                    },
                    delta,
                    backing,
                );
                position = LogicalPosition::new(moved.x, moved.y).to_physical(backing);
                assert!(view.hidden_position.is_none());
                if after_arrival {
                    view.apply_pursuit_step(PursuitStep::Arrived);
                    assert!(view.navigation.is_none());
                } else {
                    assert!(view.seeking());
                }
                let origin = position.to_logical::<f64>(backing);
                let foot = foot_offset(shown.0, backing, true, scale).unwrap();
                let current_foot = Point {
                    x: origin.x + foot.x,
                    y: origin.y + foot.y,
                };
                let expiry = view.visibility.deadline().unwrap();
                assert!(view.visibility.expire(expiry));
                view.sync_visibility(expiry).unwrap();
                assert!(!view.bubble_visible);
                // This is the hide layout's actual saved-anchor/fresh-foot choice.
                let position = view
                    .hidden_position
                    .unwrap_or_else(|| resized_position(position, shown, hidden));
                assert_ne!(
                    position, show_anchor,
                    "hide must not snap to the midrun anchor"
                );
                let origin = position.to_logical::<f64>(backing);
                let foot = foot_offset(hidden.0, backing, false, scale).unwrap();
                assert!((origin.x + foot.x - current_foot.x).abs() < 1e-9);
                assert!((origin.y + foot.y - current_foot.y).abs() < 1e-9);
                assert!(view.pending_placement.is_none());
            }
        }
    }

    #[test]
    fn nonmoving_corner_steps_preserve_saved_bubble_anchor() {
        let atlas = Arc::new(SpriteAtlas::load().unwrap());
        for step in [PursuitStep::Arrived, PursuitStep::Blocked("injected stall")] {
            let mut view = FamiliarView::new("first".into(), atlas.clone(), options());
            injected_corner(&mut view, Instant::now());
            let saved = Some(PhysicalPosition::new(-350, 240));
            view.hidden_position = saved;
            view.set_paused(true);
            assert_eq!(view.hidden_position, saved);
            assert!(view.apply_pursuit_step(step).is_none());
            assert_eq!(view.hidden_position, saved);
            assert!(view.pending_placement.is_none());
        }
    }

    #[test]
    fn no_hotkey_keeps_focused_fallbacks_but_registered_globals_suppress_duplicates() {
        for no_hotkey in [false, true] {
            for registered in [false, true] {
                let mut options = options();
                options.no_hotkey = no_hotkey;
                let mut app = App::new(SpriteAtlas::load().unwrap(), options, true);
                app.stage_snapshot(vec![familiar("first")]);
                let view = app.views.get_mut("first").unwrap();
                view.modifiers = ModifiersState::ALT;
                view.navigation_available = true;
                view.global_hotkey_registered = registered;
                view.global_navigation_registered = registered;
                view.global_corner_registered = registered;
                assert_eq!(
                    local_reveal_shortcut(
                        KeyCode::KeyI,
                        view.modifiers,
                        view.global_hotkey_registered,
                    ),
                    !registered
                );
                assert_eq!(view.local_navigation(KeyCode::KeyM, true), !registered);
                assert_eq!(
                    view.local_corner(KeyCode::KeyC, true),
                    !registered && cfg!(target_os = "macos")
                );
                assert!(!view.local_navigation(KeyCode::KeyM, false));
                assert!(!view.local_corner(KeyCode::KeyC, false));
            }
        }
        for mode in 0..3 {
            let mut options = options();
            options.no_hotkey = true;
            options.manual = mode == 0;
            options.demo = mode == 1;
            let mut app = App::new(SpriteAtlas::load().unwrap(), options, false);
            app.stage_snapshot(vec![familiar("preview")]);
            let view = app.views.get_mut("preview").unwrap();
            view.modifiers = ModifiersState::ALT;
            assert!(!view.local_corner(KeyCode::KeyC, true));
            assert!(!view.local_navigation(KeyCode::KeyM, true));
        }
    }

    #[test]
    fn local_corner_gates_are_independent_of_pointer_bubbles_and_mouse_registration() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(vec![familiar("first")]);
        let view = app.views.get_mut("first").unwrap();
        view.options.no_bubble = true;
        view.global_navigation_registered = true;
        view.modifiers = ModifiersState::ALT;
        assert!(view.local_corner(KeyCode::KeyC, true));
        assert!(!view.local_corner(KeyCode::KeyC, false));
        assert!(!view.local_corner(KeyCode::KeyM, true));
        view.modifiers |= ModifiersState::SHIFT;
        assert!(!view.local_corner(KeyCode::KeyC, true));
        view.modifiers = ModifiersState::ALT;
        view.global_corner_registered = true;
        assert!(!view.local_corner(KeyCode::KeyC, true));
        view.global_corner_registered = false;
        view.options.no_hotkey = true;
        assert!(view.local_corner(KeyCode::KeyC, true));
        assert!(!view.local_navigation(KeyCode::KeyM, true));
        view.options.no_hotkey = false;
        view.corner_available = false;
        assert!(!view.local_corner(KeyCode::KeyC, true));
    }

    #[test]
    fn shortcut_diagnostics_cover_gates_pending_and_independent_results() {
        for flags in 0..1024 {
            let flag = |bit| flags & (1 << bit) != 0;
            let mut options = options();
            options.no_hotkey = flag(0);
            options.no_bubble = flag(1);
            options.manual = flag(5);
            options.demo = flag(6);
            let pointer_available = flag(2);
            let live = flag(3);
            let platform_supported = flag(4);
            let succeeded = [flag(7), flag(8), flag(9)];
            let mut expected =
                [Shortcut::Reveal, Shortcut::GoToMouse, Shortcut::GoToCorner].map(|key| {
                    let (state, reason) = match key {
                        _ if options.no_hotkey => {
                            (ShortcutState::Disabled, ShortcutReason::NoHotkey)
                        }
                        Shortcut::Reveal if options.no_bubble => {
                            (ShortcutState::Disabled, ShortcutReason::NoBubble)
                        }
                        _ if !platform_supported => (
                            ShortcutState::Unsupported,
                            ShortcutReason::UnsupportedPlatform,
                        ),
                        Shortcut::GoToCorner if options.manual || options.demo || !live => {
                            (ShortcutState::Unsupported, ShortcutReason::UnsupportedMode)
                        }
                        Shortcut::GoToMouse if !pointer_available => {
                            if options.manual || options.demo || !live {
                                (ShortcutState::Unsupported, ShortcutReason::UnsupportedMode)
                            } else {
                                (
                                    ShortcutState::Unavailable,
                                    ShortcutReason::PointerInitializationFailed,
                                )
                            }
                        }
                        _ => (
                            ShortcutState::Uninitialized,
                            ShortcutReason::RegistrationPending,
                        ),
                    };
                    ShortcutStatus {
                        state,
                        reason: Some(reason),
                    }
                });
            let eligible =
                expected.map(|status| status.reason == Some(ShortcutReason::RegistrationPending));
            let diagnostics = RuntimeDiagnostics::new(true);
            let reader = diagnostics.clone();
            let mut attempted = [false; 3];
            let guards = register_shortcuts(
                &options,
                pointer_available,
                live,
                platform_supported,
                Some(&diagnostics),
                |key| {
                    // Reading from another thread also proves registration holds no diagnostics lock.
                    let reader = reader.clone();
                    let hotkeys = std::thread::spawn(move || reader.snapshot().hotkeys)
                        .join()
                        .unwrap();
                    assert_eq!(
                        [
                            hotkeys.option_i,
                            hotkeys.option_m,
                            hotkeys.option_c.unwrap()
                        ],
                        expected,
                        "flags={flags}"
                    );
                    let index = match key {
                        Shortcut::Reveal => 0,
                        Shortcut::GoToMouse => 1,
                        Shortcut::GoToCorner => 2,
                    };
                    assert!(eligible[index]);
                    attempted[index] = true;
                    expected[index] = if succeeded[index] {
                        ShortcutStatus {
                            state: ShortcutState::Registered,
                            reason: None,
                        }
                    } else {
                        ShortcutStatus {
                            state: ShortcutState::Unavailable,
                            reason: Some(ShortcutReason::RegistrationFailed),
                        }
                    };
                    anyhow::ensure!(succeeded[index], "private simulated OS registration error");
                    Ok(key)
                },
            );
            let hotkeys = reader.snapshot().hotkeys;
            assert_eq!(
                [
                    hotkeys.option_i,
                    hotkeys.option_m,
                    hotkeys.option_c.unwrap()
                ],
                expected,
                "flags={flags}"
            );
            assert_eq!(attempted, eligible);
            assert_eq!(
                guards.map(|guard| guard.is_some()),
                [
                    eligible[0] && succeeded[0],
                    eligible[1] && succeeded[1],
                    eligible[2] && succeeded[2]
                ]
            );
            assert!(
                !serde_json::to_string(&reader.snapshot())
                    .unwrap()
                    .contains("private simulated")
            );
        }
    }

    #[test]
    fn service_reports_injected_shortcuts_without_changing_navigation_capability() {
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;

        for reveal_ok in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let path = temp.path().join("events.sock");
            let options = options();
            let server = Server::start_graphical_with_settings(
                &path,
                |_| {},
                |_| {},
                Some(Box::new(|_| {})),
                || {},
                true,
                familiar_settings(&options),
            )
            .unwrap();
            let service = || {
                crate::ipc::control(&path, json!({"version": 1, "type": "companion.service"}))
                    .unwrap()
            };
            let before = service();
            let diagnostics = server.diagnostics();
            assert_eq!(
                diagnostics.snapshot().hotkeys.option_i.reason,
                Some(ShortcutReason::NotReported)
            );
            assert_eq!(
                diagnostics.snapshot().hotkeys.option_m.reason,
                Some(ShortcutReason::NotReported)
            );
            let guards =
                register_shortcuts(&options, true, true, true, Some(&diagnostics), |key| {
                    let response = service();
                    assert_eq!(response["navigation"], true);
                    assert!(response.to_string().contains("registration_pending"));
                    anyhow::ensure!((key == Shortcut::Reveal) == reveal_ok, "private collision");
                    Ok(key)
                });
            let after = service();
            let mut expected_service = before;
            expected_service["diagnostics"] = json!(diagnostics.snapshot());
            assert_eq!(after, expected_service);
            assert_eq!(
                guards.map(|guard| guard.is_some()),
                [reveal_ok, !reveal_ok, !reveal_ok]
            );
            let encoded = after.to_string();
            assert!(encoded.contains("registered"));
            assert!(encoded.contains("registration_failed"));
            assert!(!encoded.contains("registration_pending"));
            assert!(!encoded.contains("private collision"));
        }
    }

    #[test]
    fn shortcut_registration_gates_and_failures_are_independent() {
        for no_hotkey in [false, true] {
            for no_bubble in [false, true] {
                for pointer_available in [false, true] {
                    for reveal_ok in [false, true] {
                        for navigation_ok in [false, true] {
                            let mut options = options();
                            options.no_hotkey = no_hotkey;
                            options.no_bubble = no_bubble;
                            let mut attempted = Vec::new();
                            let guards = register_shortcuts(
                                &options,
                                pointer_available,
                                true,
                                true,
                                None,
                                |key| {
                                    attempted.push(key);
                                    let ok = match key {
                                        Shortcut::Reveal => reveal_ok,
                                        Shortcut::GoToMouse => navigation_ok,
                                        Shortcut::GoToCorner => true,
                                    };
                                    anyhow::ensure!(ok, "simulated key collision");
                                    Ok(key)
                                },
                            );
                            let reveal_enabled = !no_hotkey && !no_bubble;
                            let navigation_enabled = !no_hotkey && pointer_available;
                            assert_eq!(attempted.contains(&Shortcut::Reveal), reveal_enabled);
                            assert_eq!(
                                attempted.contains(&Shortcut::GoToMouse),
                                navigation_enabled
                            );
                            assert_eq!(guards[0].is_some(), reveal_enabled && reveal_ok);
                            assert_eq!(guards[1].is_some(), navigation_enabled && navigation_ok);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn local_shortcuts_have_independent_exact_modifier_and_pointer_gates() {
        for reveal_registered in [false, true] {
            for navigation_registered in [false, true] {
                for pointer_available in [false, true] {
                    for modifiers in [
                        ModifiersState::empty(),
                        ModifiersState::ALT,
                        ModifiersState::ALT | ModifiersState::SHIFT,
                        ModifiersState::ALT | ModifiersState::CONTROL,
                        ModifiersState::ALT | ModifiersState::SUPER,
                    ] {
                        for code in [KeyCode::KeyI, KeyCode::KeyM, KeyCode::KeyQ] {
                            assert_eq!(
                                local_reveal_shortcut(code, modifiers, reveal_registered),
                                !reveal_registered
                                    && code == KeyCode::KeyI
                                    && modifiers == ModifiersState::ALT
                            );
                            assert_eq!(
                                local_navigation_shortcut(
                                    code,
                                    modifiers,
                                    navigation_registered,
                                    pointer_available,
                                ),
                                pointer_available
                                    && !navigation_registered
                                    && code == KeyCode::KeyM
                                    && modifiers == ModifiersState::ALT
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn local_navigation_only_changes_focused_view_and_keeps_pause() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(vec![familiar("focused"), familiar("other")]);
        let view = app.views.get_mut("focused").unwrap();
        view.navigation_available = true;
        view.modifiers = ModifiersState::ALT;
        view.options.no_hotkey = true;
        view.paused = true;
        view.display.manual(Animation::Waving);
        assert!(!view.local_navigation(KeyCode::KeyM, false));
        assert!(view.display.is_manual());
        assert!(view.navigation.is_none());
        assert!(view.local_navigation(KeyCode::KeyM, true));
        assert!(view.seeking());
        assert!(view.paused);
        assert!(!view.display.is_manual());
        view.apply_pursuit_step(PursuitStep::Arrived);
        view.global_navigation_registered = true;
        assert!(!view.local_navigation(KeyCode::KeyM, true));
        assert!(matches!(view.navigation, Some(Navigation::Celebrating)));
        assert!(app.views["other"].navigation.is_none());
    }

    #[test]
    fn new_views_inherit_independent_registration_and_manual_demo_have_no_navigation() {
        for reveal_registered in [false, true] {
            for navigation_registered in [false, true] {
                let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
                app.global_hotkey_registered = reveal_registered;
                app.global_navigation_registered = navigation_registered;
                app.stage_snapshot(vec![]);
                app.stage_snapshot(vec![familiar("later")]);
                assert_eq!(
                    app.views["later"].global_hotkey_registered,
                    reveal_registered
                );
                assert_eq!(
                    app.views["later"].global_navigation_registered,
                    navigation_registered
                );
                assert!(!app.views["later"].navigation_available);
            }
        }
        for demo in [false, true] {
            let mut options = options();
            options.demo = demo;
            options.manual = !demo;
            let mut app = App::new(SpriteAtlas::load().unwrap(), options, false);
            app.stage_snapshot(vec![familiar("preview")]);
            let view = app.views.get_mut("preview").unwrap();
            view.modifiers = ModifiersState::ALT;
            assert!(!view.local_navigation(KeyCode::KeyM, true));
            assert!(view.display.is_manual());
            assert!(view.navigation.is_none());
            let guards = register_shortcuts(
                &app.options,
                app.pointer.is_some(),
                app.live,
                true,
                None,
                |key| {
                    assert_eq!(key, Shortcut::Reveal);
                    Ok(())
                },
            );
            assert!(guards[1].is_none());
        }
    }

    #[test]
    fn global_navigation_resolves_current_registry_and_skips_unavailable_views() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(vec![familiar("removed")]);
        app.navigation_pending.post(None);
        app.mailbox.post(vec![
            familiar("new"),
            familiar("closing"),
            familiar("pending"),
            familiar("submitted"),
            familiar("failed"),
            familiar("error"),
        ]);
        app.stage_snapshot(app.mailbox.take().unwrap());
        app.views.get_mut("closing").unwrap().close_requested = true;
        app.pending_close.insert("pending".into());
        app.submitted_close.insert("submitted".into());
        app.failed.insert("failed".into());
        app.views.get_mut("error").unwrap().error = Some(anyhow::anyhow!("failed"));
        app.views.get_mut("new").unwrap().paused = true;
        app.apply_navigation();
        assert!(!app.views.contains_key("removed"));
        for (id, view) in &app.views {
            assert_eq!(view.seeking(), id == "new", "{id}");
        }
        let view = app.views.get_mut("new").unwrap();
        view.apply_pursuit_step(PursuitStep::Arrived);
        assert!(matches!(view.navigation, Some(Navigation::Celebrating)));
        app.navigation_pending.post(None);
        app.apply_navigation();
        assert!(app.views["new"].seeking());
        assert!(app.views["new"].paused);
        app.stage_snapshot(vec![]);
        app.navigation_pending.post(None);
        app.apply_navigation();
        assert!(app.views.is_empty());
        app.stage_snapshot(vec![familiar("later")]);
        app.apply_navigation();
        assert!(app.views["later"].navigation.is_none());
        app.navigation_pending.post(None);
        app.apply_navigation();
        assert!(app.views["later"].seeking());
    }

    #[test]
    fn navigation_mailbox_is_bounded_and_targeted_requests_never_broadcast() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        let snapshot = (0..40).map(|n| familiar(&format!("f{n}"))).collect();
        app.stage_snapshot(snapshot);
        for n in 0..40 {
            assert_eq!(
                app.navigation_pending.post(Some(vec![format!("f{n}")])),
                n == 0
            );
        }
        assert_eq!(app.navigation_pending.0.lock().unwrap().len(), 32);
        app.apply_navigation();
        for n in 0..40 {
            assert_eq!(app.views[&format!("f{n}")].seeking(), n >= 8);
        }
        app.navigation_pending.post(Some(vec![]));
        app.apply_navigation();
        assert!(!app.views["f0"].seeking());
        app.navigation_pending
            .post(Some((0..20).map(|n| format!("f{n}")).collect()));
        assert_eq!(
            app.navigation_pending.take()[0].1.as_ref().unwrap().len(),
            16
        );
        app.navigation_pending.post(Some(vec!["f0".into()]));
        app.stage_snapshot(vec![familiar("f0"), familiar("unrelated")]);
        app.apply_navigation();
        assert!(app.views["f0"].seeking());
        assert!(app.views["unrelated"].navigation.is_none());
    }

    fn familiar(id: &str) -> FamiliarPresentation {
        FamiliarPresentation {
            id: id.into(),
            preset: DEFAULT_CHARACTER_ID.into(),
            settings: FamiliarSettings::default(),
            presentation: Presentation::default(),
            preference: None,
            runtime_binding: unbound_attachment(),
        }
    }

    #[test]
    fn settings_update_only_selected_view_without_resetting_live_state() {
        for manual in [false, true] {
            for seeking in [false, true] {
                let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
                let mut selected = familiar("selected");
                selected.presentation = source(crate::protocol::ActivityState::Succeeded, Some(71));
                app.stage_snapshot(vec![selected.clone(), familiar("other")]);
                let view = app.views.get_mut("selected").unwrap();
                view.latest_view = selected.presentation.clone();
                view.source_animation(selected.presentation.clone());
                if seeking {
                    view.start_navigation(Instant::now());
                    view.select(Animation::RunningLeft);
                }
                if manual {
                    view.display.manual(Animation::Waving);
                    view.select(Animation::Waving);
                }
                view.player.advance(Duration::from_millis(175), &view.atlas);
                view.paused = true;
                view.scene_dirty = false;
                view.cache_raster(bubble::dimensions(false, view.options.sprite_scale));
                view.renderer = Some(bubble::Renderer::new(
                    Some(Typeface::bundled().unwrap()),
                    12.0,
                ));
                let animation = view.player.animation();
                let frame = view.player.frame_index();
                let remaining = view.player.until_next_frame(&view.atlas);
                let tick = view.last_tick;
                let demo = view.next_demo;
                let motion = view.motion_clock.deadline();
                let deadline = view.visibility.deadline();
                let atlas = view.atlas.clone();
                let other = &app.views["other"];
                let other_tick = other.last_tick;
                let other_settings = familiar_settings(&other.options);
                selected.settings = FamiliarSettings {
                    sprite_scale: 1.375,
                    font_size: 23.5,
                    bubble_duration: 0.25,
                };
                app.stage_snapshot(vec![selected.clone(), familiar("other")]);
                let view = app.views.get_mut("selected").unwrap();
                assert_eq!(familiar_settings(&view.options), selected.settings);
                assert!(view.scene_dirty);
                assert!(view.raster.is_none());
                assert!(view.renderer.is_some());
                assert!(Arc::ptr_eq(&view.atlas, &atlas));
                assert_eq!(view.latest_view, selected.presentation);
                assert_eq!(view.latest_view.cue.as_ref().unwrap().id, 71);
                assert_eq!(view.display.is_manual(), manual);
                assert_eq!(view.display.update(selected.presentation.clone()), None);
                assert_eq!(view.player.animation(), animation);
                assert_eq!(view.player.frame_index(), frame);
                assert_eq!(view.player.until_next_frame(&view.atlas), remaining);
                assert!(view.paused);
                assert_eq!(view.seeking(), seeking);
                assert_eq!(view.motion_clock.deadline(), motion);
                assert_eq!(view.last_tick, tick);
                assert_eq!(view.next_demo, demo);
                assert_eq!(view.visibility.deadline(), deadline);
                view.scene_dirty = false;
                view.cache_raster(bubble::dimensions(false, view.options.sprite_scale));
                app.stage_snapshot(vec![selected, familiar("other")]);
                assert!(!app.views["selected"].scene_dirty);
                assert!(app.views["selected"].raster.is_some());
                let other = &app.views["other"];
                assert_eq!(familiar_settings(&other.options), other_settings);
                assert_eq!(other.last_tick, other_tick);
                assert!(!other.paused);
                assert_eq!(other.player.animation(), Animation::Idle);
                assert!(other.navigation.is_none());
                assert_eq!(familiar_settings(&app.options), FamiliarSettings::default());
            }
        }
    }

    #[test]
    fn newest_complete_snapshot_configures_new_view_before_native_initialization() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        let mut new = familiar("new");
        app.mailbox.post(vec![new.clone()]);
        new.settings.font_size = 30.0;
        app.mailbox.post(vec![new.clone()]);
        new.settings.sprite_scale = 0.375;
        new.settings.bubble_duration = 17.25;
        app.mailbox.post(vec![new.clone()]);
        app.stage_snapshot(app.mailbox.take().unwrap());
        let view = app.views.get_mut("new").unwrap();
        assert!(view.window.is_none());
        assert!(view.renderer.is_none());
        assert_eq!(familiar_settings(&view.options), new.settings);
        let now = Instant::now();
        assert!(view.visibility.reveal(now));
        assert_eq!(
            view.visibility.deadline(),
            Some(now + Duration::from_secs_f64(17.25))
        );
        assert_eq!(
            bubble::dimensions(false, view.options.sprite_scale),
            (72, 78)
        );
    }

    #[test]
    fn settings_resize_requires_fresh_contact_without_restarting_running_pose() {
        let mut view = test_view();
        let now = Instant::now();
        view.start_navigation(now);
        view.prepare_running_pose(Facing::Right, Duration::ZERO);
        view.player.advance(Duration::from_millis(175), &view.atlas);
        let frame_index = view.player.frame_index();
        let remaining = view.player.until_next_frame(&view.atlas);
        view.cache_raster(bubble::dimensions(false, view.options.sprite_scale));
        view.apply_settings(FamiliarSettings {
            sprite_scale: 1.0,
            ..FamiliarSettings::default()
        })
        .unwrap();
        assert!(view.raster.is_none());
        let frame = view.atlas.frame(Animation::RunningRight, frame_index);
        let index = frame
            .chunks_exact(4)
            .enumerate()
            .find(|(index, rgba)| index % 192 >= 96 && rgba[3] >= 32)
            .unwrap()
            .0;
        let cursor = Point {
            x: (index % 192) as f64 + 0.5,
            y: (index / 192) as f64 + 0.5,
        };
        let step = view.plan_pursuit(
            now,
            cursor,
            Point::default(),
            (192, 208),
            1.0,
            Duration::ZERO,
        );
        assert!(matches!(step, PursuitStep::Move { .. }));
        assert_eq!(view.player.animation(), Animation::RunningRight);
        assert_eq!(view.player.frame_index(), frame_index);
        assert_eq!(view.player.until_next_frame(&view.atlas), remaining);
        assert!(view.seeking());
        assert_eq!(
            view.plan_pursuit(
                now + FRAME_INTERVAL,
                cursor,
                Point::default(),
                (192, 208),
                1.0,
                Duration::ZERO
            ),
            PursuitStep::Arrived
        );
    }

    #[test]
    fn resizing_preserves_physical_feet_across_visibility_scale_and_rounding() {
        for density in [1.0, 1.25, 1.5, 2.0, 2.75] {
            for scene in [1.0, 1.375] {
                let physical = density * scene;
                let layout = |show, scale| (layout_size(show, scale, physical), show, scale);
                let initial = layout(false, 0.5);
                let start = PhysicalPosition::new(-123, 456);
                let mut position = start;
                let mut old = initial;
                for _ in 0..10 {
                    for (show, scale) in [
                        (true, 0.5),
                        (true, 0.713),
                        (false, 0.713),
                        (false, 2.0),
                        (true, 2.0),
                        (true, 0.25),
                        (false, 0.5),
                    ] {
                        let new = layout(show, scale);
                        position = resized_position(position, old, new);
                        assert_eq!(resized_position(position, new, initial), start);
                        old = new;
                    }
                    assert_eq!(position, start, "repeated edits must not drift");
                }
                // A shown window may have been monitor-clamped. Its saved hidden
                // origin must still be rebased to the new sprite's feet.
                let saved = resized_position(start, initial, layout(false, 1.375));
                assert_eq!(
                    resized_position(saved, layout(false, 1.375), initial),
                    start
                );
                let shown = resized_position(start, initial, layout(true, 0.5));
                let clamped = clamp_to_monitor(
                    shown,
                    PhysicalPosition::new(0, 0),
                    (1920, 1080),
                    (
                        f64::from(layout(true, 0.5).0.0),
                        f64::from(layout(true, 0.5).0.1),
                    ),
                );
                let edited = resized_position(clamped, layout(true, 0.5), layout(true, 1.375));
                assert_eq!(
                    resized_position(edited, layout(true, 1.375), layout(true, 0.5)),
                    clamped
                );
                assert_ne!(saved, start, "hide must not restore the old-scale origin");
            }
        }
    }

    #[test]
    fn complete_registry_reconciliation_preserves_independent_view_state() {
        let mut options = options();
        options.focus = true;
        let mut app = App::new(SpriteAtlas::load().unwrap(), options, true);
        let updates = app.stage_snapshot(vec![familiar("main"), familiar("second")]);
        assert_eq!(updates.len(), 2);
        assert!(app.views["main"].options.focus);
        assert!(!app.views["second"].options.focus);
        assert!(Arc::ptr_eq(
            &app.views["main"].atlas,
            &app.views["second"].atlas
        ));
        let main = app.views.get_mut("main").unwrap();
        main.display.manual(Animation::Waving);
        main.select(Animation::Waving);
        main.paused = true;
        main.hidden_position = Some(PhysicalPosition::new(123, 456));
        let tick = main.last_tick;
        let demo = main.next_demo;
        let mut changed = familiar("second");
        changed.presentation.base = Some(crate::activity::Selection {
            source: "second-source".into(),
            instance: "one".into(),
            activity: "task".into(),
            state: crate::protocol::ActivityState::Working,
            attention: false,
            reason: None,
            summary: Some("second view only".into()),
        });
        let updates = app.stage_snapshot(vec![familiar("main"), changed.clone()]);
        assert_eq!(updates[1], ("second".into(), changed.presentation));
        let main = &app.views["main"];
        assert!(main.display.is_manual());
        assert_eq!(main.player.animation(), Animation::Waving);
        assert!(main.paused);
        assert_eq!(main.hidden_position, Some(PhysicalPosition::new(123, 456)));
        assert_eq!(main.last_tick, tick);
        assert_eq!(main.next_demo, demo);
        assert!(!app.views["second"].display.is_manual());
        assert_eq!(app.views["second"].player.animation(), Animation::Idle);
        app.stage_snapshot(vec![familiar("second"), familiar("third")]);
        assert!(!app.views.contains_key("main"));
        assert!(app.views.contains_key("third"));
        app.stage_snapshot(vec![]);
        assert!(app.views.is_empty());
        app.stage_snapshot(vec![familiar("main")]);
        assert_eq!(app.views.len(), 1);
        assert!(!app.views["main"].options.focus);
    }

    #[test]
    fn unexpected_presets_and_failed_views_wait_for_registry_removal() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        let mut unsupported = familiar("bad");
        unsupported.preset = "unknown".into();
        app.stage_snapshot(vec![familiar("main"), unsupported]);
        assert!(app.views.contains_key("main"));
        assert!(!app.views.contains_key("bad"));
        assert!(app.pending_close.contains("bad"));
        app.stage_snapshot(vec![familiar("main"), familiar("bad")]);
        assert!(!app.views.contains_key("bad"));
        app.stage_snapshot(vec![]);
        assert!(app.failed.is_empty());
        assert!(app.pending_close.is_empty());
        app.stage_snapshot(vec![familiar("bad")]);
        assert!(app.views.contains_key("bad"));
    }

    #[test]
    fn user_close_retries_full_queue_without_another_gesture_and_submits_once() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(vec![familiar("main")]);
        let view = app.views.get_mut("main").unwrap();
        view.paused = true;
        assert!(view.window.is_none());
        view.close_requested = true;

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender.try_send("already-queued".to_owned()).unwrap();
        let submit = |id: &str| {
            sender
                .try_send(id.to_owned())
                .context("queue familiar close")
        };
        app.retry_closes(submit);
        app.retry_closes(submit);
        assert_eq!(app.pending_close, BTreeSet::from(["main".into()]));
        assert!(app.submitted_close.is_empty());
        // An unrelated snapshot does not drop an outstanding close intent.
        app.stage_snapshot(vec![familiar("main")]);
        assert_eq!(receiver.try_recv().unwrap(), "already-queued");

        app.retry_closes(submit);
        assert_eq!(receiver.try_recv().unwrap(), "main");
        assert!(app.pending_close.is_empty());
        assert!(app.submitted_close.contains("main"));
        assert!(app.views.contains_key("main"));
        app.stage_snapshot(vec![familiar("main")]);
        app.retry_closes(submit);
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));

        app.stage_snapshot(vec![]);
        assert!(app.views.is_empty());
        assert!(app.pending_close.is_empty());
        assert!(app.submitted_close.is_empty());
        // Reopening the same ID after acknowledgment has no stale close intent.
        app.stage_snapshot(vec![familiar("main")]);
        app.retry_closes(submit);
        assert!(!app.views["main"].close_requested);
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn registry_removal_cancels_close_before_retry_submission() {
        let mut app = App::new(SpriteAtlas::load().unwrap(), options(), true);
        app.stage_snapshot(vec![familiar("main")]);
        app.views.get_mut("main").unwrap().close_requested = true;
        app.retry_closes(|_| anyhow::bail!("queue full"));
        assert!(app.pending_close.contains("main"));
        app.stage_snapshot(vec![]);
        app.retry_closes(|_| panic!("removed familiar must not be submitted"));
        assert!(app.pending_close.is_empty());
    }

    #[test]
    fn targeted_reveals_union_without_promoting_to_broadcast() {
        let mailbox = RevealMailbox::default();
        assert!(mailbox.post(Some(vec!["first".into()])));
        assert!(!mailbox.post(Some(vec!["second".into(), "first".into()])));
        let targets = reveal_targets(mailbox.take(), ["unrelated".into()].into_iter());
        assert_eq!(targets, BTreeSet::from(["first".into(), "second".into()]));
        assert!(mailbox.post(Some(vec![])));
        assert!(reveal_targets(mailbox.take(), ["unrelated".into()].into_iter()).is_empty());
        assert!(mailbox.post(Some(vec!["first".into()])));
        assert!(!mailbox.post(None));
        assert!(!mailbox.post(Some(vec!["second".into()])));
        assert_eq!(mailbox.0.lock().unwrap().len(), 1);
        assert_eq!(
            reveal_targets(mailbox.take(), ["current".into()].into_iter()),
            BTreeSet::from(["current".into()])
        );
        assert!(mailbox.post(Some(vec!["new-wake".into()])));
    }

    #[test]
    fn reveal_queue_is_bounded_and_keeps_latest_targeted_requests() {
        let mailbox = RevealMailbox::default();
        for index in 0..40 {
            assert_eq!(mailbox.post(Some(vec![index.to_string()])), index == 0);
        }
        let requests = mailbox.take();
        assert_eq!(requests.len(), 32);
        assert_eq!(requests.front(), Some(&Some(vec!["8".into()])));
        let targets = reveal_targets(requests, ["unrelated".into()].into_iter());
        assert_eq!(targets.len(), 32);
        assert!(targets.contains("39"));
        assert!(!targets.contains("unrelated"));
        mailbox.post(Some((0..20).map(|i| i.to_string()).collect()));
        assert_eq!(mailbox.take()[0].as_ref().unwrap().len(), 16);
    }

    #[test]
    fn local_reveal_is_an_exact_fallback_not_a_duplicate_global_gesture() {
        assert!(local_reveal_shortcut(
            KeyCode::KeyI,
            ModifiersState::ALT,
            false
        ));
        assert!(!local_reveal_shortcut(
            KeyCode::KeyI,
            ModifiersState::ALT,
            true
        ));
        assert!(!local_reveal_shortcut(
            KeyCode::KeyQ,
            ModifiersState::ALT,
            false
        ));
        assert!(!local_reveal_shortcut(
            KeyCode::KeyI,
            ModifiersState::empty(),
            false
        ));
        assert!(!local_reveal_shortcut(
            KeyCode::KeyI,
            ModifiersState::ALT | ModifiersState::SHIFT,
            false
        ));
    }

    #[test]
    fn notification_geometry_preserves_sprite_anchor_and_handles_screen_edges() {
        for density in [1.0, 1.5, 2.0] {
            let start = PhysicalPosition::new(1000, 800);
            let expanded = anchored_position(start, false, true, 0.5, 1.5 * density);
            assert_eq!(
                anchored_position(expanded, true, false, 0.5, 1.5 * density),
                start
            );
        }
        assert_eq!(
            clamp_to_monitor(
                PhysicalPosition::new(1700, 900),
                PhysicalPosition::new(0, 0),
                (1920, 1080),
                (1152.0, 744.0)
            ),
            PhysicalPosition::new(768, 336)
        );
        assert_eq!(
            clamp_to_monitor(
                PhysicalPosition::new(-3000, -500),
                PhysicalPosition::new(-1920, 0),
                (1920, 1080),
                (576.0, 372.0)
            ),
            PhysicalPosition::new(-1920, 0)
        );
    }

    #[test]
    fn keyboard_selects_every_expression_once() {
        let keys = [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ];
        for (key, animation) in keys.into_iter().zip(Animation::ALL) {
            assert_eq!(animation_for_key(key), Some(animation));
        }
        assert_eq!(animation_for_key(KeyCode::KeyQ), None);
    }

    #[test]
    fn cycle_remembers_selection_after_one_shot_returns_to_idle() {
        use clap::Parser;
        for (selected, next) in [
            (Animation::Succeeded, Animation::Failed),
            (Animation::Failed, Animation::Review),
            (Animation::Waving, Animation::RunningLeft),
        ] {
            let Some(crate::Command::Run(mut options)) =
                crate::Cli::parse_from(["numen", "run"]).command
            else {
                unreachable!()
            };
            options.animation = selected;
            let mut app = FamiliarView::new(
                "main".into(),
                Arc::new(SpriteAtlas::load().unwrap()),
                options,
            );
            let total: Duration = app
                .atlas
                .clip(selected)
                .frame_durations
                .iter()
                .copied()
                .sum();
            app.player.advance(total, &app.atlas);
            assert_eq!(app.player.animation(), Animation::Idle);
            app.cycle();
            assert_eq!(app.player.animation(), next);
        }
    }
}
