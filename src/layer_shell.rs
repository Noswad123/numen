//! Linux software layer-shell backend with optional per-surface fractional scaling.
//! Global keyboard/navigation are not implemented.
//! Full-output coordinates ignore exclusive panel regions and reserve no space.
//! Familiars may overlap panels; ordering among surfaces in Layer::Top is undefined.

use std::{
    collections::BTreeMap,
    net::Shutdown,
    os::unix::net::UnixStream,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, SurfaceData},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_subcompositor,
    globals::ProvidesBoundGlobal,
    output::{OutputHandler, OutputInfo, OutputState},
    reexports::{
        calloop::{EventLoop, ping::make_ping},
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, EventQueue, Proxy, QueueHandle,
            backend::WaylandError,
            globals::registry_queue_init,
            protocol::{
                wl_compositor, wl_output, wl_pointer, wl_region, wl_seat, wl_shm, wl_subsurface,
                wl_surface,
            },
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{
            CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
    subcompositor::SubcompositorState,
};
use tracing::{debug, info, warn};

mod popup;
mod scaling;
mod software;
mod travel;
use crate::raster::{Density, RasterMode, RasterSpec};
use scaling::{ScalingGlobals, SurfaceScaling};
use software::{
    CloseRequest, Gesture, Reveals, clamp_origin, fallback_cursor, framebuffer_fits,
    initial_origin, input_rectangles, write_argb,
};

use crate::{
    RunOptions,
    activity::{FamiliarPresentation, PreferencePresentation},
    assets::{DEFAULT_CHARACTER_ID, SpriteAtlas, character},
    bubble_layout::{self, BubbleLayout},
    desktop_scene::FamiliarScene,
    diagnostics::{ShortcutReason, ShortcutState, ShortcutStatus},
    display::Mailbox,
    hotkey::Shortcut,
    ipc::{FamiliarControl, Server},
    preferences,
    protocol::FamiliarSettings,
    typography::Typeface,
};

pub(crate) struct Prepared {
    connection: Connection,
    queue: EventQueue<State>,
    state: State,
}

pub(crate) fn prepare() -> Result<Prepared> {
    let connection = Connection::connect_to_env().context("connect to existing Wayland display")?;
    // SO_RCVTIMEO does not bound a nonblocking Wayland backend. A duplicate of
    // this connection's socket lets the watchdog interrupt *both* roundtrips.
    let socket = UnixStream::from(connection.backend().poll_fd().try_clone_to_owned()?);
    let (cancel, wait) = mpsc::channel();
    let expired = Arc::new(AtomicBool::new(false));
    let timed_out = expired.clone();
    let watchdog = std::thread::spawn(move || {
        if matches!(
            wait.recv_timeout(Duration::from_secs(2)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            timed_out.store(true, Ordering::Release);
            let _ = socket.shutdown(Shutdown::Both);
        }
    });
    let result = (|| {
        let (globals, mut queue) = registry_queue_init::<State>(&connection)
            .context("layer-shell registry discovery (two-second deadline)")?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh)?;
        let raw: wl_compositor::WlCompositor = <CompositorState as ProvidesBoundGlobal<
            wl_compositor::WlCompositor,
            6,
        >>::with_min_version(&compositor, 4)?;
        let layers = LayerShell::bind(&globals, &qh)?;
        let shm = Shm::bind(&globals, &qh)?;
        let subs = SubcompositorState::bind(raw.clone(), &globals, &qh)?;
        let mut state = State {
            registry: RegistryState::new(&globals),
            seats: SeatState::new(&globals, &qh),
            outputs: OutputState::new(&globals, &qh),
            compositor,
            raw_compositor: raw,
            layers,
            xdg: smithay_client_toolkit::shell::xdg::XdgShell::bind(&globals, &qh).ok(),
            shm,
            subs,
            scaling: ScalingGlobals::bind(&globals, &qh),
            views: BTreeMap::new(),
            pointers: Vec::new(),
            snapshots: Arc::new(Mailbox::default()),
            reveals: Arc::new(Reveals::default()),
            quit: Arc::new(AtomicBool::new(false)),
            control: None,
            options: None,
            typeface: None,
            atlases: BTreeMap::new(),
            next_parent: 0,
        };
        queue
            .roundtrip(&mut state)
            .context("layer-shell protocol preflight (two-second deadline)")?;
        ensure!(
            state.shm.formats().contains(&wl_shm::Format::Argb8888),
            "Wayland SHM ARGB8888 unavailable"
        );
        Ok(Prepared {
            connection,
            queue,
            state,
        })
    })();
    let _ = cancel.send(());
    let _ = watchdog.join();
    ensure!(
        !expired.load(Ordering::Acquire),
        "layer-shell discovery exceeded two-second deadline"
    );
    result
}

impl Prepared {
    pub(crate) fn run(
        self,
        atlas: SpriteAtlas,
        options: RunOptions,
        socket: Option<&Path>,
    ) -> Result<()> {
        let Self {
            connection,
            queue,
            mut state,
        } = self;
        let qh = queue.handle();
        let mut event_loop = EventLoop::<State>::try_new()?;
        WaylandSource::new(connection.clone(), queue)
            .insert(event_loop.handle())
            .map_err(|e| anyhow::anyhow!("insert Wayland source: {e}"))?;
        let (ping, source) = make_ping()?;
        event_loop.handle().insert_source(source, |_, _, _| {})?;
        if !options.no_bubble {
            state.typeface = Some(
                Typeface::load(options.font_family.as_deref(), options.font_file.as_deref())
                    .context(
                        "cannot load conversation font; choose --font-family or --font-file",
                    )?,
            );
        }
        let exit_at = options.exit_after.map(|d| Instant::now() + d);
        state.atlases.insert(DEFAULT_CHARACTER_ID, Arc::new(atlas));
        state.options = Some(options.clone());
        let socket = socket.filter(|_| !options.manual && !options.demo);
        let server = if let Some(path) = socket {
            let snapshots = state.snapshots.clone();
            let wake_views = ping.clone();
            let reveals = state.reveals.clone();
            let wake_reveal = ping.clone();
            let quit = state.quit.clone();
            let wake_quit = ping.clone();
            let (path_preferences, reason) = if options.no_preferences {
                (None, None)
            } else if let Some(path) = &options.preferences_file {
                (Some(path.clone()), None)
            } else {
                match preferences::default_path(
                    std::env::var_os("XDG_CONFIG_HOME"),
                    std::env::var_os("HOME"),
                ) {
                    Ok(path) => (Some(path), None),
                    Err(error) => {
                        warn!(%error, "Preferences path unavailable");
                        (None, Some(preferences::error_reason(&error)))
                    }
                }
            };
            let server = Server::start_graphical_with_preferences(
                path,
                move |views| {
                    if snapshots.post(views) {
                        wake_views.ping();
                    }
                },
                move |ids| {
                    reveals.post(ids);
                    wake_reveal.ping();
                },
                None,
                move || {
                    quit.store(true, Ordering::Release);
                    wake_quit.ping();
                },
                !options.empty,
                FamiliarSettings {
                    sprite_scale: options.sprite_scale.into(),
                    font_size: options.font_size.into(),
                    bubble_duration: options.bubble_duration.as_secs_f64(),
                },
                path_preferences,
            )?;
            if let Some(reason) = reason {
                server
                    .diagnostics()
                    .set_preferences_status(preferences::Status::unavailable(reason));
            }
            for shortcut in [Shortcut::Reveal, Shortcut::GoToMouse, Shortcut::GoToCorner] {
                let (status, reason) = if options.no_hotkey {
                    (ShortcutState::Disabled, ShortcutReason::NoHotkey)
                } else if shortcut == Shortcut::Reveal && options.no_bubble {
                    (ShortcutState::Disabled, ShortcutReason::NoBubble)
                } else {
                    (
                        ShortcutState::Unsupported,
                        ShortcutReason::UnsupportedPlatform,
                    )
                };
                server.diagnostics().set_hotkey(
                    shortcut,
                    ShortcutStatus {
                        state: status,
                        reason: Some(reason),
                    },
                );
            }
            state.control = Some(server.familiar_control());
            info!(socket = %path.display(), "Local activity listener ready");
            Some(server)
        } else {
            state.add_scene("main".into(), DEFAULT_CHARACTER_ID, None)?;
            None
        };
        info!("Desktop backend selected backend=LayerShell");
        info!(
            side_button = if options.manual || options.demo {
                "preview"
            } else {
                "disabled in live mode"
            },
            "Layer-shell controls: left click reveal, drag move, middle click pause/resume, right click menu; optional per-surface fractional scaling; no global navigation or keyboard focus"
        );
        loop {
            let now = Instant::now();
            if state.quit.load(Ordering::Acquire) || exit_at.is_some_and(|at| now >= at) {
                break;
            }
            if let Some(snapshot) = state.snapshots.take() {
                state.reconcile(snapshot, now)?;
            }
            let reveals = state.reveals.take();
            for (id, view) in &mut state.views {
                if reveals
                    .as_ref()
                    .is_some_and(|ids| ids.as_ref().is_none_or(|ids| ids.contains(id)))
                {
                    view.scene.reveal(now);
                }
            }
            let mut deadline = exit_at;
            state.refresh_surfaces(&qh, now)?;
            state.service_popups(&qh, now, &mut deadline);
            let menu_held = state
                .views
                .values()
                .any(|v| v.native.as_ref().is_some_and(|n| n.popup.is_some()));
            for view in state.views.values_mut() {
                if view.native.is_none() && !view.failed {
                    earliest(
                        &mut deadline,
                        view.rejected_outputs.iter().map(|(_, at)| *at).min(),
                    );
                }
                if view.close.retry_at.is_some() {
                    if let Some(control) = &state.control {
                        view.close
                            .retry(now, || control.close(&view.scene.id).is_ok());
                    } else {
                        state.quit.store(true, Ordering::Release);
                    }
                    earliest(&mut deadline, view.close.retry_at);
                }
                if let Some(native) = &mut view.native {
                    match native.turn(
                        &mut view.scene,
                        now,
                        menu_held || view.closing,
                        &state.raw_compositor,
                        &qh,
                        &mut deadline,
                    ) {
                        Ok(false) => {
                            earliest(&mut deadline, Some(now + Duration::from_millis(100)))
                        }
                        Ok(true) => {}
                        Err(error) => {
                            warn!(familiar = %view.scene.id, %error, "Layer rendering failed");
                            native.cancel_corner(&mut view.scene, now);
                            fail_view(&mut view.failed, state.control.is_some(), error)?;
                            view.native = None;
                        }
                    }
                }
            }
            earliest(
                &mut deadline,
                flush_pending(Instant::now(), || connection.flush())?,
            );
            if state.quit.load(Ordering::Acquire) {
                break;
            }
            event_loop.dispatch(
                deadline.map(|at| at.saturating_duration_since(Instant::now())),
                &mut state,
            )?;
        }
        // Destroy child roles/surfaces before their parents, while the connection lives.
        state.views.clear();
        let _ = connection.flush();
        drop(server);
        Ok(())
    }
}

fn earliest(deadline: &mut Option<Instant>, other: Option<Instant>) {
    if let Some(other) = other {
        *deadline = Some(deadline.map_or(other, |at| at.min(other)));
    }
}

/// Only a live service has IPC controls and other views to keep alive. A
/// standalone session must return its original failure rather than become hidden.
fn fail_view(failed: &mut bool, live: bool, error: anyhow::Error) -> Result<()> {
    if !live {
        return Err(error);
    }
    *failed = true;
    Ok(())
}

/// One nonblocking attempt per event-loop turn. Even a paused, empty service
/// must wake to retry queued protocol writes when the compositor stops reading.
fn flush_pending(
    now: Instant,
    flush: impl FnOnce() -> std::result::Result<(), WaylandError>,
) -> std::result::Result<Option<Instant>, WaylandError> {
    match flush() {
        Ok(()) => Ok(None),
        Err(WaylandError::Io(error)) if error.kind() == std::io::ErrorKind::WouldBlock => {
            Ok(Some(now + Duration::from_millis(10)))
        }
        Err(error) => Err(error),
    }
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    compositor: CompositorState,
    raw_compositor: wl_compositor::WlCompositor,
    layers: LayerShell,
    xdg: Option<smithay_client_toolkit::shell::xdg::XdgShell>,
    shm: Shm,
    subs: SubcompositorState,
    scaling: ScalingGlobals,
    pointers: Vec<SeatPointer>,
    views: BTreeMap<String, View>,
    snapshots: Arc<Mailbox<Vec<FamiliarPresentation>>>,
    reveals: Arc<Reveals>,
    quit: Arc<AtomicBool>,
    control: Option<FamiliarControl>,
    options: Option<RunOptions>,
    typeface: Option<Typeface>,
    atlases: BTreeMap<&'static str, Arc<SpriteAtlas>>,
    next_parent: u64,
}

struct View {
    runtime_binding: Option<crate::activity::AttachmentBinding>,
    preference: Option<PreferencePresentation>,
    settings: FamiliarSettings,
    scene: FamiliarScene,
    native: Option<Native>,
    rejected_outputs: Vec<(wl_output::WlOutput, Instant)>,
    close: CloseRequest,
    closing: bool,
    failed: bool,
}

struct SeatPointer {
    seat: wl_seat::WlSeat,
    themed: ThemedPointer,
    fallback: Option<CursorFallback>,
}

struct CursorFallback {
    _pool: SlotPool,
    buffer: Buffer,
}

impl SeatPointer {
    fn default_cursor(&mut self, connection: &Connection, serial: u32, shm: &Shm) -> Result<()> {
        if self
            .themed
            .set_cursor(connection, CursorIcon::Default)
            .is_ok()
        {
            return Ok(());
        }
        if self.fallback.is_none() {
            let mut pool = SlotPool::new(16 * 24 * 4, shm)?;
            let (buffer, canvas) = pool.create_buffer(16, 24, 16 * 4, wl_shm::Format::Argb8888)?;
            write_argb(&fallback_cursor(), canvas, false);
            self.fallback = Some(CursorFallback {
                _pool: pool,
                buffer,
            });
        }
        let surface = self.themed.surface();
        surface.set_buffer_scale(1);
        // This immutable buffer is never recycled or modified. Reattaching it
        // while active is legal and needs no SlotPool activation bookkeeping.
        surface.attach(
            Some(self.fallback.as_ref().unwrap().buffer.wl_buffer()),
            0,
            0,
        );
        surface.damage_buffer(0, 0, 16, 24);
        surface.commit();
        self.themed
            .pointer()
            .set_cursor(serial, Some(surface), 0, 0);
        Ok(())
    }
}

impl State {
    fn add_scene(
        &mut self,
        id: String,
        preset: &str,
        settings: Option<FamiliarSettings>,
    ) -> Result<()> {
        let character = character(preset).context("unsupported familiar preset")?;
        let atlas = match self.atlases.entry(character.id) {
            std::collections::btree_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::btree_map::Entry::Vacant(e) => e
                .insert(Arc::new(SpriteAtlas::load_character(character.id)?))
                .clone(),
        };
        let mut scene = FamiliarScene::new(
            id.clone(),
            character,
            atlas,
            self.options.as_ref().unwrap().clone(),
            self.typeface.as_ref().map(Typeface::fork),
        );
        let settings = settings.unwrap_or_else(|| FamiliarSettings {
            sprite_scale: scene.options.sprite_scale.into(),
            font_size: scene.options.font_size.into(),
            bubble_duration: scene.options.bubble_duration.as_secs_f64(),
        });
        scene.apply_settings(settings)?;
        self.views.insert(
            id,
            View {
                runtime_binding: None,
                preference: None,
                settings,
                scene,
                native: None,
                rejected_outputs: Vec::new(),
                close: CloseRequest::default(),
                closing: false,
                failed: false,
            },
        );
        Ok(())
    }

    fn reconcile(&mut self, snapshot: Vec<FamiliarPresentation>, now: Instant) -> Result<()> {
        self.views
            .retain(|id, _| snapshot.iter().any(|v| &v.id == id));
        for familiar in snapshot {
            if self
                .views
                .get(&familiar.id)
                .is_some_and(|v| v.scene.character.id != familiar.preset)
            {
                self.views.remove(&familiar.id);
            }
            if !self.views.contains_key(&familiar.id) {
                self.add_scene(
                    familiar.id.clone(),
                    &familiar.preset,
                    Some(familiar.settings),
                )?;
            }
            let view = self.views.get_mut(&familiar.id).unwrap();
            if view.settings.sprite_scale != familiar.settings.sprite_scale {
                if let Some(n) = &mut view.native {
                    n.cancel_corner(&mut view.scene, now);
                } else {
                    view.scene.finish_corner(now);
                }
            }
            if (view.runtime_binding.as_ref() != Some(&familiar.runtime_binding)
                || view.settings != familiar.settings
                || view.preference.as_ref().map(|p| (&p.binding, p.writable))
                    != familiar
                        .preference
                        .as_ref()
                        .map(|p| (&p.binding, p.writable)))
                && let Some(native) = &mut view.native
            {
                native.popup = None;
            }
            view.runtime_binding = Some(familiar.runtime_binding);
            view.preference = familiar.preference;
            view.settings = familiar.settings;
            if view.scene.apply_settings(familiar.settings)? {
                view.failed = false;
            }
            view.scene.update(familiar.presentation, now);
        }
        Ok(())
    }

    fn refresh_surfaces(&mut self, qh: &QueueHandle<Self>, now: Instant) -> Result<()> {
        let outputs: Vec<_> = self.outputs.outputs().collect();
        for (index, view) in self.views.values_mut().enumerate() {
            if view.native.is_none() && view.scene.corner_active() {
                view.scene.finish_corner(now);
            }
            if view.failed {
                continue;
            }
            if view
                .native
                .as_ref()
                .is_some_and(|n| !outputs.contains(&n.output))
            {
                view.native
                    .as_mut()
                    .unwrap()
                    .cancel_corner(&mut view.scene, now);
                view.native = None;
            }
            view.rejected_outputs
                .retain(|(output, retry)| outputs.contains(output) && now < *retry);
            if view.native.is_some() {
                continue;
            }
            let Some(output) = outputs.iter().find(|o| {
                !view
                    .rejected_outputs
                    .iter()
                    .any(|(rejected, _)| rejected == *o)
            }) else {
                continue;
            };
            let info = self.outputs.info(output);
            let scale = info.as_ref().map_or(1, |i| i.scale_factor.max(1));
            let dimensions = info.as_ref().and_then(output_dimensions);
            let size = view.scene.sprite_size();
            let origin = initial_origin(dimensions, size, index);
            let pool = match SlotPool::new(4096, &self.shm) {
                Ok(pool) => pool,
                Err(error) => {
                    warn!(familiar = %view.scene.id, %error, "Cannot create layer buffer pool");
                    fail_view(&mut view.failed, self.control.is_some(), error.into())?;
                    continue;
                }
            };
            let parent = self.compositor.create_surface(qh);
            let sprite_scaling = SurfaceScaling::new(&self.scaling, &parent, scale, qh);
            let layer = self.layers.create_layer_surface(
                qh,
                parent,
                Layer::Top,
                Some("numen"),
                Some(output),
            );
            layer.set_anchor(Anchor::TOP | Anchor::LEFT);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            // -1 reserves nothing and makes margins relative to the full output,
            // matching output_dimensions, even when panels reserve exclusive space.
            layer.set_exclusive_zone(-1);
            layer.set_size(size.0, size.1);
            layer.set_margin(origin.1, 0, 0, origin.0);
            let (subsurface, child) = self.subs.create_subsurface(layer.wl_surface().clone(), qh);
            let mut bubble_scaling = SurfaceScaling::new(&self.scaling, &child, scale, qh);
            bubble_scaling
                .state
                .inherit_density(Some(sprite_scaling.state.requested_density()));
            subsurface.set_sync();
            subsurface.place_above(layer.wl_surface());
            let empty = self.raw_compositor.create_region(qh, ());
            child.set_input_region(Some(&empty));
            empty.destroy();
            child.commit();
            layer.commit();
            view.scene.sprite_dirty = true;
            view.scene.bubble_dirty = true;
            self.next_parent += 1;
            view.native = Some(Native {
                identity: self.next_parent,
                popup: None,
                travel: None,
                layer,
                child,
                subsurface,
                output: output.clone(),
                pool,
                sprite_buffers: Vec::new(),
                bubble_buffers: Vec::new(),
                sprite_scaling,
                bubble_scaling,
                origin,
                committed_origin: origin,
                output_size: dimensions,
                configured: false,
                frame_pending: false,
                buffers_blocked: false,
                layout_dirty: true,
                sprite_size: (0, 0),
                bubble_visible: false,
                bubble_layout: None,
                input_rectangles: 0,
                gesture: Gesture::default(),
            });
            info!(familiar = %view.scene.id, character = view.scene.character.id,
                side_button = if view.scene.options.manual || view.scene.options.demo { "preview" } else { "ignored in live mode" },
                "Familiar ready; left click reveal, drag move, middle click pause/resume, right click menu");
        }
        Ok(())
    }
}

struct StoredBuffer {
    size: (u32, u32),
    buffer: Buffer,
}

fn output_dimensions(info: &OutputInfo) -> Option<(i32, i32)> {
    info.logical_size
        .filter(|(w, h)| *w > 0 && *h > 0)
        .or_else(|| {
            info.modes.iter().find(|mode| mode.current).map(|mode| {
                let (mut w, mut h) = mode.dimensions;
                if matches!(
                    info.transform,
                    wl_output::Transform::_90
                        | wl_output::Transform::_270
                        | wl_output::Transform::Flipped90
                        | wl_output::Transform::Flipped270
                ) {
                    std::mem::swap(&mut w, &mut h);
                }
                (w / info.scale_factor.max(1), h / info.scale_factor.max(1))
            })
        })
        .filter(|(w, h)| *w > 0 && *h > 0)
}

// Never discard an active buffer to allocate another: even rapid resize with
// delayed wl_buffer.release remains bounded to three slots per surface.
fn available_buffer(
    pool: &mut SlotPool,
    buffers: &mut Vec<StoredBuffer>,
    size: (u32, u32),
) -> Result<Option<usize>> {
    ensure!(framebuffer_fits(size), "layer framebuffer too large");
    if let Some(index) = buffers
        .iter()
        .position(|b| b.size == size && pool.canvas(&b.buffer).is_some())
    {
        return Ok(Some(index));
    }
    if let Some(index) = buffers
        .iter()
        .position(|b| pool.canvas(&b.buffer).is_some())
    {
        buffers.remove(index);
    }
    if buffers.len() == 3 {
        return Ok(None);
    }
    let (buffer, _) = pool.create_buffer(
        size.0 as i32,
        size.1 as i32,
        size.0 as i32 * 4,
        wl_shm::Format::Argb8888,
    )?;
    buffers.push(StoredBuffer { size, buffer });
    Ok(Some(buffers.len() - 1))
}

struct Native {
    travel: Option<travel::CornerTravel>,
    identity: u64,
    popup: Option<popup::NativePopup>,
    layer: LayerSurface,
    child: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    output: wl_output::WlOutput,
    pool: SlotPool,
    sprite_buffers: Vec<StoredBuffer>,
    bubble_buffers: Vec<StoredBuffer>,
    sprite_scaling: SurfaceScaling,
    bubble_scaling: SurfaceScaling,
    origin: (i32, i32),
    // Last client-submitted margin, not absolute compositor position feedback.
    committed_origin: (i32, i32),
    output_size: Option<(i32, i32)>,
    configured: bool,
    frame_pending: bool,
    buffers_blocked: bool,
    layout_dirty: bool,
    sprite_size: (u32, u32),
    bubble_visible: bool,
    bubble_layout: Option<BubbleLayout>,
    input_rectangles: usize,
    gesture: Gesture,
}

impl Drop for Native {
    fn drop(&mut self) {
        self.popup = None;
        self.bubble_scaling.destroy();
        self.subsurface.destroy();
        self.child.destroy();
        self.sprite_scaling.destroy();
    }
}

impl Native {
    fn sync_density_hints(&mut self) {
        let density = Some(self.sprite_scaling.state.requested_density());
        self.bubble_scaling.state.inherit_density(density);
        if let Some(popup) = &mut self.popup {
            popup.scaling.state.inherit_density(density);
        }
    }

    fn request_corner(&mut self, scene: &mut FamiliarScene, now: Instant) -> Result<()> {
        let corner = [
            crate::corner::Corner::TopLeft,
            crate::corner::Corner::TopRight,
            crate::corner::Corner::BottomLeft,
            crate::corner::Corner::BottomRight,
        ][fastrand::usize(0..4)];
        self.request_corner_at(scene, now, corner)
    }

    fn request_corner_at(
        &mut self,
        scene: &mut FamiliarScene,
        now: Instant,
        corner: crate::corner::Corner,
    ) -> Result<()> {
        let output = self.output_size.context("output dimensions unavailable")?;
        self.travel = Some(
            travel::CornerTravel::new(
                corner,
                output,
                scene.sprite_size(),
                scene.options.move_speed,
                now,
            )
            .context("invalid corner geometry")?,
        );
        scene.begin_corner(now);
        info!(familiar = %scene.id, ?corner, "corner travel submitted");
        Ok(())
    }

    fn suspend_corner(&mut self, now: Instant) {
        // Only travel owns this pending origin. A drag cancels travel and must
        // retain its own unsubmitted margin, even while paused or opening menus.
        if travel::discard_pending(
            &mut self.travel,
            &mut self.origin,
            self.committed_origin,
            now,
        ) {
            self.layout_dirty = true;
        }
    }

    fn cancel_corner(&mut self, scene: &mut FamiliarScene, now: Instant) {
        self.suspend_corner(now);
        self.travel = None;
        scene.finish_corner(now);
    }

    fn update_output_size(
        &mut self,
        scene: &mut FamiliarScene,
        size: Option<(i32, i32)>,
        now: Instant,
    ) {
        if self.output_size == size {
            return;
        }
        self.cancel_corner(scene, now);
        self.popup = None;
        self.output_size = size;
        self.origin = clamp_origin(self.origin, size, scene.sprite_size());
        self.layout_dirty = true;
        self.gesture = Gesture::default();
    }

    fn advance_corner(&mut self, scene: &mut FamiliarScene, now: Instant, held: bool) {
        if held || scene.options.paused {
            self.suspend_corner(now);
        }
        let eligible = !held
            && !scene.options.paused
            && self.configured
            && self.sprite_size.0 > 0
            && !self.frame_pending
            && !self.buffers_blocked
            && self.origin == self.committed_origin;
        let Some(travel) = &mut self.travel else {
            return;
        };
        match travel.step(
            now,
            self.output_size,
            scene.sprite_size(),
            self.committed_origin,
            eligible,
        ) {
            travel::Step::Move(origin, facing) => {
                scene.face_corner(facing, now);
                if origin != self.origin {
                    self.origin = origin;
                    self.layout_dirty = true;
                }
            }
            result @ (travel::Step::Completed | travel::Step::Cancelled) => {
                if result == travel::Step::Completed {
                    info!(familiar = %scene.id, submitted_origin = ?self.committed_origin, "corner travel submitted/completed");
                } else {
                    debug!(familiar = %scene.id, "corner travel cancelled");
                }
                self.cancel_corner(scene, now);
            }
            travel::Step::Hold => {}
        }
    }

    fn turn(
        &mut self,
        scene: &mut FamiliarScene,
        now: Instant,
        held: bool,
        compositor: &wl_compositor::WlCompositor,
        qh: &QueueHandle<State>,
        deadline: &mut Option<Instant>,
    ) -> Result<bool> {
        self.advance_corner(scene, now, held);
        earliest(deadline, scene.tick(now));
        let ready = if self.configured && !self.frame_pending {
            let ready = self.present(scene, compositor, qh)?;
            self.buffers_blocked = !ready;
            ready
        } else {
            true
        };
        if ready
            && !held
            && !scene.options.paused
            && self.configured
            && !self.frame_pending
            && self.origin == self.committed_origin
            && let Some(travel) = &self.travel
        {
            earliest(deadline, travel.deadline());
        }
        Ok(ready)
    }

    fn present(
        &mut self,
        scene: &mut FamiliarScene,
        compositor: &wl_compositor::WlCompositor,
        qh: &QueueHandle<State>,
    ) -> Result<bool> {
        let size = scene.sprite_size();
        let origin = clamp_origin(self.origin, self.output_size, size);
        let preferred_bubble = scene.bubble_size();
        let layout = bubble_layout::layout(
            origin,
            size,
            preferred_bubble,
            scene.minimum_bubble_size(),
            self.output_size,
        );
        let bubble_size = layout.map_or(preferred_bubble, |layout| layout.size);
        let requested_visible = scene.bubble_visible();
        let visible = requested_visible && layout.is_some();
        let paint_layout_changed =
            self.bubble_layout.map(|l| (l.size, l.tail)) != layout.map(|l| (l.size, l.tail));
        let placement_changed = self.bubble_layout != layout;
        let sprite_spec = self
            .sprite_scaling
            .state
            .plan(size)
            .context("logical layer size exceeds raster budget even at scale one")?;
        let bubble_spec = self
            .bubble_scaling
            .state
            .plan(bubble_size)
            .context("logical bubble size exceeds raster budget even at scale one")?;
        let sprite_density_changed = Some(sprite_spec) != self.sprite_scaling.state.committed;
        let bubble_density_changed = Some(bubble_spec) != self.bubble_scaling.state.committed;
        let resized = self.sprite_size != size;
        if resized || self.origin != self.committed_origin {
            self.popup = None;
        }
        let sprite_dirty = scene.sprite_dirty || resized || sprite_density_changed;
        let bubble_dirty = scene.bubble_dirty
            || self.layout_dirty
            || resized
            || (visible && bubble_density_changed)
            || placement_changed
            || visible != self.bubble_visible;
        let bubble_pixels_dirty = visible
            && (scene.bubble_dirty
                || !self.bubble_visible
                || bubble_density_changed
                || paint_layout_changed);
        if !sprite_dirty && !bubble_dirty && !self.layout_dirty {
            return Ok(true);
        }
        let sprite_physical = sprite_spec.physical_size();
        let bubble_physical = bubble_spec.physical_size();
        let sprite_slot = if sprite_dirty {
            let Some(index) =
                available_buffer(&mut self.pool, &mut self.sprite_buffers, sprite_physical)?
            else {
                return Ok(false);
            };
            Some(index)
        } else {
            None
        };
        let bubble_slot = if bubble_pixels_dirty {
            let Some(index) =
                available_buffer(&mut self.pool, &mut self.bubble_buffers, bubble_physical)?
            else {
                return Ok(false);
            };
            Some(index)
        } else {
            None
        };
        let parent = self.layer.wl_surface();
        if resized {
            self.layer.set_size(size.0, size.1);
            self.origin = origin;
            self.layout_dirty = true;
            self.gesture = Gesture::default();
        }
        if self.layout_dirty {
            self.layer.set_margin(self.origin.1, 0, 0, self.origin.0);
        }
        if let Some(layout) = layout {
            self.subsurface.set_position(
                layout.origin.0 - self.origin.0,
                layout.origin.1 - self.origin.1,
            );
        }
        if let Some(index) = sprite_slot {
            let rgba = scene.render_sprite_raster(sprite_spec);
            let region = compositor.create_region(qh, ());
            let rectangles = input_rectangles(&rgba, sprite_spec);
            self.input_rectangles = rectangles.len();
            for (x, y, w, h) in rectangles {
                region.add(x, y, w, h);
            }
            parent.set_input_region(Some(&region));
            region.destroy();
            let buffer = &self.sprite_buffers[index].buffer;
            write_argb(
                &rgba,
                self.pool.canvas(buffer).unwrap(),
                scene.options.opaque,
            );
            buffer.attach_to(parent)?;
            self.sprite_scaling.apply(parent, sprite_spec);
            parent.damage_buffer(0, 0, sprite_physical.0 as i32, sprite_physical.1 as i32);
        }
        if bubble_dirty {
            if let Some(index) = bubble_slot {
                let rgba = scene
                    .render_bubble_raster(bubble_spec, layout.expect("visible bubble has layout"));
                let buffer = &self.bubble_buffers[index].buffer;
                write_argb(&rgba, self.pool.canvas(buffer).unwrap(), false);
                buffer.attach_to(&self.child)?;
                self.bubble_scaling.apply(&self.child, bubble_spec);
                self.child
                    .damage_buffer(0, 0, bubble_physical.0 as i32, bubble_physical.1 as i32);
            } else if !visible {
                self.child.attach(None, 0, 0);
                scene.bubble_dirty = false;
            }
            self.child.commit();
        }
        parent.frame(qh, parent.clone());
        self.frame_pending = true;
        // The parent commit applies the synchronized child's pixels, layout and
        // input region atomically, even when the sprite buffer did not change.
        self.layer.commit();
        if sprite_slot.is_some() {
            self.sprite_scaling.committed(sprite_spec, "sprite");
        }
        if bubble_slot.is_some() {
            self.bubble_scaling.committed(bubble_spec, "bubble");
        }
        debug!(familiar = %scene.id, logical_width = size.0, logical_height = size.1,
            bubble_visible = visible, input_rectangles = self.input_rectangles, raster_units = sprite_spec.density().units_120(), "Layer sprite presented");
        if bubble_dirty {
            debug!(familiar = %scene.id, visible, logical_width = bubble_size.0,
                logical_height = bubble_size.1, input_empty = true,
                requested_visible, suppressed = requested_visible && !visible,
                reflowed = bubble_size != preferred_bubble,
                tail_edge = ?layout.and_then(|l| l.tail).map(|t| t.edge),
                "Layer bubble presented");
        }
        self.sprite_size = size;
        self.committed_origin = self.origin;
        self.bubble_visible = visible;
        self.bubble_layout = layout;
        self.layout_dirty = false;
        Ok(true)
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        for view in self.views.values_mut() {
            if let Some(n) = &mut view.native {
                if n.layer.wl_surface() == surface {
                    n.sprite_scaling.state.integer_changed(factor);
                } else if &n.child == surface {
                    n.bubble_scaling.state.integer_changed(factor);
                } else if let Some(popup) = &mut n.popup
                    && popup.role.wl_surface() == surface
                {
                    popup.scaling.state.integer_changed(factor);
                }
                n.sync_density_hints();
            }
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        for view in self.views.values_mut() {
            if let Some(popup) = view.native.as_mut().and_then(|n| n.popup.as_mut())
                && popup.role.wl_surface() == surface
            {
                popup.frame();
            }
            if let Some(n) = &mut view.native
                && n.layer.wl_surface() == surface
            {
                n.frame_pending = false;
            }
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        for view in self.views.values_mut() {
            if view
                .native
                .as_ref()
                .is_some_and(|n| n.layer.wl_surface() == layer.wl_surface())
            {
                let mut n = view.native.take().unwrap();
                n.cancel_corner(&mut view.scene, Instant::now());
                let retry_at = Instant::now() + Duration::from_secs(1);
                view.rejected_outputs.push((n.output.clone(), retry_at));
                warn!(familiar = %view.scene.id, "Layer closed; trying another output or retrying after one second");
                // A closed layer can mean its output disappeared. Never mutate
                // the canonical familiar registry for a compositor-side close.
            }
        }
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        for view in self.views.values_mut() {
            if let Some(n) = &mut view.native
                && n.layer.wl_surface() == layer.wl_surface()
            {
                // Repeated configure events must not create a self-sustaining
                // commit/configure loop when no pixels or layout changed.
                n.layout_dirty |= !n.configured;
                n.configured = true;
            }
        }
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let info = self.outputs.info(&output);
        let factor = info.as_ref().map_or(1, |i| i.scale_factor.max(1));
        let size = info.as_ref().and_then(output_dimensions);
        for view in self.views.values_mut() {
            if let Some(n) = &mut view.native
                && n.output == output
            {
                n.update_output_size(&mut view.scene, size, Instant::now());
                n.sprite_scaling.state.output_changed(factor);
                n.bubble_scaling.state.output_changed(factor);
                if let Some(popup) = &mut n.popup {
                    popup.scaling.state.output_changed(factor);
                }
                n.sync_density_hints();
            }
        }
    }
    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        for view in self.views.values_mut() {
            if view.native.as_ref().is_some_and(|n| n.output == output) {
                view.native
                    .as_mut()
                    .unwrap()
                    .cancel_corner(&mut view.scene, Instant::now());
                view.native = None;
            }
            view.rejected_outputs.retain(|(o, _)| o != &output);
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && !self.pointers.iter().any(|p| p.seat == seat) {
            let surface = self.compositor.create_surface(qh);
            match self.seats.get_pointer_with_theme::<Self, SurfaceData>(
                qh,
                &seat,
                self.shm.wl_shm(),
                surface.clone(),
                ThemeSpec::System,
            ) {
                Ok(themed) => self.pointers.push(SeatPointer {
                    seat,
                    themed,
                    fallback: None,
                }),
                Err(error) => {
                    surface.destroy();
                    warn!(%error, "Layer-shell pointer unavailable");
                }
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.remove_pointer(&seat);
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.remove_pointer(&seat);
    }
}

impl State {
    fn remove_pointer(&mut self, seat: &wl_seat::WlSeat) {
        for pointer in self.pointers.iter().filter(|p| &p.seat == seat) {
            for view in self.views.values_mut() {
                if let Some(n) = &mut view.native {
                    if n.popup
                        .as_ref()
                        .is_some_and(|p| p.owner() == pointer.themed.pointer().id().protocol_id())
                    {
                        n.popup = None;
                    }
                    n.gesture
                        .cancel(pointer.themed.pointer().id().protocol_id());
                }
            }
        }
        // ThemedPointer destroys its cursor-shape device, releases wl_pointer,
        // and destroys the cursor surface exactly once on drop.
        self.pointers.retain(|p| &p.seat != seat);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.views.clear();
        self.pointers.clear();
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        connection: &Connection,
        qh: &QueueHandle<Self>,
        pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        let pointer_id = pointer.id().protocol_id();
        for event in events {
            // An active menu belongs to its opening pointer, including its parent
            // toggle and leave grace. Other seats cannot replace or interact with it.
            if !self.popup_accepts_pointer(pointer_id) {
                continue;
            }
            if let PointerEventKind::Enter { serial } = event.kind
                && let Some(cursor) = self
                    .pointers
                    .iter_mut()
                    .find(|p| p.themed.pointer() == pointer)
                && let Err(error) = cursor.default_cursor(connection, serial, &self.shm)
            {
                warn!(%error, "Cannot display layer-shell cursor");
            }
            if self.popup_pointer(pointer_id, event) {
                continue;
            }
            let parent_id = self
                .views
                .iter()
                .find(|(_, v)| {
                    v.native
                        .as_ref()
                        .is_some_and(|n| n.layer.wl_surface() == &event.surface)
                })
                .map(|(id, _)| id.clone());
            if let Some(id) = parent_id.as_deref() {
                if matches!(event.kind, PointerEventKind::Press { button: 0x111, .. }) {
                    let toggle = self.views[id]
                        .native
                        .as_ref()
                        .is_some_and(|n| n.popup.is_some());
                    self.dismiss_popups();
                    if !toggle
                        && let Err(error) = self.open_popup(id, event.position, pointer_id, qh)
                    {
                        warn!(%error, "Cannot open familiar menu");
                    }
                    continue;
                }
                self.popup_parent_pointer(id, pointer_id, &event.kind);
            }
            let Some(view) = self.views.values_mut().find(|v| {
                v.native
                    .as_ref()
                    .is_some_and(|n| n.layer.wl_surface() == &event.surface)
            }) else {
                continue;
            };
            let n = view.native.as_mut().unwrap();
            if !event.position.0.is_finite() || !event.position.1.is_finite() {
                n.gesture.cancel(pointer_id);
                continue;
            }
            let (origin, reveal) = pointer_gesture(
                &mut n.gesture,
                pointer_id,
                &event.kind,
                event.position,
                n.committed_origin,
            );
            if let Some(origin) = origin {
                n.cancel_corner(&mut view.scene, Instant::now());
                n.origin = clamp_origin(origin, n.output_size, view.scene.sprite_size());
                n.layout_dirty = true;
            }
            if reveal {
                view.scene.reveal(Instant::now());
            }
            match event.kind {
                PointerEventKind::Press { button: 0x112, .. } => {
                    view.scene.toggle_pause(Instant::now())
                }
                PointerEventKind::Press { button: 275, .. }
                    if view.scene.options.manual || view.scene.options.demo =>
                {
                    n.cancel_corner(&mut view.scene, Instant::now());
                    view.scene.cycle(Instant::now())
                }
                _ => {}
            }
        }
    }
}

fn pointer_gesture(
    gesture: &mut Gesture,
    pointer: u32,
    kind: &PointerEventKind,
    position: (f64, f64),
    committed_origin: (i32, i32),
) -> (Option<(i32, i32)>, bool) {
    match kind {
        PointerEventKind::Press { button: 0x110, .. } => {
            gesture.press(pointer, position, committed_origin)
        }
        // Only motion carries fresh surface-logical coordinates, independent of
        // buffer scale. Button events repeat SCTK's cached position.
        PointerEventKind::Motion { .. } => {
            return (gesture.moved(pointer, position, committed_origin), false);
        }
        PointerEventKind::Release { button: 0x110, .. } => return (None, gesture.release(pointer)),
        PointerEventKind::Leave { .. } => gesture.cancel(pointer),
        _ => {}
    }
    (None, false)
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
delegate_compositor!(State);
delegate_layer!(State);
delegate_output!(State);
delegate_pointer!(State);
delegate_registry!(State);
delegate_seat!(State);
delegate_shm!(State);
delegate_subcompositor!(State);
smithay_client_toolkit::reexports::client::delegate_noop!(State: ignore wl_region::WlRegion);

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io};

    #[test]
    fn standalone_pool_and_presentation_failures_return_original_errors_without_suppression() {
        for stage in ["pool", "presentation"] {
            let mut failed = false;
            let original = io::Error::new(io::ErrorKind::OutOfMemory, stage);
            let error = fail_view(&mut failed, false, original.into()).unwrap_err();
            let original = error
                .downcast::<io::Error>()
                .expect("original error type preserved");
            assert_eq!(original.kind(), io::ErrorKind::OutOfMemory);
            assert_eq!(original.to_string(), stage);
            assert!(
                !failed,
                "standalone {stage} failure must exit, not suppress the view"
            );
        }
    }

    #[test]
    fn live_pool_and_presentation_failures_suppress_only_the_failed_view() {
        for stage in ["pool", "presentation"] {
            let mut views_failed = [false; 2];
            let original = io::Error::new(io::ErrorKind::OutOfMemory, stage);
            fail_view(&mut views_failed[0], true, original.into()).unwrap();
            assert_eq!(
                views_failed,
                [true, false],
                "live {stage} failure must isolate this view"
            );
        }
    }

    #[test]
    fn cached_release_after_motion_and_commit_cannot_move_the_layer_twice() {
        let mut gesture = Gesture::default();
        let press = PointerEventKind::Press {
            time: 1,
            serial: 1,
            button: 0x110,
        };
        let motion = PointerEventKind::Motion { time: 2 };
        let release = PointerEventKind::Release {
            time: 3,
            serial: 2,
            button: 0x110,
        };
        assert_eq!(
            pointer_gesture(&mut gesture, 1, &press, (10.0, 10.0), (100, 100)),
            (None, false)
        );
        let cached_position = (20.0, 10.0);
        let (origin, reveal) =
            pointer_gesture(&mut gesture, 1, &motion, cached_position, (100, 100));
        assert!(!reveal);
        let committed = origin.unwrap();
        assert_eq!(committed, (110, 100));
        assert_eq!(
            pointer_gesture(&mut gesture, 1, &release, cached_position, committed),
            (None, false)
        );
    }

    #[test]
    fn flush_backpressure_schedules_one_bounded_retry_then_clears() {
        let calls = Cell::new(0);
        let now = Instant::now();
        let retry = flush_pending(now, || {
            calls.set(calls.get() + 1);
            Err(WaylandError::Io(io::ErrorKind::WouldBlock.into()))
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            calls.get(),
            1,
            "must not spin until the socket becomes writable"
        );
        assert!(retry > now && retry <= now + Duration::from_millis(10));
        let mut deadline = None; // Paused service, no frame or exit deadline.
        earliest(&mut deadline, Some(retry));
        assert_eq!(deadline, Some(retry));
        assert_eq!(
            flush_pending(retry, || {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .unwrap(),
            None
        );
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn flush_propagates_genuine_errors_without_retrying() {
        let calls = Cell::new(0);
        let error = flush_pending(Instant::now(), || {
            calls.set(calls.get() + 1);
            Err(WaylandError::Io(io::ErrorKind::BrokenPipe.into()))
        })
        .unwrap_err();
        assert!(
            matches!(error, WaylandError::Io(error) if error.kind() == io::ErrorKind::BrokenPipe)
        );
        assert_eq!(calls.get(), 1);
    }
}
