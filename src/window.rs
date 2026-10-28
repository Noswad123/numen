use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
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
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId, WindowLevel};

use crate::RunOptions;
use crate::activity::{FamiliarPresentation, Presentation};
use crate::animation::{Animation, Player};
use crate::assets::{DEFAULT_CHARACTER_ID, DEFAULT_CHARACTER_NAME, SpriteAtlas};
use crate::bubble;
use crate::display::{LiveDisplay, Mailbox};
use crate::hotkey::GlobalRevealHotkey;
use crate::ipc::{FamiliarControl, Server};
#[cfg(test)]
use crate::motion::{FRAME_INTERVAL, SpriteMask};
use crate::motion::{
    Facing, MotionClock, NativeMotion, Navigator, Point, PursuitStep, RasterMask, SpriteGeometry,
};
use crate::pointer::{self, PointerReader};
use crate::typography::Typeface;
use crate::visibility::BubbleVisibility;

const DEMO_INTERVAL: Duration = Duration::from_secs(3);

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
            Err(error) => warn!(%error, "Live pointer unavailable; navigation disabled"),
        }
    }
    let mailbox = app.mailbox.clone();
    let proxy = event_loop.create_proxy();
    let reveal = RevealNotifier {
        pending: app.reveal_pending.clone(),
        proxy: proxy.clone(),
    };
    let ipc_reveal = reveal.clone();
    let navigation_pending = app.navigation_pending.clone();
    let navigation_proxy = proxy.clone();
    let quit_proxy = proxy.clone();
    let quit_pending = Arc::new(AtomicBool::new(false));
    let server = socket
        .map(|path| {
            let on_views = move |view| {
                if mailbox.post(view) {
                    let _ = proxy.send_event(AppEvent::ActivityChanged);
                }
            };
            let on_reveal = move |ids| ipc_reveal.request(ids);
            let on_go_to_mouse = if app.pointer.is_some() {
                Some(Box::new(move |ids| {
                    if navigation_pending.post(Some(ids))
                        && navigation_proxy.send_event(AppEvent::GoToMouse).is_err()
                    {
                        navigation_pending.take();
                    }
                }) as Box<dyn Fn(Vec<String>) + Send>)
            } else {
                None
            };
            let server = Server::start_graphical(
                path,
                on_views,
                on_reveal,
                on_go_to_mouse,
                move || {
                    if !quit_pending.swap(true, Ordering::AcqRel) {
                        let _ = quit_proxy.send_event(AppEvent::QuitService);
                    }
                },
                !app.options.empty,
            )?;
            info!(socket = %path.display(), "Local activity listener ready");
            Ok::<_, anyhow::Error>(server)
        })
        .transpose()?;
    app.control = server.as_ref().map(Server::familiar_control);
    let hotkey = if !app.options.no_bubble && !app.options.no_hotkey {
        match GlobalRevealHotkey::register(move || reveal.request(None)) {
            Ok(registration) => {
                info!("Global Option+I reveal shortcut registered");
                Some(registration)
            }
            Err(error) => {
                warn!(%error, "Global shortcut unavailable; use `numen bubbles` or local Option+I instead");
                None
            }
        }
    } else {
        None
    };
    app.global_hotkey_registered = hotkey.is_some();
    let result = event_loop.run_app(&mut app);
    drop(hotkey);
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
    navigation: Option<Navigation>,
    motion_clock: MotionClock,
    native_motion: NativeMotion,
    raster: Option<CachedRaster>,
    scene_dirty: bool,
}

