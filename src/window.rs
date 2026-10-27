use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pixels::wgpu::{BlendState, Color, CompositeAlphaMode};
use pixels::{Pixels, PixelsBuilder, ScalingMode, SurfaceTexture};
use tracing::{info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId, WindowLevel};

use crate::RunOptions;
use crate::animation::{Animation, Player};
use crate::assets::{CELL_HEIGHT, CELL_WIDTH, HexAtlas};

const DEMO_INTERVAL: Duration = Duration::from_secs(3);

pub fn run(atlas: HexAtlas, options: RunOptions) -> Result<()> {
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
    event_loop.run_app(&mut app)?;
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
    atlas: HexAtlas,
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
}

impl App {
    fn new(atlas: HexAtlas, options: RunOptions) -> Self {
        let now = Instant::now();
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
        }
    }

    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let attributes = Window::default_attributes()
            .with_title("Numen — Hex")
            .with_inner_size(LogicalSize::new(
                f64::from(CELL_WIDTH) * self.options.scale,
                f64::from(CELL_HEIGHT) * self.options.scale,
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
        let mut pixels = PixelsBuilder::new(CELL_WIDTH, CELL_HEIGHT, surface)
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
        info!(adapter = ?pixels.adapter().get_info(), "Graphics initialized");
        self.pixels = Some(pixels);
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
            animation = self.player.animation().label(),
            "Hex ready; click to focus, 1–9 expressions, Space pause, right-click cycle, Esc/Q quit"
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
        self.select(Animation::ALL[(self.selection_index + 1) % Animation::ALL.len()]);
    }

    fn keyboard(&mut self, event_loop: &ActiveEventLoop, code: KeyCode) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => event_loop.exit(),
            KeyCode::Space => {
                self.paused = !self.paused;
                self.last_tick = Instant::now();
                self.next_demo = self.last_tick + DEMO_INTERVAL;
                info!(paused = self.paused, "Animation pause toggled");
            }
            _ => {
                if let Some(animation) = animation_for_key(code) {
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
}

impl ApplicationHandler for App {
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
            WindowEvent::Resized(size) if size.width > 0 && size.height > 0 => {
                if let Some(pixels) = &mut self.pixels
                    && let Err(error) = pixels.resize_surface(size.width, size.height)
                {
                    self.fail(event_loop, error.into());
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
            WindowEvent::RedrawRequested if self.drawable() => {
                if let Some(pixels) = &mut self.pixels {
                    pixels.frame_mut().copy_from_slice(
                        self.atlas
                            .frame(self.player.animation(), self.player.frame_index()),
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
            let mut app = App::new(HexAtlas::load().unwrap(), options);
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
