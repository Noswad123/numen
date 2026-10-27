use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pixels::wgpu::{BlendState, Color, CompositeAlphaMode};
use pixels::{Pixels, PixelsBuilder, ScalingMode, SurfaceTexture};
use tracing::{debug, info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId, WindowLevel};

use crate::RunOptions;
use crate::activity::Presentation;
use crate::animation::{Animation, Player};
use crate::assets::{DEFAULT_CHARACTER_ID, DEFAULT_CHARACTER_NAME, SpriteAtlas};
use crate::bubble;
use crate::display::{LiveDisplay, Mailbox};
use crate::ipc::{FocusControl, Server};
use crate::typography::Typeface;

const DEMO_INTERVAL: Duration = Duration::from_secs(3);

pub fn run(atlas: SpriteAtlas, options: RunOptions, socket: Option<&Path>) -> Result<()> {
    let mut builder = EventLoop::builder();
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
    let mut app = App::new(atlas, options);
    let mailbox = app.mailbox.clone();
    let proxy = event_loop.create_proxy();
    let server = socket
        .map(|path| {
            let server = Server::start(path, move |view| {
                if mailbox.post(view) {
                    let _ = proxy.send_event(());
                }
            })?;
            info!(socket = %path.display(), "Local activity listener ready");
            Ok::<_, anyhow::Error>(server)
        })
        .transpose()?;
    app.focus_control = server.as_ref().map(Server::focus_control);
    let result = event_loop.run_app(&mut app);
    drop(server);
    result?;
    info!(draws = app.draws, "Numen window closed");
    match app.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

struct App {
    // Drop the GPU surface before the window (the surface also holds its own Arc).
    pixels: Option<Pixels<'static>>,
    window: Option<Arc<Window>>,
    atlas: SpriteAtlas,
    player: Player,
    selection_index: usize,
    options: RunOptions,
    paused: bool,
    occluded: bool,
    last_tick: Instant,
    next_demo: Instant,
    exit_at: Option<Instant>,
    draws: u64,
    error: Option<anyhow::Error>,
    display: LiveDisplay,
    mailbox: Arc<Mailbox>,
    focus_control: Option<FocusControl>,
    latest_view: Presentation,
    renderer: Option<bubble::Renderer>,
    frame_size: (u32, u32),
}

impl App {
    fn new(atlas: SpriteAtlas, options: RunOptions) -> Self {
        let now = Instant::now();
        let mut display = LiveDisplay::default();
        if options.manual || options.demo {
            display.manual(options.animation);
        } else {
            display.on_advance(options.animation);
        }
        Self {
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
            exit_at: None,
            draws: 0,
            error: None,
            display,
            mailbox: Arc::new(Mailbox::default()),
            focus_control: None,
            latest_view: Presentation::default(),
            renderer: None,
            frame_size: (0, 0),
        }
    }

    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let typeface = if self.options.no_bubble {
            None
        } else {
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
            Some(face)
        };
        self.renderer = Some(bubble::Renderer::new(typeface, self.options.font_size));
        let (canvas_width, canvas_height) = bubble::dimensions(!self.options.no_bubble);
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
        info!(adapter = ?pixels.adapter().get_info(),
            framebuffer_width = size.width, framebuffer_height = size.height,
            display_scale = window.scale_factor(), "Graphics initialized");
        self.pixels = Some(pixels);
        self.frame_size = (size.width.max(1), size.height.max(1));
        self.last_tick = Instant::now();
        self.next_demo = self.last_tick + DEMO_INTERVAL;
        self.exit_at = self
            .options
            .exit_after
            .map(|duration| self.last_tick + duration);
        if self.options.focus {
            window.focus_window();
        }
        window.request_redraw();
        self.window = Some(window);
        info!(
            character = DEFAULT_CHARACTER_ID,
            animation = self.player.animation().label(),
            "{DEFAULT_CHARACTER_NAME} ready; Tab/next conversation, 1–9/manual, L/live, Space/pause, right-click/cycle, Esc/Q/quit"
        );
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.error = Some(error);
        event_loop.exit();
    }

    fn select(&mut self, animation: Animation) {
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
        info!(animation = animation.label(), "Expression selected");
    }

    fn cycle(&mut self) {
        let animation = Animation::ALL[(self.selection_index + 1) % Animation::ALL.len()];
        self.display.manual(animation);
        self.select(animation);
    }

    fn keyboard(&mut self, event_loop: &ActiveEventLoop, code: KeyCode) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
            KeyCode::Tab => {
                if let Some(control) = &self.focus_control {
                    match control.next() {
                        Ok(()) => {
                            let animation = self.display.resume();
                            self.select(animation);
                            info!("Requested next focused conversation");
                        }
                        Err(error) => warn!(%error, "Could not switch conversation"),
                    }
                }
            }
            KeyCode::KeyL if !self.options.demo && !self.options.manual => {
                let animation = self.display.resume();
                self.select(animation);
                info!("Live activity control resumed");
            }
            KeyCode::Space => {
                self.paused = !self.paused;
                self.last_tick = Instant::now();
                self.next_demo = self.last_tick + DEMO_INTERVAL;
                info!(paused = self.paused, "Animation pause toggled");
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {
                if let Some(animation) = animation_for_key(code) {
                    self.display.manual(animation);
                    self.select(animation);
                }
            }
        }
    }

    fn drawable(&self) -> bool {
        self.window.as_ref().is_some_and(|window| {
            let size = window.inner_size();
            !self.occluded && size.width > 0 && size.height > 0
        })
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        if let Some(pixels) = &mut self.pixels {
            pixels.resize_surface(width, height)?;
            pixels.resize_buffer(width, height)?;
            self.frame_size = (width, height);
        }
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, (): ()) {
        if let Some(view) = self.mailbox.take() {
            let represented = view
                .cue
                .as_ref()
                .map(|cue| &cue.selection)
                .or(view.base.as_ref());
            if let Some(selection) = represented {
                info!(source = %selection.source, activity = %selection.activity,
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
                info!("No live activity; resting");
                if let Some(window) = &self.window {
                    window.set_title(&format!("Numen — {DEFAULT_CHARACTER_NAME}"));
                }
            }
            self.latest_view = view.clone();
            if let Some(animation) = self.display.update(view) {
                self.select(animation);
            }
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
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.fail(event_loop, error);
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
            WindowEvent::CloseRequested => event_loop.exit(),
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
                        if let Some(window) = &self.window {
                            // Focus only after an explicit user interaction, never on a timer.
                            window.focus_window();
                            if let Err(error) = window.drag_window() {
                                warn!(%error, "Desktop does not support native window dragging");
                            }
                        }
                    }
                    MouseButton::Right => self.cycle(),
                    _ => {}
                }
            }
            WindowEvent::Resized(size) => {
                // Do not catch up hours of animation after a zero-sized window restores.
                self.last_tick = Instant::now();
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
                self.last_tick = Instant::now();
                self.next_demo = self.last_tick + DEMO_INTERVAL;
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
                    renderer.compose(
                        pixels.frame_mut(),
                        self.frame_size,
                        self.atlas
                            .frame(self.player.animation(), self.player.frame_index()),
                        &self.latest_view,
                        self.display.is_manual().then_some(self.player.animation()),
                        !self.options.no_bubble,
                    );
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.exit_at.is_some_and(|deadline| now >= deadline) {
            event_loop.exit();
            return;
        }
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let mut wake_at = self.exit_at;
        if self.drawable() && !self.paused {
            let changed = self.player.advance(elapsed, &self.atlas);
            if let Some(animation) = self.display.on_advance(self.player.animation()) {
                self.select(animation);
            }
            if self.options.demo && now >= self.next_demo {
                // Use a separate cursor: one-shot clips return to idle before
                // their demo interval ends, so current animation is not a cursor.
                self.cycle();
            } else if changed && let Some(window) = &self.window {
                window.request_redraw();
            }
            let frame_at = now + self.player.until_next_frame(&self.atlas);
            wake_at = Some(wake_at.map_or(frame_at, |deadline| deadline.min(frame_at)));
            if self.options.demo {
                wake_at = wake_at.map(|deadline| deadline.min(self.next_demo));
            }
        }
        event_loop.set_control_flow(wake_at.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
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

#[cfg(test)]
mod tests {
    use super::*;

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
            let mut app = App::new(SpriteAtlas::load().unwrap(), options);
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