impl FamiliarView {
    fn new(id: String, atlas: Arc<SpriteAtlas>, options: RunOptions) -> Self {
        let now = Instant::now();
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
            navigation: None,
            motion_clock: MotionClock::default(),
            native_motion: NativeMotion::default(),
            raster: None,
            scene_dirty: true,
        }
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
            "{DEFAULT_CHARACTER_NAME} ready; Option+I/bubbles, Tab/next conversation, 1–9/manual, L/live, Space/pause, right-click/cycle, Esc/Q/quit"
        );
        Ok(())
    }

    fn fail(&mut self, _event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.cancel_navigation("view failed");
        self.error = Some(error);
    }

    fn close(&mut self, event_loop: &ActiveEventLoop) {
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
                self.paused = !self.paused;
                self.last_tick = Instant::now();
                self.resume_navigation(self.last_tick);
                self.next_demo = self.last_tick + DEMO_INTERVAL;
                info!(paused = self.paused, "Animation pause toggled");
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
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
        self.seeking() && self.animation_eligible(self.native_geometry_valid())
    }

    fn animation_eligible(&self, native_valid: bool) -> bool {
        native_valid && !self.paused && (self.navigation.is_some() || !self.occluded)
    }

    fn start_navigation(&mut self, now: Instant) {
        if self.close_requested || self.error.is_some() {
            return;
        }
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
        matches!(self.navigation, Some(Navigation::Seeking(_)))
            && !self.close_requested
            && self.error.is_none()
    }

    fn resume_navigation(&mut self, now: Instant) {
        if let Some(Navigation::Seeking(navigator)) = &mut self.navigation {
            navigator.resume(now);
            self.motion_clock.reset(now);
            self.native_motion = NativeMotion::default();
        }
    }

    fn cancel_navigation(&mut self, reason: &str) {
        if self.navigation.take().is_some() {
            self.motion_clock.clear();
            let base = self.display.resume();
            self.select(base);
            info!(familiar = %self.id, reason, "Pointer pursuit cancelled");
        }
    }

    fn source_animation(&mut self, view: Presentation) {
        if let Some(animation) = self.display.update(view)
            && self.navigation.is_none()
        {
            self.select(animation);
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
            PursuitStep::Move { delta, .. } => Some(delta),
            PursuitStep::Arrived => {
                self.navigation = Some(Navigation::Celebrating);
                self.motion_clock.clear();
                self.select(Animation::Succeeded);
                info!(familiar = %self.id, "Pointer pursuit arrived");
                None
            }
            PursuitStep::Blocked(reason) => {
                warn!(familiar = %self.id, reason, "Pointer pursuit blocked");
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
            info!(familiar = %self.id, ?facing, "Pointer pursuit direction changed");
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
            self.hidden_position = None;
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
        let show = self.visibility.visible(now);
        if show == self.bubble_visible {
            return Ok(());
        }
        let old_show = self.bubble_visible;
        self.scene_dirty = true;
        self.bubble_visible = show;
        if let Some(window) = self.window.clone() {
            let position = window.outer_position().ok();
            let physical_scale = self.options.scale * window.scale_factor();
            let anchored = position.map(|origin| {
                anchored_position(
                    origin,
                    old_show,
                    show,
                    self.options.sprite_scale,
                    physical_scale,
                )
            });
            // Restore the saved position, rather than interpreting a delayed
            // window-manager move as a user drag. A real drag clears the saved
            // anchor and commits the new sprite location instead.
            let desired = if show {
                anchored
            } else {
                self.hidden_position.or(anchored)
            };
            let dimensions = bubble::dimensions(show, self.options.sprite_scale);
            let desired = desired.map(|desired| {
                if show && let Some(monitor) = window.current_monitor() {
                    return clamp_to_monitor(
                        desired,
                        monitor.position(),
                        monitor.size().into(),
                        (
                            f64::from(dimensions.0) * physical_scale,
                            f64::from(dimensions.1) * physical_scale,
                        ),
                    );
                }
                desired
            });
            // Resize/reposition only; never activate or focus on a notification.
            let _ = window.request_inner_size(LogicalSize::new(
                f64::from(dimensions.0) * self.options.scale,
                f64::from(dimensions.1) * self.options.scale,
            ));
            if let Some(desired) = desired {
                window.set_outer_position(desired);
            }
            if show {
                self.hidden_position = position;
            } else {
                self.hidden_position = None;
            }
            let size = window.inner_size();
            self.resize(size.width, size.height)?;
            window.request_redraw();
            debug!(familiar = %self.id, position = ?window.outer_position().ok(),
                framebuffer_width = self.frame_size.0, framebuffer_height = self.frame_size.1,
                "Bubble layout applied");
        }
        info!(familiar = %self.id, visible = show, "Bubble visibility changed");
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
    ) {
        if self
            .window
            .as_ref()
            .is_none_or(|window| window.id() != window_id)
        {
            return;
        }
        match event {
            WindowEvent::CloseRequested => self.close(event_loop),
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
                        self.cycle();
                        self.reveal_bubbles(event_loop);
                    }
                    _ => {}
                }
            }
            WindowEvent::Resized(size) => {
                // Do not catch up hours of animation after a zero-sized window restores.
                self.last_tick = Instant::now();
                self.resume_navigation(self.last_tick);
                self.next_demo = self.last_tick + DEMO_INTERVAL;
                if size.width == 0 || size.height == 0 {
                    return;
                }
                if let Err(error) = self.resize(size.width, size.height) {
                    self.fail(event_loop, error);
                    return;
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
                        return;
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
    }

    fn tick(
        &mut self,
        event_loop: &ActiveEventLoop,
        now: Instant,
        cursor: Option<Point>,
    ) -> Option<Instant> {
        self.visibility.expire(now);
        if let Err(error) = self.sync_visibility(now) {
            self.fail(event_loop, error);
            return None;
        }
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let mut wake_at: Option<Instant> = None;
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
                if let Some(cursor) = cursor
                    && let Some(motion_elapsed) = self.motion_clock.take(now)
                {
                    self.pursue(now, cursor, motion_elapsed);
                    if let Some(window) = &self.window {
                        // Moving an unchanged local image does not dirty its pixels.
                        if self.scene_dirty {
                            window.request_redraw();
                        }
                    }
                }
            } else {
                self.motion_clock.clear();
                if let Some(Navigation::Seeking(navigator)) = &mut self.navigation {
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
    navigation_pending: Arc<RevealMailbox>,
    pointer: Option<PointerReader>,
    motion_clock: MotionClock,
    control: Option<FamiliarControl>,
    typeface: Option<Typeface>,
    initialized: bool,
    live: bool,
    initial_snapshot: bool,
    global_hotkey_registered: bool,
    exit_at: Option<Instant>,
    error: Option<anyhow::Error>,
    // Failed native views stay suppressed until the registry acknowledges removal.
    failed: BTreeSet<String>,
    pending_close: BTreeSet<String>,
    submitted_close: BTreeSet<String>,
}

impl App {
    fn new(atlas: SpriteAtlas, options: RunOptions, live: bool) -> Self {
        Self {
            views: BTreeMap::new(),
            atlas: Arc::new(atlas),
            options,
            mailbox: Arc::new(Mailbox::default()),
            reveal_pending: Arc::new(RevealMailbox::default()),
            navigation_pending: Arc::new(RevealMailbox::default()),
            pointer: None,
            motion_clock: MotionClock::default(),
            control: None,
            typeface: None,
            initialized: false,
            live,
            initial_snapshot: true,
            global_hotkey_registered: false,
            exit_at: None,
            error: None,
            failed: BTreeSet::new(),
            pending_close: BTreeSet::new(),
            submitted_close: BTreeSet::new(),
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

    // Reconcile registry state without creating native windows or touching the GPU.
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
                self.views.insert(id.clone(), view);
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
                let created = view.window.is_none();
                if view.window.is_none() {
                    if let Err(error) =
                        view.initialize(event_loop, self.typeface.as_ref().map(Typeface::fork))
                    {
                        view.error = Some(error);
                        continue;
                    }
                    if let (Some(position), Some(window)) = (cascade, &view.window) {
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
                }
                view.update(event_loop, presentation);
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
        // This mailbox is only posted with Some(ids), never a broadcast.
        let targets = reveal_targets(self.navigation_pending.take(), std::iter::empty());
        let now = Instant::now();
        for id in targets {
            if !self.pending_close.contains(&id)
                && !self.failed.contains(&id)
                && let Some(view) = self.views.get_mut(&id)
            {
                view.start_navigation(now);
            }
        }
    }
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
                    presentation: Presentation::default(),
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
        if let Some(view) = self.views.values_mut().find(|view| {
            view.window
                .as_ref()
                .is_some_and(|window| window.id() == window_id)
        }) {
            view.window_event(event_loop, window_id, event);
        }
        self.handle_errors(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.exit_at.is_some_and(|deadline| now >= deadline) {
            event_loop.exit();
            return;
        }
        let mut wake_at = self.exit_at;
        let needs_pointer = self.views.values().any(FamiliarView::motion_eligible);
        if !needs_pointer {
            self.motion_clock.clear();
        }
        let cursor = if needs_pointer && self.motion_clock.take(now).is_some() {
            match self
                .pointer
                .as_ref()
                .context("live pointer reader unavailable")
                .and_then(PointerReader::position)
            {
                Ok(point) => Some(point),
                Err(error) => {
                    for view in self.views.values_mut().filter(|view| view.seeking()) {
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
            if let Some(deadline) = view.tick(event_loop, now, cursor) {
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

fn anchored_position(
    origin: PhysicalPosition<i32>,
    old_show: bool,
    new_show: bool,
    sprite_scale: f32,
    physical_scale: f64,
) -> PhysicalPosition<i32> {
    let full = bubble::dimensions(true, sprite_scale);
    let sprite = bubble::dimensions(false, sprite_scale);
    let offset = |show| {
        if show {
            (
                (f64::from(full.0) - 192.0 * f64::from(sprite_scale)) / 2.0 * physical_scale,
                f64::from(full.1 - sprite.1) * physical_scale,
            )
        } else {
            (0.0, 0.0)
        }
    };
    let old = offset(old_show);
    let new = offset(new_show);
    PhysicalPosition::new(
        (f64::from(origin.x) + old.0 - new.0).round() as i32,
        (f64::from(origin.y) + old.1 - new.1).round() as i32,
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
        let crate::Command::Run(options) = crate::Cli::parse_from(["numen", "run"]).command else {
            unreachable!()
        };
        options
    }

    fn familiar(id: &str) -> FamiliarPresentation {
        FamiliarPresentation {
            id: id.into(),
            preset: DEFAULT_CHARACTER_ID.into(),
            presentation: Presentation::default(),
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
            let crate::Command::Run(mut options) = crate::Cli::parse_from(["numen", "run"]).command
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
