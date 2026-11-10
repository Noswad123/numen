//! Ungrabbed, pointer-only menus. No outside-click guarantee or keyboard focus.
use super::*;
use crate::{
    activity::{AttachmentBinding, PreferenceBinding},
    context_menu::Action,
    pointer_menu::{self, FrozenAttachment, MenuResult, Model},
};
use smithay_client_toolkit::{
    delegate_xdg_popup, delegate_xdg_shell,
    reexports::protocols::xdg::shell::client::xdg_positioner::{
        Anchor as PopupAnchor, ConstraintAdjustment, Gravity,
    },
    shell::xdg::{
        XdgPositioner,
        popup::{Popup, PopupConfigure, PopupHandler},
    },
};

pub(super) struct NativePopup {
    pub role: Popup,
    input: MenuInput,
    font: Typeface,
    pool: SlotPool,
    buffers: Vec<StoredBuffer>,
    size: (u32, u32),
    pub(super) scaling: SurfaceScaling,
    configured: bool,
    pub frame_pending: bool,
    dirty: bool,
    configure_by: Instant,
    leave_by: Option<Instant>,
    idle_by: Instant,
    bindings: MenuBindings,
    parent: u64,
}

#[derive(Default)]
struct MenuBindings {
    binding: Option<AttachmentBinding>,
    frozen: Option<FrozenAttachment>,
    preference: Option<PreferenceBinding>,
}

fn writable_preference(preference: &Option<PreferencePresentation>) -> Option<&PreferenceBinding> {
    preference
        .as_ref()
        .filter(|p| p.writable)
        .map(|p| &p.binding)
}

struct MenuInput {
    model: Model,
    owner: u32,
    // Hover commits retain the revision; only a frame of the current layout
    // permits selection, even if an older frame completes after configure.
    layout_revision: u64,
    committed_revision: Option<u64>,
    interactive_revision: Option<u64>,
}

impl MenuInput {
    fn new(model: Model, owner: u32) -> Self {
        Self {
            model,
            owner,
            layout_revision: 0,
            committed_revision: None,
            interactive_revision: None,
        }
    }

    fn invalidate(&mut self) {
        self.layout_revision += 1;
        self.model.pointer_press(0x110, (-1.0, -1.0));
    }

    fn configure(&mut self, size: (u32, u32)) {
        self.model.configure(size);
        self.invalidate();
    }

    fn committed(&mut self) {
        self.committed_revision = Some(self.layout_revision);
    }

    fn frame(&mut self) {
        self.interactive_revision = self.committed_revision;
    }

    fn pointer(
        &mut self,
        pointer: u32,
        kind: &PointerEventKind,
        position: (f64, f64),
    ) -> (MenuResult, bool) {
        if pointer != self.owner {
            return (MenuResult::None, false);
        }
        let interactive = self.interactive_revision == Some(self.layout_revision);
        let result = match *kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                return (MenuResult::None, self.model.pointer_motion(position));
            }
            PointerEventKind::Leave { .. } => {
                self.model.pointer_press(0x110, (-1.0, -1.0));
                return (MenuResult::None, self.model.pointer_motion((-1.0, -1.0)));
            }
            PointerEventKind::Press { button: 0x111, .. } => MenuResult::Dismiss,
            PointerEventKind::Press { button, .. } if interactive => {
                self.model.pointer_press(button, position);
                MenuResult::None
            }
            PointerEventKind::Release { button, .. } if interactive => {
                self.model.pointer_release(button, position)
            }
            PointerEventKind::Press { .. } | PointerEventKind::Release { .. } => {
                // A press while blocked must never survive the replacement frame.
                self.model.pointer_press(0x110, (-1.0, -1.0));
                MenuResult::None
            }
            _ => MenuResult::None,
        };
        if result == MenuResult::Changed {
            self.invalidate();
        }
        let dirty = result == MenuResult::Changed;
        (result, dirty)
    }
}

fn expired(
    now: Instant,
    configured: bool,
    configure_by: Instant,
    leave_by: Option<Instant>,
    idle_by: Instant,
) -> bool {
    (!configured && now >= configure_by) || leave_by.is_some_and(|at| now >= at) || now >= idle_by
}

fn action_is_current(
    parent: Option<u64>,
    expected: u64,
    binding: &Option<AttachmentBinding>,
    applied: &Option<AttachmentBinding>,
    closing: bool,
) -> bool {
    parent == Some(expected) && binding == applied && !closing
}

impl NativePopup {
    pub(super) fn owner(&self) -> u32 {
        self.input.owner
    }

    pub(super) fn frame(&mut self) {
        self.frame_pending = false;
        self.input.frame();
    }

    fn present(
        &mut self,
        compositor: &wl_compositor::WlCompositor,
        qh: &QueueHandle<State>,
    ) -> Result<bool> {
        let spec = self
            .scaling
            .state
            .plan(self.size)
            .context("menu exceeds raster budget")?;
        if !self.configured
            || self.frame_pending
            || (!self.dirty && self.scaling.state.committed == Some(spec))
        {
            return Ok(true);
        }
        let physical = spec.physical_size();
        let Some(index) = available_buffer(&mut self.pool, &mut self.buffers, physical)? else {
            return Ok(false);
        };
        let rgba = self.input.model.paint_raster(&mut self.font, spec);
        ensure!(
            rgba.len() == physical.0 as usize * physical.1 as usize * 4,
            "invalid menu raster"
        );
        let surface = self.role.wl_surface();
        let region = compositor.create_region(qh, ());
        let (x, y, w, h) = self.input.model.input_rect();
        if w > 0 && h > 0 {
            region.add(x, y, w, h);
        }
        surface.set_input_region(Some(&region));
        region.destroy();
        self.role
            .xdg_surface()
            .set_window_geometry(0, 0, self.size.0 as i32, self.size.1 as i32);
        let buffer = &self.buffers[index].buffer;
        write_argb(&rgba, self.pool.canvas(buffer).unwrap(), false);
        buffer.attach_to(surface)?;
        self.scaling.apply(surface, spec);
        surface.damage_buffer(0, 0, physical.0 as i32, physical.1 as i32);
        surface.frame(qh, surface.clone());
        surface.commit();
        self.scaling.committed(spec, "popup");
        self.input.committed();
        self.frame_pending = true;
        self.dirty = false;
        Ok(true)
    }
}

impl Drop for NativePopup {
    fn drop(&mut self) {
        self.scaling.destroy();
    }
}

impl State {
    pub(super) fn popup_accepts_pointer(&self, pointer: u32) -> bool {
        self.views
            .values()
            .filter_map(|v| v.native.as_ref()?.popup.as_ref())
            .all(|p| p.owner() == pointer)
    }

    pub(super) fn dismiss_popups(&mut self) {
        for view in self.views.values_mut() {
            if let Some(n) = &mut view.native {
                n.popup = None;
            }
        }
    }

    pub(super) fn open_popup(
        &mut self,
        id: &str,
        point: (f64, f64),
        owner: u32,
        qh: &QueueHandle<Self>,
    ) -> Result<()> {
        if !self.popup_accepts_pointer(owner) {
            return Ok(());
        }
        self.dismiss_popups();
        ensure!(
            point.0.is_finite() && point.1.is_finite(),
            "invalid menu position"
        );
        let shell = self.xdg.as_ref().context("XDG shell unavailable")?;
        let open_familiar = self
            .control
            .as_ref()
            .map(|_| self.views.len() < crate::protocol::MAX_FAMILIARS);
        let view = self.views.get_mut(id).context("familiar removed")?;
        ensure!(!view.closing, "familiar closing");
        let native = view.native.as_mut().context("parent unavailable")?;
        ensure!(
            native.configured && native.sprite_size.0 > 0,
            "parent not mapped"
        );
        let font = match &self.typeface {
            Some(face) => face.fork(),
            None => Typeface::bundled()?,
        };
        let (attachments, frozen) = if let Some(binding) = &view.runtime_binding {
            let snapshot = self
                .control
                .as_ref()
                .and_then(|c| c.target_snapshot(id).ok());
            let menu = pointer_menu::AttachmentMenu::new(id, binding, snapshot);
            (Some(menu.context), menu.frozen)
        } else {
            (None, None)
        };
        let preference = writable_preference(&view.preference).cloned();
        let model = Model::new(crate::context_menu::Context {
            paused: view.scene.options.paused,
            bubbles: !view.scene.options.no_bubble,
            copy_session_id: false,
            open_chat: false,
            navigation: false,
            corner_navigation: self.control.is_some()
                && !view.scene.options.manual
                && !view.scene.options.demo
                && native.configured
                && native.sprite_size.0 > 0
                && native.output_size.is_some_and(|(w, h)| w > 0 && h > 0),
            settings: Some(view.settings),
            preferences: preference.is_some(),
            attachments,
            open_familiar,
        });
        let size = model.size();
        let pool = SlotPool::new(4096, &self.shm)?;
        let positioner = XdgPositioner::new(shell)?;
        positioner.set_size(size.0 as i32, size.1 as i32);
        positioner.set_anchor_rect(
            point
                .0
                .clamp(0.0, f64::from(native.sprite_size.0.saturating_sub(1))) as i32,
            point
                .1
                .clamp(0.0, f64::from(native.sprite_size.1.saturating_sub(1))) as i32,
            1,
            1,
        );
        positioner.set_anchor(PopupAnchor::BottomRight);
        positioner.set_gravity(Gravity::BottomRight);
        positioner.set_constraint_adjustment(
            ConstraintAdjustment::FlipX
                | ConstraintAdjustment::FlipY
                | ConstraintAdjustment::SlideX
                | ConstraintAdjustment::SlideY
                | ConstraintAdjustment::ResizeX
                | ConstraintAdjustment::ResizeY,
        );
        let surface = self.compositor.create_surface(qh);
        let role = Popup::from_surface(None, &positioner, qh, surface, shell)?;
        let mut scaling = SurfaceScaling::new(
            &self.scaling,
            role.wl_surface(),
            native.sprite_scaling.state.fallback(),
            qh,
        );
        scaling
            .state
            .inherit_density(Some(native.sprite_scaling.state.requested_density()));
        // Layer-shell parenting must precede the first (empty) commit.
        native.layer.get_popup(role.xdg_popup());
        role.wl_surface().commit();
        let now = Instant::now();
        native.popup = Some(NativePopup {
            role,
            input: MenuInput::new(model, owner),
            font,
            pool,
            buffers: Vec::new(),
            size,
            scaling,
            configured: false,
            frame_pending: false,
            dirty: true,
            configure_by: now + Duration::from_secs(2),
            leave_by: None,
            idle_by: now + Duration::from_secs(10),
            bindings: MenuBindings {
                binding: view.runtime_binding.clone(),
                frozen,
                preference,
            },
            parent: native.identity,
        });
        // Menu ownership suspends sprite input globally, so releases from other
        // pointers may be filtered out. Cancel their held gestures first.
        for view in self.views.values_mut() {
            if let Some(native) = &mut view.native {
                native.gesture = Gesture::default();
                native.suspend_corner(now);
            }
        }
        Ok(())
    }

    pub(super) fn service_popups(
        &mut self,
        qh: &QueueHandle<Self>,
        now: Instant,
        deadline: &mut Option<Instant>,
    ) {
        for view in self.views.values_mut() {
            let Some(n) = &mut view.native else {
                continue;
            };
            let Some(p) = &mut n.popup else {
                continue;
            };
            if expired(now, p.configured, p.configure_by, p.leave_by, p.idle_by)
                || n.origin != n.committed_origin
                || n.sprite_size != view.scene.sprite_size()
                || p.bindings.binding != view.runtime_binding
                || p.bindings.preference.as_ref() != writable_preference(&view.preference)
                || view.closing
            {
                if !p.configured {
                    warn!("Menu configure timed out");
                }
                n.popup = None;
                continue;
            }
            earliest(deadline, Some(p.idle_by));
            earliest(deadline, p.leave_by);
            if !p.configured {
                earliest(deadline, Some(p.configure_by));
            }
            match p.present(&self.raw_compositor, qh) {
                Ok(false) => earliest(deadline, Some(now + Duration::from_millis(20))),
                Ok(true) => {}
                Err(error) => {
                    warn!(%error, "Menu rendering failed");
                    n.popup = None;
                }
            }
        }
    }

    pub(super) fn popup_parent_pointer(&mut self, id: &str, pointer: u32, kind: &PointerEventKind) {
        if !self.popup_accepts_pointer(pointer) {
            return;
        }
        if matches!(kind, PointerEventKind::Press { .. }) {
            self.dismiss_popups();
            return;
        }
        if let Some(p) = self
            .views
            .get_mut(id)
            .and_then(|v| v.native.as_mut())
            .and_then(|n| n.popup.as_mut())
        {
            match kind {
                PointerEventKind::Leave { .. } => {
                    p.leave_by = Some(Instant::now() + Duration::from_millis(350))
                }
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    p.leave_by = None;
                    p.idle_by = Instant::now() + Duration::from_secs(10);
                }
                _ => {}
            }
        }
    }

    pub(super) fn popup_pointer(&mut self, pointer: u32, event: &PointerEvent) -> bool {
        let Some(id) = self
            .views
            .iter()
            .find(|(_, v)| {
                v.native
                    .as_ref()
                    .and_then(|n| n.popup.as_ref())
                    .is_some_and(|p| p.role.wl_surface() == &event.surface)
            })
            .map(|(id, _)| id.clone())
        else {
            return false;
        };
        let p = self
            .views
            .get_mut(&id)
            .unwrap()
            .native
            .as_mut()
            .unwrap()
            .popup
            .as_mut()
            .unwrap();
        if p.owner() != pointer {
            return true;
        }
        let now = Instant::now();
        p.idle_by = now + Duration::from_secs(10);
        if !p.configured {
            return true;
        }
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                p.leave_by = None;
            }
            PointerEventKind::Leave { .. } => {
                p.leave_by = Some(now + Duration::from_millis(350));
            }
            _ => {}
        }
        let (result, dirty) = p.input.pointer(pointer, &event.kind, event.position);
        p.dirty |= dirty;
        match result {
            MenuResult::Changed => p.dirty = true,
            MenuResult::Dismiss | MenuResult::Action(_) => {
                let mut popup = self
                    .views
                    .get_mut(&id)
                    .unwrap()
                    .native
                    .as_mut()
                    .unwrap()
                    .popup
                    .take()
                    .unwrap();
                let parent = popup.parent;
                let bindings = std::mem::take(&mut popup.bindings);
                drop(popup);
                // All native popup resources are gone before reconciliation/dispatch.
                if let MenuResult::Action(action) = result
                    && let Err(error) = self.popup_action(&id, parent, bindings, action, now)
                {
                    warn!(%error, "Menu action rejected");
                }
            }
            MenuResult::None => {}
        }
        true
    }

    fn popup_action(
        &mut self,
        id: &str,
        parent: u64,
        bindings: MenuBindings,
        action: Action,
        now: Instant,
    ) -> Result<()> {
        if let Some(snapshot) = self.snapshots.take() {
            self.reconcile(snapshot, now)?;
        }
        match action {
            Action::QuitNumen => {
                self.quit.store(true, Ordering::Release);
                return Ok(());
            }
            Action::OpenFamiliar => {
                let control = self
                    .control
                    .as_ref()
                    .context("opening familiars unavailable")?;
                ensure!(
                    self.views.len() < crate::protocol::MAX_FAMILIARS,
                    "familiar capacity reached"
                );
                return control.open_unattached(crate::assets::RANDOM_PRESET);
            }
            _ => {}
        }
        let view = self.views.get_mut(id).context("familiar removed")?;
        ensure!(
            action_is_current(
                view.native.as_ref().map(|n| n.identity),
                parent,
                &bindings.binding,
                &view.runtime_binding,
                view.closing || view.failed
            ),
            "stale menu action"
        );
        ensure!(
            view.native.as_ref().is_some_and(
                |n| n.sprite_size == view.scene.sprite_size() && n.origin == n.committed_origin
            ),
            "parent geometry changed"
        );
        match action {
            Action::GoToCorner => {
                ensure!(
                    self.control.is_some()
                        && !view.scene.options.manual
                        && !view.scene.options.demo,
                    "corner navigation unavailable"
                );
                view.native
                    .as_mut()
                    .context("parent unavailable")?
                    .request_corner(&mut view.scene, now)?;
            }
            Action::RevealStatus => {
                ensure!(!view.scene.options.no_bubble, "status bubbles unavailable");
                view.scene.reveal(now);
            }
            Action::SetPaused(paused) => view.scene.set_paused(paused, now),
            Action::CloseFamiliar => {
                if let Some(n) = &mut view.native {
                    n.cancel_corner(&mut view.scene, now);
                } else {
                    view.scene.finish_corner(now);
                }
                view.closing = true;
                view.close.request(now);
            }
            Action::Settings(patch) => {
                patch.validate()?;
                if let Some(control) = &self.control {
                    control.settings(id, patch)?;
                } else {
                    let settings = patch.apply(view.settings);
                    settings.validate()?;
                    if settings.sprite_scale != view.settings.sprite_scale {
                        if let Some(n) = &mut view.native {
                            n.cancel_corner(&mut view.scene, now);
                        } else {
                            view.scene.finish_corner(now);
                        }
                    }
                    view.scene.apply_settings(settings)?;
                    view.settings = settings;
                }
            }
            Action::Attach(index) => {
                let control = self.control.as_ref().context("attachment unavailable")?;
                let fresh = control.target_snapshot(id).ok();
                bindings.frozen.context("attachment unavailable")?.queue(
                    id,
                    index,
                    bindings.binding.as_ref().context("binding unavailable")?,
                    fresh.as_ref(),
                    |expected, choice| {
                        control.attach(id, expected, &choice.target, choice.incarnation)
                    },
                )?;
            }
            Action::ForgetPreferences => {
                let binding = bindings
                    .preference
                    .as_ref()
                    .context("preferences unavailable")?;
                ensure!(
                    writable_preference(&view.preference) == Some(binding),
                    "stale preference binding"
                );
                self.control
                    .as_ref()
                    .context("preferences unavailable")?
                    .forget_preferences(id, binding)?;
            }
            Action::CopySessionId | Action::OpenChat | Action::GoToMouse => {
                anyhow::bail!("menu action unsupported on layer-shell");
            }
            Action::OpenFamiliar | Action::QuitNumen => {
                unreachable!("service action handled above")
            }
        }
        Ok(())
    }
}

// SCTK's XDG delegation includes decoration callbacks. This backend never
// constructs an XDG toplevel, so only the popup handler can receive events.
impl smithay_client_toolkit::shell::xdg::window::WindowHandler for State {
    fn request_close(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &smithay_client_toolkit::shell::xdg::window::Window,
    ) {
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &smithay_client_toolkit::shell::xdg::window::Window,
        _: smithay_client_toolkit::shell::xdg::window::WindowConfigure,
        _: u32,
    ) {
    }
}

impl PopupHandler for State {
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        role: &Popup,
        config: PopupConfigure,
    ) {
        for view in self.views.values_mut() {
            let Some(n) = &mut view.native else {
                continue;
            };
            let Some(p) = &mut n.popup else {
                continue;
            };
            if &p.role != role {
                continue;
            }
            if config.width <= 0
                || config.height <= 0
                || config.width > 8192
                || config.height > 8192
                || p.scaling
                    .state
                    .plan((config.width as u32, config.height as u32))
                    .is_none()
            {
                warn!("Invalid menu configure");
                n.popup = None;
                continue;
            }
            p.size = (config.width as u32, config.height as u32);
            p.input.configure(p.size);
            p.configured = true;
            p.dirty = true;
        }
    }
    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, role: &Popup) {
        for view in self.views.values_mut() {
            if let Some(n) = &mut view.native
                && n.popup.as_ref().is_some_and(|p| &p.role == role)
            {
                n.popup = None;
            }
        }
    }
}

delegate_xdg_shell!(State);
delegate_xdg_popup!(State);

#[cfg(test)]
mod tests {
    use super::*;
    use smithay_client_toolkit::reexports::{
        client::Dispatch,
        protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
            self, WpFractionalScaleV1,
        },
    };

    fn inject_parent_density(
        state: &mut State,
        id: &str,
        units: u32,
        connection: &Connection,
        qh: &QueueHandle<State>,
    ) {
        let native = state.views[id].native.as_ref().unwrap();
        if let Some(fractional) = native.sprite_scaling.fractional().cloned() {
            let surface = native.layer.wl_surface().clone();
            <State as Dispatch<WpFractionalScaleV1, wl_surface::WlSurface>>::event(
                state,
                &fractional,
                wp_fractional_scale_v1::Event::PreferredScale { scale: units },
                &surface,
                connection,
                qh,
            );
        }
    }

    fn button(input: &mut MenuInput, pointer: u32, row: usize, press: bool) -> MenuResult {
        let kind = if press {
            PointerEventKind::Press {
                button: 0x110,
                time: 0,
                serial: 0,
            }
        } else {
            PointerEventKind::Release {
                button: 0x110,
                time: 0,
                serial: 0,
            }
        };
        input
            .pointer(pointer, &kind, (40.0, 19.0 + row as f64 * 30.0))
            .0
    }

    fn click(input: &mut MenuInput, row: usize) -> MenuResult {
        assert_eq!(button(input, input.owner, row, true), MenuResult::None);
        button(input, input.owner, row, false)
    }

    fn presented(input: &mut MenuInput) {
        input.committed();
        input.frame();
    }

    fn input() -> MenuInput {
        MenuInput::new(Model::new(context()), 1)
    }

    fn context() -> crate::context_menu::Context {
        crate::context_menu::Context {
            paused: false,
            bubbles: true,
            copy_session_id: false,
            open_chat: false,
            navigation: false,
            corner_navigation: false,
            settings: Some(FamiliarSettings::default()),
            preferences: false,
            attachments: None,
            open_familiar: None,
        }
    }

    fn row(point: (f64, f64)) -> usize {
        ((point.1 - 19.0) / 30.0) as usize
    }

    fn action_row(input: &MenuInput, action: Action) -> usize {
        row(input
            .model
            .visible_position_for_action(action)
            .expect("visible enabled action"))
    }

    fn navigate(input: &mut MenuInput, submenu: &str) -> MenuResult {
        let point = input
            .model
            .visible_position_for_submenu(submenu)
            .or_else(|| input.model.visible_position_for_navigation("Next"))
            .expect("visible submenu or next page");
        click(input, row(point))
    }

    #[test]
    fn interleaved_seats_cannot_press_release_navigate_or_dismiss_for_owner() {
        let mut input = input();
        presented(&mut input);
        let pause = action_row(&input, Action::SetPaused(true));
        assert_eq!(button(&mut input, 2, pause, true), MenuResult::None);
        assert_eq!(button(&mut input, 1, pause, false), MenuResult::None);
        assert_eq!(button(&mut input, 1, pause, true), MenuResult::None);
        for kind in [
            PointerEventKind::Enter { serial: 0 },
            PointerEventKind::Motion { time: 0 },
            PointerEventKind::Press {
                button: 0x111,
                time: 0,
                serial: 0,
            },
            PointerEventKind::Release {
                button: 0x110,
                time: 0,
                serial: 0,
            },
            PointerEventKind::Leave { serial: 0 },
        ] {
            assert_eq!(
                input.pointer(2, &kind, (40.0, 109.0)),
                (MenuResult::None, false)
            );
        }
        // The owner's original press remains its own, not another seat's release.
        assert_eq!(
            button(&mut input, 1, pause, false),
            MenuResult::Action(Action::SetPaused(true))
        );
    }

    #[test]
    fn page_navigation_cannot_select_replacement_rows_before_their_frame() {
        use crate::{
            activity::FocusTarget,
            context_menu::{AttachmentContext, ChatChoice},
        };
        let mut input = MenuInput::new(
            Model::new(crate::context_menu::Context {
                attachments: Some(AttachmentContext {
                    current: None,
                    available: true,
                    choices: (0..9)
                        .map(|index| ChatChoice {
                            target: FocusTarget {
                                source: "test".into(),
                                instance: format!("chat-{index}"),
                                activity: "chat".into(),
                            },
                            incarnation: index + 1,
                            summary: None,
                        })
                        .collect(),
                }),
                ..context()
            }),
            1,
        );
        let attach = row(input
            .model
            .visible_position_for_submenu("Attach to app / chat")
            .unwrap());
        assert_eq!(click(&mut input, attach), MenuResult::None); // Not initially presented.
        presented(&mut input);
        assert_eq!(click(&mut input, attach), MenuResult::Changed);
        presented(&mut input);
        let next = row(input.model.visible_position_for_navigation("Next").unwrap());
        assert_eq!(click(&mut input, next), MenuResult::Changed);
        presented(&mut input);
        let previous = row(input
            .model
            .visible_position_for_navigation("Previous")
            .unwrap());
        assert_eq!(click(&mut input, previous), MenuResult::Changed);
        let choice = action_row(&input, Action::Attach(2));
        assert_eq!(click(&mut input, choice), MenuResult::None);
        input.committed();
        assert_eq!(click(&mut input, choice), MenuResult::None); // Commit alone isn't enough.
        assert_eq!(button(&mut input, 1, choice, true), MenuResult::None);
        input.frame();
        assert_eq!(button(&mut input, 1, choice, false), MenuResult::None); // No latent press.
        assert_eq!(
            click(&mut input, choice),
            MenuResult::Action(Action::Attach(2))
        );
    }

    #[test]
    fn configure_blocks_old_frames_and_presses_but_hover_does_not() {
        let mut input = input();
        presented(&mut input);
        let pause = action_row(&input, Action::SetPaused(true));
        assert_eq!(button(&mut input, 1, pause, true), MenuResult::None);
        input.committed(); // A hover frame is pending when configure arrives.
        input.configure((320, 338)); // Even an unchanged size invalidates input.
        input.frame(); // Old layout's callback must not enable the new layout.
        assert_eq!(button(&mut input, 1, pause, false), MenuResult::None);
        assert_eq!(click(&mut input, pause), MenuResult::None);
        input.committed();
        assert_eq!(click(&mut input, pause), MenuResult::None);
        input.frame();
        assert_eq!(button(&mut input, 1, pause, false), MenuResult::None);
        assert_eq!(
            input.pointer(
                1,
                &PointerEventKind::Motion { time: 0 },
                (40.0, 19.0 + pause as f64 * 30.0)
            ),
            (MenuResult::None, true)
        );
        assert_eq!(
            click(&mut input, pause),
            MenuResult::Action(Action::SetPaused(true))
        );
    }

    #[test]
    fn density_repaint_preserves_page_owner_and_in_flight_press() {
        let mut input = input();
        presented(&mut input);
        assert_eq!(navigate(&mut input, "Settings"), MenuResult::Changed);
        presented(&mut input);
        let revision = input.layout_revision;
        let mut scaling = super::super::scaling::ScaleState::new(true, true, 1);
        let mut font = Typeface::bundled().unwrap();
        let point = input
            .model
            .visible_position_for_submenu("Text size")
            .unwrap();
        let target = row(point);
        assert_eq!(button(&mut input, 1, target, true), MenuResult::None);
        for units in [120, 150, 180, 210, 240] {
            scaling.preferred_changed(units);
            let spec = scaling.plan(input.model.size()).unwrap();
            input.model.paint_raster(&mut font, spec);
            input.committed();
            input.frame();
            assert_eq!(input.layout_revision, revision);
            assert_eq!(input.owner, 1);
            assert_eq!(
                input.model.visible_position_for_submenu("Text size"),
                Some(point)
            );
        }
        assert_eq!(button(&mut input, 1, target, false), MenuResult::Changed);
    }

    #[test]
    fn deadlines_work_without_parent_frames_and_configuration_is_bounded() {
        let now = Instant::now();
        let configure = now + Duration::from_secs(2);
        let idle = now + Duration::from_secs(10);
        let leave = now + Duration::from_millis(350);
        assert!(!expired(now, false, configure, None, idle));
        assert!(expired(configure, false, configure, None, idle));
        assert!(!expired(configure, true, configure, None, idle));
        assert!(expired(leave, true, configure, Some(leave), idle));
        assert!(expired(idle, true, configure, None, idle));
        let mut deadline = None;
        earliest(&mut deadline, Some(idle));
        earliest(&mut deadline, Some(leave));
        assert_eq!(deadline, Some(leave));
    }

    #[test]
    fn actions_reject_removed_recreated_closing_and_rebound_parents() {
        let binding = Some(AttachmentBinding {
            target: None,
            generation: 1,
        });
        assert!(action_is_current(Some(1), 1, &binding, &binding, false));
        for parent in [None, Some(2)] {
            assert!(!action_is_current(parent, 1, &binding, &binding, false));
        }
        assert!(!action_is_current(Some(1), 1, &binding, &binding, true));
        let rebound = Some(AttachmentBinding {
            target: None,
            generation: 2,
        });
        assert!(!action_is_current(Some(1), 1, &binding, &rebound, false));
        assert!(!action_is_current(Some(1), 1, &binding, &None, false));
    }

    #[test]
    fn forget_uses_only_writable_preference_identity_and_shared_settings_catalog() {
        let binding = PreferenceBinding {
            target: crate::activity::FocusTarget {
                source: "private".into(),
                instance: "one".into(),
                activity: "chat".into(),
            },
            generation: 3,
        };
        for preference in [
            None,
            Some(PreferencePresentation {
                binding: binding.clone(),
                placement: None,
                writable: false,
            }),
            Some(PreferencePresentation {
                binding: binding.clone(),
                placement: Some(preferences::Placement { x: 1.0, y: 2.0 }),
                writable: true,
            }),
        ] {
            let frozen = writable_preference(&preference).cloned();
            assert_eq!(
                frozen.is_some(),
                preference.as_ref().is_some_and(|p| p.writable)
            );
            let mut input = MenuInput::new(
                Model::new(crate::context_menu::Context {
                    preferences: frozen.is_some(),
                    ..context()
                }),
                1,
            );
            presented(&mut input);
            assert_eq!(navigate(&mut input, "Settings"), MenuResult::Changed);
            presented(&mut input);
            let forget = input
                .model
                .visible_position_for_action(Action::ForgetPreferences);
            assert_eq!(forget.is_some(), frozen.is_some());
            if let Some(point) = forget {
                assert_eq!(
                    click(&mut input, row(point)),
                    MenuResult::Action(Action::ForgetPreferences)
                );
            }
        }
        let mut preference = Some(PreferencePresentation {
            binding: binding.clone(),
            placement: None,
            writable: true,
        });
        let frozen = writable_preference(&preference).cloned();
        preference.as_mut().unwrap().placement = Some(preferences::Placement { x: 8.0, y: 9.0 });
        assert_eq!(frozen.as_ref(), writable_preference(&preference));
        preference.as_mut().unwrap().binding.generation += 1;
        assert_ne!(frozen.as_ref(), writable_preference(&preference));
        preference.as_mut().unwrap().writable = false;
        assert!(writable_preference(&preference).is_none());
    }

    fn pump_until(
        state: &mut State,
        connection: &Connection,
        event_loop: &mut EventLoop<State>,
        qh: &QueueHandle<State>,
        ready: impl Fn(&State) -> bool,
    ) -> Result<()> {
        let limit = Instant::now() + Duration::from_secs(6);
        loop {
            let now = Instant::now();
            ensure!(now < limit, "private fixture condition timed out");
            if let Some(snapshot) = state.snapshots.take() {
                state.reconcile(snapshot, now)?;
            }
            state.refresh_surfaces(qh, now)?;
            state.service_popups(qh, now, &mut None);
            let held = state
                .views
                .values()
                .any(|v| v.native.as_ref().is_some_and(|n| n.popup.is_some()));
            for view in state.views.values_mut() {
                if let Some(control) = &state.control {
                    view.close
                        .retry(now, || control.close(&view.scene.id).is_ok());
                }
                if let Some(n) = &mut view.native {
                    n.turn(
                        &mut view.scene,
                        now,
                        held || view.closing,
                        &state.raw_compositor,
                        qh,
                        &mut None,
                    )?;
                    if n.travel.is_some() && n.frame_pending && !held && !view.scene.options.paused
                    {
                        // A source/bubble wakeup before every frame callback must
                        // not reset elapsed motion and starve every ready turn.
                        view.scene.bubble_dirty = true;
                        n.turn(
                            &mut view.scene,
                            now,
                            held || view.closing,
                            &state.raw_compositor,
                            qh,
                            &mut None,
                        )?;
                    }
                }
            }
            connection.flush()?;
            if ready(state) {
                return Ok(());
            }
            event_loop.dispatch(Some(Duration::from_millis(10)), state)?;
        }
    }

    fn route(state: &mut State, id: &str, action: Action) -> Result<()> {
        let view = &state.views[id];
        let parent = view.native.as_ref().unwrap().identity;
        let binding = view.runtime_binding.clone();
        let preference = writable_preference(&view.preference).cloned();
        state.dismiss_popups();
        state.popup_action(
            id,
            parent,
            MenuBindings {
                binding,
                preference,
                frozen: None,
            },
            action,
            Instant::now(),
        )
    }

    fn assert_sprite_pose(
        state: &mut State,
        id: &str,
        poses: &[crate::animation::Animation],
    ) -> Vec<u8> {
        let view = state.views.get_mut(id).unwrap();
        let size = view.scene.sprite_size();
        let dirty = view.scene.sprite_dirty;
        let spec = RasterSpec::fit(size, Density::ONE, RasterMode::IntegerBufferScale).unwrap();
        let actual = view.scene.render_sprite_raster(spec);
        view.scene.sprite_dirty = dirty;
        let facing = view.scene.facing();
        let atlas = &state.atlases[view.scene.character.id];
        let width = 192.0 * f64::from(view.scene.options.sprite_scale) * view.scene.options.scale;
        let height = 208.0 * f64::from(view.scene.options.sprite_scale) * view.scene.options.scale;
        assert!(
            poses.iter().any(|&pose| {
                (0..atlas.clip(pose).frame_durations.len()).any(|frame| {
                    let mut expected = vec![0; actual.len()];
                    let source = atlas.frame(pose, frame);
                    for y in 0..size.1 {
                        for x in 0..size.0 {
                            let lx = f64::from(x) + 0.5;
                            let ly = f64::from(y) + 0.5;
                            if lx < width && ly < height {
                                let sx = ((lx / width * f64::from(source.width)).floor() as usize)
                                    .min(source.width as usize - 1);
                                let sx = if facing == crate::motion::Facing::Left {
                                    source.width as usize - 1 - sx
                                } else {
                                    sx
                                };
                                let sy = ((ly / height * f64::from(source.height)).floor()
                                    as usize)
                                    .min(source.height as usize - 1);
                                let from = (sy * source.width as usize + sx) * 4;
                                let to = (y as usize * size.0 as usize + x as usize) * 4;
                                expected[to..to + 4].copy_from_slice(&source[from..from + 4]);
                            }
                        }
                    }
                    expected == actual
                })
            }),
            "sprite must be one of {poses:?}"
        );
        actual
    }

    fn defer_corner_origin(state: &mut State, id: &str) -> Result<(i32, i32)> {
        let view = state.views.get_mut(id).unwrap();
        let native = view.native.as_mut().unwrap();
        ensure!(
            !native.frame_pending && !view.scene.options.paused,
            "fixture must be ready to plan travel"
        );
        let committed = native.committed_origin;
        let output = native.output_size.unwrap();
        let corner = if committed.0 < output.0 / 2 {
            crate::corner::Corner::BottomRight
        } else {
            crate::corner::Corner::TopLeft
        };
        let now = Instant::now();
        native.request_corner_at(&mut view.scene, now - Duration::from_millis(50), corner)?;
        native.advance_corner(&mut view.scene, now - Duration::from_millis(50), false);
        native.advance_corner(&mut view.scene, now, false);
        ensure!(
            native.origin != committed,
            "fixture must have an unsubmitted travel step"
        );
        // Model the state left by a failed buffer acquisition, without changing
        // compositor-owned buffers or relying on wl_buffer.release timing.
        native.buffers_blocked = true;
        Ok(committed)
    }

    fn verify_hyprland_geometry(
        state: &mut State,
        connection: &Connection,
        event_loop: &mut EventLoop<State>,
        qh: &QueueHandle<State>,
        corner: crate::corner::Corner,
    ) -> Result<()> {
        if std::env::var_os("NUMEN_TEST_HYPRLAND_GEOMETRY").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return Ok(());
        }
        fn query(kind: &str) -> Result<serde_json::Value> {
            let result = std::process::Command::new("timeout")
                .args(["--signal=KILL", "2s", "hyprctl", "-j", kind])
                .output()
                .context("run bounded read-only Hyprland geometry query")?;
            ensure!(
                result.status.success(),
                "hyprctl {kind} failed or timed out: {}",
                result.status
            );
            serde_json::from_slice(&result.stdout).context("parse Hyprland geometry JSON")
        }

        // Freeze both familiars' expected geometry. Layer JSON uses global
        // logical coordinates; layer margins are local to the selected output.
        let expected = state
            .views
            .iter()
            .map(|(id, view)| {
                let native = view.native.as_ref().context("fixture parent unavailable")?;
                let monitor = state
                    .outputs
                    .info(&native.output)
                    .and_then(|info| info.name)
                    .context("fixture output name unavailable")?;
                Ok((
                    id.clone(),
                    monitor,
                    native.committed_origin,
                    view.scene.sprite_size(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let pid = u64::from(std::process::id());
        let settle_by = Instant::now() + Duration::from_secs(1);
        loop {
            let monitors = query("monitors")?;
            let layers = query("layers")?;
            let mut actual = Vec::new();
            let mut observed = Vec::new();
            for (id, name, origin, size) in &expected {
                let monitor = monitors
                    .as_array()
                    .context("Hyprland monitors must be an array")?
                    .iter()
                    .find(|m| m["name"].as_str() == Some(name.as_str()))
                    .context("fixture monitor absent from Hyprland")?;
                let x =
                    monitor["x"].as_i64().context("monitor x unavailable")? + i64::from(origin.0);
                let y =
                    monitor["y"].as_i64().context("monitor y unavailable")? + i64::from(origin.1);
                let candidates = layers[name]["levels"]
                    .as_object()
                    .context("Hyprland monitor levels unavailable")?
                    .values()
                    .filter_map(|level| level.as_array())
                    .flatten()
                    .filter(|layer| {
                        layer["namespace"].as_str() == Some("numen")
                            && layer["pid"].as_u64() == Some(pid)
                    })
                    .map(|layer| {
                        (
                            layer["x"].as_i64(),
                            layer["y"].as_i64(),
                            layer["w"].as_u64(),
                            layer["h"].as_u64(),
                        )
                    })
                    .collect::<Vec<_>>();
                let geometry = (
                    Some(x),
                    Some(y),
                    Some(u64::from(size.0)),
                    Some(u64::from(size.1)),
                );
                if candidates
                    .iter()
                    .filter(|&&candidate| candidate == geometry)
                    .count()
                    == 1
                {
                    actual.push((id, name, x, y, size));
                }
                observed.push((id, geometry, candidates));
            }
            ensure!(
                Instant::now() <= settle_by,
                "Hyprland geometry did not settle within one second for {corner:?}: {observed:?}"
            );
            if actual.len() == expected.len() {
                for (id, name, x, y, size) in actual {
                    eprintln!(
                        "Hyprland actual corner={corner:?} fixture_pid={pid} familiar={id} monitor={name} x={x} y={y} w={} h={}",
                        size.0, size.1
                    );
                }
                return Ok(());
            }
            let retry_at = (Instant::now() + Duration::from_millis(20)).min(settle_by);
            pump_until(state, connection, event_loop, qh, |_| {
                Instant::now() >= retry_at
            })?;
        }
    }

    fn popup_ready(state: &State, id: &str) -> bool {
        state
            .views
            .get(id)
            .and_then(|v| v.native.as_ref()?.popup.as_ref())
            .is_some_and(|p| {
                p.configured
                    && !p.dirty
                    && !p.frame_pending
                    && p.scaling.state.committed == p.scaling.state.plan(p.size)
            })
    }

    fn check_text_preset(
        state: &mut State,
        connection: &Connection,
        event_loop: &mut EventLoop<State>,
        qh: &QueueHandle<State>,
        id: &str,
        checked_row: usize,
    ) -> Result<()> {
        state.open_popup(id, (1.0, 1.0), 0, qh)?;
        pump_until(state, connection, event_loop, qh, |s| popup_ready(s, id))?;
        enter_submenu(state, connection, event_loop, qh, id, "Settings")?;
        enter_submenu(state, connection, event_loop, qh, id, "Text size")?;
        let p = state
            .views
            .get_mut(id)
            .unwrap()
            .native
            .as_mut()
            .unwrap()
            .popup
            .as_mut()
            .unwrap();
        let spec = RasterSpec::fit(p.size, Density::ONE, RasterMode::IntegerBufferScale).unwrap();
        let rgba = p.input.model.paint_raster(&mut p.font, spec);
        for row in 0..4 {
            let offset = ((4 + row * 30 + 13) * p.size.0 as usize + 12) * 4;
            assert_eq!(
                rgba[offset..offset + 3] == [140, 195, 236],
                row == checked_row
            );
        }
        state.dismiss_popups();
        Ok(())
    }

    fn enter_submenu(
        state: &mut State,
        connection: &Connection,
        event_loop: &mut EventLoop<State>,
        qh: &QueueHandle<State>,
        id: &str,
        label: &str,
    ) -> Result<()> {
        for _ in 0..8 {
            let p = state
                .views
                .get_mut(id)
                .unwrap()
                .native
                .as_mut()
                .unwrap()
                .popup
                .as_mut()
                .unwrap();
            let found = p.input.model.visible_position_for_submenu(label).is_some();
            assert_eq!(navigate(&mut p.input, label), MenuResult::Changed);
            p.dirty = true;
            pump_until(state, connection, event_loop, qh, |s| popup_ready(s, id))?;
            if found {
                return Ok(());
            }
        }
        anyhow::bail!("submenu unavailable: {label}")
    }

    fn select_native_action(
        state: &mut State,
        connection: &Connection,
        event_loop: &mut EventLoop<State>,
        qh: &QueueHandle<State>,
        id: &str,
        action: Action,
    ) -> Result<()> {
        for _ in 0..8 {
            pump_until(state, connection, event_loop, qh, |s| popup_ready(s, id))?;
            let p = state
                .views
                .get_mut(id)
                .unwrap()
                .native
                .as_mut()
                .unwrap()
                .popup
                .as_mut()
                .unwrap();
            if let Some(position) = p.input.model.visible_position_for_action(action) {
                let surface = p.role.wl_surface().clone();
                let owner = p.owner();
                for kind in [
                    PointerEventKind::Press {
                        button: 0x110,
                        time: 0,
                        serial: 0,
                    },
                    PointerEventKind::Release {
                        button: 0x110,
                        time: 0,
                        serial: 0,
                    },
                ] {
                    assert!(state.popup_pointer(
                        owner,
                        &PointerEvent {
                            surface: surface.clone(),
                            position,
                            kind
                        }
                    ));
                }
                return Ok(());
            }
            let next = p
                .input
                .model
                .visible_position_for_navigation("Next")
                .context("action unavailable")?;
            assert_eq!(click(&mut p.input, row(next)), MenuResult::Changed);
            p.dirty = true;
        }
        anyhow::bail!("action unavailable: {action:?}")
    }

    #[test]
    #[ignore = "requires an existing Wayland layer-shell compositor; standalone and isolated temporary service only"]
    fn private_wayland_popup_lifecycle() -> Result<()> {
        use clap::Parser;
        let Prepared {
            connection,
            queue,
            mut state,
        } = prepare()?;
        let Some(crate::Command::Run(mut options)) =
            crate::Cli::parse_from(["numen", "run"]).command
        else {
            unreachable!()
        };
        options.manual = true;
        options.paused = true;
        options.no_bubble = true;
        options.no_preferences = true;
        state.options = Some(options);
        state.add_scene("popup-test".into(), DEFAULT_CHARACTER_ID, None)?;
        let binding = AttachmentBinding {
            target: None,
            generation: 1,
        };
        state.views.get_mut("popup-test").unwrap().runtime_binding = Some(binding.clone());
        let qh = queue.handle();
        let mut event_loop = EventLoop::<State>::try_new()?;
        WaylandSource::new(connection.clone(), queue)
            .insert(event_loop.handle())
            .map_err(|error| anyhow::anyhow!("insert test Wayland source: {error}"))?;
        let limit = Instant::now() + Duration::from_secs(2);
        loop {
            state.refresh_surfaces(&qh, Instant::now())?;
            let view = state.views.get_mut("popup-test").unwrap();
            if let Some(n) = &mut view.native
                && n.configured
            {
                n.present(&mut view.scene, &state.raw_compositor, &qh)?;
                break;
            }
            ensure!(Instant::now() < limit, "test parent configure timeout");
            connection.flush()?;
            event_loop.dispatch(Some(Duration::from_millis(10)), &mut state)?;
        }
        state.open_popup("popup-test", (1.0, 1.0), 0, &qh)?;
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            popup_ready(s, "popup-test")
        })?;
        enter_submenu(
            &mut state,
            &connection,
            &mut event_loop,
            &qh,
            "popup-test",
            "Settings",
        )?;
        {
            let n = state.views["popup-test"].native.as_ref().unwrap();
            let p = n.popup.as_ref().unwrap();
            eprintln!(
                "Compositor initial raster sprite={:?} popup={:?}",
                n.sprite_scaling.state.committed, p.scaling.state.committed
            );
        }
        // Inject preferences only into this fixture's local state. These are not
        // compositor-delivered preferences and never alter output configuration.
        for units in [120, 150, 180, 210, 240] {
            inject_parent_density(&mut state, "popup-test", 150, &connection, &qh);
            let (revision, binding, point) = {
                let n = state
                    .views
                    .get_mut("popup-test")
                    .unwrap()
                    .native
                    .as_mut()
                    .unwrap();
                n.bubble_scaling.state.preferred_changed(210);
                let p = n.popup.as_mut().unwrap();
                p.scaling.state.preferred_changed(units);
                (
                    p.input.layout_revision,
                    p.bindings.binding.clone(),
                    p.input.model.visible_position_for_submenu("Text size"),
                )
            };
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                popup_ready(s, "popup-test")
            })?;
            let n = state.views["popup-test"].native.as_ref().unwrap();
            let p = n.popup.as_ref().unwrap();
            assert_eq!(p.input.layout_revision, revision);
            assert_eq!(p.bindings.binding, binding);
            assert_eq!(
                p.input.model.visible_position_for_submenu("Text size"),
                point
            );
            eprintln!(
                "Locally injected units={units} sprite={:?} popup={:?}",
                n.sprite_scaling.state.committed, p.scaling.state.committed
            );
        }
        let settings = state.views["popup-test"].settings;
        let changed = FamiliarSettings {
            sprite_scale: settings.sprite_scale + f64::EPSILON,
            ..settings
        };
        assert_ne!(settings, changed);
        assert_eq!(settings.sprite_scale as f32, changed.sprite_scale as f32);
        state.reconcile(
            vec![FamiliarPresentation {
                id: "popup-test".into(),
                runtime_binding: binding,
                preset: DEFAULT_CHARACTER_ID.into(),
                presentation: Default::default(),
                settings: changed,
                preference: None,
            }],
            Instant::now(),
        )?;
        assert!(
            state.views["popup-test"]
                .native
                .as_ref()
                .unwrap()
                .popup
                .is_none(),
            "exact authoritative settings changes must dismiss stale checkmarks"
        );
        let patch = crate::protocol::SettingsPatch {
            font_size: Some(16.0),
            ..Default::default()
        };
        route(&mut state, "popup-test", Action::Settings(patch))?;
        assert_eq!(state.views["popup-test"].settings, patch.apply(changed));
        assert_eq!(state.views["popup-test"].scene.options.font_size, 16.0);
        check_text_preset(
            &mut state,
            &connection,
            &mut event_loop,
            &qh,
            "popup-test",
            2,
        )?;
        state.views.clear();

        // This service owns only a fresh private socket. No default service,
        // HOME preference file, navigation callback, CLI, or synthetic OS input.
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let path = directory.path().join("popup.sock");
        let preferences_path = directory.path().join("preferences.toml");
        let snapshots = state.snapshots.clone();
        let reveals = state.reveals.clone();
        let quit = state.quit.clone();
        let defaults = FamiliarSettings::default();
        let server = Server::start_graphical_with_preferences(
            &path,
            move |views| {
                snapshots.post(views);
            },
            move |ids| reveals.post(ids),
            None,
            move || {
                quit.store(true, Ordering::Release);
            },
            true,
            defaults,
            Some(preferences_path.clone()),
        )?;
        state.control = Some(server.familiar_control());
        state.typeface = Some(Typeface::bundled()?);
        let options = state.options.as_mut().unwrap();
        options.manual = false;
        options.no_bubble = false;
        options.paused = true;
        let control = state.control.as_ref().unwrap().clone();
        control.open_unattached(DEFAULT_CHARACTER_ID)?;
        let mut publisher = crate::ipc::Publisher::connect(&path, "popup-test", "private")?;
        for activity in ["a", "b"] {
            publisher.update(
                activity,
                1,
                crate::protocol::ActivityState::Working,
                false,
                None,
            )?;
        }
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            s.views.len() == 2
                && s.views.values().all(|v| {
                    v.native
                        .as_ref()
                        .is_some_and(|n| n.configured && n.sprite_size.0 > 0)
                })
                && control
                    .target_snapshot("main")
                    .is_ok_and(|memo| memo.targets.len() == 2)
        })?;
        let other = state
            .views
            .keys()
            .find(|id| id.as_str() != "main")
            .unwrap()
            .clone();
        // Dispatch locally through the production pointer router, not to the OS.
        // An inert pointer supplies B's identity without creating another seat.
        let surface = state.views[&other]
            .native
            .as_ref()
            .unwrap()
            .layer
            .wl_surface()
            .clone();
        let pointer_b = wl_pointer::WlPointer::inert(surface.backend().clone());
        let dispatch_b = |state: &mut State, kind, position| {
            state.pointer_frame(
                &connection,
                &qh,
                &pointer_b,
                &[PointerEvent {
                    surface: surface.clone(),
                    position,
                    kind,
                }],
            );
        };
        for dragging in [true, false] {
            for blocked in [
                PointerEventKind::Release {
                    button: 0x110,
                    time: 0,
                    serial: 0,
                },
                PointerEventKind::Leave { serial: 0 },
            ] {
                let initial = state.views[&other].native.as_ref().unwrap().origin;
                dispatch_b(
                    &mut state,
                    PointerEventKind::Press {
                        button: 0x110,
                        time: 0,
                        serial: 0,
                    },
                    (40.0, 40.0),
                );
                if dragging {
                    dispatch_b(
                        &mut state,
                        PointerEventKind::Motion { time: 0 },
                        (20.0, 40.0),
                    );
                    assert_ne!(state.views[&other].native.as_ref().unwrap().origin, initial);
                }
                state.open_popup("main", (1.0, 1.0), u32::MAX, &qh)?;
                assert!(!state.popup_accepts_pointer(pointer_b.id().protocol_id()));
                pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                    popup_ready(s, "main")
                })?;
                dispatch_b(&mut state, blocked, (20.0, 40.0));
                assert!(popup_ready(&state, "main"), "B must not dismiss A's menu");
                state.dismiss_popups();
                let origin = state.views[&other].native.as_ref().unwrap().origin;
                dispatch_b(
                    &mut state,
                    PointerEventKind::Motion { time: 0 },
                    (0.0, 40.0),
                );
                assert_eq!(
                    state.views[&other].native.as_ref().unwrap().origin,
                    origin,
                    "menu cleanup must not leave B's sprite gesture held"
                );
                dispatch_b(
                    &mut state,
                    PointerEventKind::Release {
                        button: 0x110,
                        time: 0,
                        serial: 0,
                    },
                    (40.0, 40.0),
                );
                assert!(
                    !state.views[&other].scene.bubble_visible(),
                    "cancelled presses must not reveal"
                );
                assert!(!state.views["main"].scene.bubble_visible());
            }
        }
        assert!(!state.views["main"].scene.bubble_visible());
        assert!(!state.views[&other].scene.bubble_visible());
        route(&mut state, "main", Action::RevealStatus)?;
        assert!(state.views["main"].scene.bubble_visible());
        assert!(!state.views[&other].scene.bubble_visible());
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            let n = s.views["main"].native.as_ref().unwrap();
            n.bubble_visible && !n.frame_pending
        })?;
        state.open_popup("main", (1.0, 1.0), 0, &qh)?;
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            popup_ready(s, "main")
        })?;
        enter_submenu(
            &mut state,
            &connection,
            &mut event_loop,
            &qh,
            "main",
            "Attach to app / chat",
        )?;
        let (revision, binding, preference, target) = {
            let p = state
                .views
                .get_mut("main")
                .unwrap()
                .native
                .as_mut()
                .unwrap()
                .popup
                .as_mut()
                .unwrap();
            let target = action_row(&p.input, Action::Attach(0));
            assert!(p.bindings.frozen.is_some());
            assert_eq!(button(&mut p.input, 0, target, true), MenuResult::None);
            (
                p.input.layout_revision,
                p.bindings.binding.clone(),
                p.bindings.preference.clone(),
                target,
            )
        };
        // A local callback injection, not an observed compositor preference.
        inject_parent_density(&mut state, "main", 150, &connection, &qh);
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            let n = s.views["main"].native.as_ref().unwrap();
            popup_ready(s, "main")
                && !n.frame_pending
                && n.bubble_scaling.state.committed
                    == n.bubble_layout
                        .and_then(|layout| n.bubble_scaling.state.plan(layout.size))
        })?;
        {
            let n = state
                .views
                .get_mut("main")
                .unwrap()
                .native
                .as_mut()
                .unwrap();
            assert_eq!(
                n.bubble_scaling.state.requested_density(),
                n.sprite_scaling.state.requested_density()
            );
            eprintln!(
                "Locally injected parent units=150 inherited bubble={:?}",
                n.bubble_scaling.state.committed
            );
            let p = n.popup.as_mut().unwrap();
            assert_eq!(p.input.layout_revision, revision);
            assert_eq!(p.bindings.binding, binding);
            assert_eq!(p.bindings.preference, preference);
            assert!(p.bindings.frozen.is_some());
            assert_eq!(
                button(&mut p.input, 0, target, false),
                MenuResult::Action(Action::Attach(0))
            );
        }
        state.dismiss_popups();
        let sprite_spec = state.views["main"]
            .native
            .as_ref()
            .unwrap()
            .sprite_scaling
            .state
            .committed;
        state
            .views
            .get_mut("main")
            .unwrap()
            .native
            .as_mut()
            .unwrap()
            .bubble_scaling
            .state
            .preferred_changed(210);
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            let n = s.views["main"].native.as_ref().unwrap();
            !n.frame_pending
                && n.bubble_scaling.state.committed
                    == n.bubble_layout
                        .and_then(|layout| n.bubble_scaling.state.plan(layout.size))
        })?;
        {
            let n = state.views["main"].native.as_ref().unwrap();
            assert_eq!(n.sprite_scaling.state.committed, sprite_spec);
            eprintln!(
                "Locally injected bubble units=210 sprite={:?} bubble={:?}",
                sprite_spec, n.bubble_scaling.state.committed
            );
        }
        for paused in [true, true, false, false, true] {
            route(&mut state, "main", Action::SetPaused(paused))?;
            assert_eq!(state.views["main"].scene.options.paused, paused);
            assert!(state.views[&other].scene.options.paused);
        }

        let applied = state.views["main"].runtime_binding.clone().unwrap();
        let snapshot = control.target_snapshot("main")?;
        let (context, frozen) = pointer_menu::attachment("main", &applied, Some(snapshot.clone()));
        let (_, stale) = pointer_menu::attachment("main", &applied, Some(snapshot));
        let selected = context.choices[1].target.clone();
        let parent = state.views["main"].native.as_ref().unwrap().identity;
        state.popup_action(
            "main",
            parent,
            MenuBindings {
                binding: Some(applied.clone()),
                frozen,
                preference: None,
            },
            Action::Attach(1),
            Instant::now(),
        )?;
        assert_eq!(
            state.views["main"].runtime_binding.as_ref(),
            Some(&applied),
            "attach must not apply optimistically"
        );
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            s.views["main"]
                .runtime_binding
                .as_ref()
                .is_some_and(|b| b.target.as_ref() == Some(&selected))
        })?;
        let committed = state.views["main"].runtime_binding.clone().unwrap();
        assert!(committed.generation > applied.generation);
        assert!(
            state.views[&other]
                .runtime_binding
                .as_ref()
                .unwrap()
                .target
                .is_none()
        );
        // Use the current parent/applied binding so the frozen payload itself,
        // rather than just the outer lifecycle guard, must reject this replay.
        assert!(
            state
                .popup_action(
                    "main",
                    parent,
                    MenuBindings {
                        binding: Some(committed.clone()),
                        frozen: stale,
                        preference: None
                    },
                    Action::Attach(0),
                    Instant::now()
                )
                .is_err()
        );
        assert_eq!(
            state.views["main"].runtime_binding.as_ref(),
            Some(&committed)
        );

        let patch = crate::protocol::SettingsPatch {
            font_size: Some(20.0),
            ..Default::default()
        };
        let before = state.views["main"].settings;
        route(&mut state, "main", Action::Settings(patch))?;
        assert_eq!(
            state.views["main"].settings, before,
            "queued appearance must not apply optimistically"
        );
        assert_eq!(
            state.views["main"].scene.options.font_size,
            before.font_size as f32
        );
        let expected = patch.apply(before);
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            s.views["main"].settings == expected
        })?;
        assert_eq!(state.views["main"].scene.options.font_size, 20.0);
        assert_eq!(state.views[&other].settings, defaults);
        check_text_preset(&mut state, &connection, &mut event_loop, &qh, "main", 3)?;

        for action in [Action::CopySessionId, Action::OpenChat, Action::GoToMouse] {
            assert!(route(&mut state, "main", action).is_err());
        }
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            writable_preference(&s.views["main"].preference).is_some()
                && std::fs::read(&preferences_path)
                    .ok()
                    .and_then(|bytes| preferences::Document::decode(&bytes).ok())
                    .is_some_and(|doc| doc.records.len() == 1)
        })?;
        let frozen_preference = writable_preference(&state.views["main"].preference)
            .unwrap()
            .clone();
        state.open_popup("main", (1.0, 1.0), 0, &qh)?;
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            popup_ready(s, "main")
        })?;
        enter_submenu(
            &mut state,
            &connection,
            &mut event_loop,
            &qh,
            "main",
            "Settings",
        )?;
        let authoritative: Vec<_> = state
            .views
            .iter()
            .map(|(id, view)| FamiliarPresentation {
                id: id.clone(),
                runtime_binding: view.runtime_binding.clone().unwrap(),
                preset: view.scene.character.id.into(),
                presentation: Default::default(),
                settings: view.settings,
                preference: view.preference.clone(),
            })
            .collect();
        let mut moved = authoritative.clone();
        moved
            .iter_mut()
            .find(|v| v.id == "main")
            .unwrap()
            .preference
            .as_mut()
            .unwrap()
            .placement = Some(preferences::Placement { x: 40.0, y: 50.0 });
        state.reconcile(moved, Instant::now())?;
        assert!(
            popup_ready(&state, "main"),
            "placement metadata alone must not dismiss the menu"
        );
        for rebind in [false, true] {
            let mut revoked = authoritative.clone();
            let preference = revoked
                .iter_mut()
                .find(|v| v.id == "main")
                .unwrap()
                .preference
                .as_mut()
                .unwrap();
            if rebind {
                preference.binding.generation += 1;
            } else {
                preference.writable = false;
            }
            state.snapshots.post(revoked);
            assert!(
                state
                    .popup_action(
                        "main",
                        parent,
                        MenuBindings {
                            binding: Some(committed.clone()),
                            frozen: None,
                            preference: Some(frozen_preference.clone()),
                        },
                        Action::ForgetPreferences,
                        Instant::now()
                    )
                    .is_err()
            );
            assert!(
                state.views["main"].native.as_ref().unwrap().popup.is_none(),
                "preference revocation must dismiss the menu"
            );
            state.reconcile(authoritative.clone(), Instant::now())?;
            state.open_popup("main", (1.0, 1.0), 0, &qh)?;
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                popup_ready(s, "main")
            })?;
            enter_submenu(
                &mut state,
                &connection,
                &mut event_loop,
                &qh,
                "main",
                "Settings",
            )?;
        }
        select_native_action(
            &mut state,
            &connection,
            &mut event_loop,
            &qh,
            "main",
            Action::ForgetPreferences,
        )?;
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            writable_preference(&s.views["main"].preference)
                .is_some_and(|p| p != &frozen_preference)
                && std::fs::read(&preferences_path)
                    .ok()
                    .and_then(|bytes| preferences::Document::decode(&bytes).ok())
                    .is_some_and(|doc| doc.records.is_empty())
        })?;
        assert_eq!(
            state.views["main"].settings, expected,
            "forget must not reset live settings"
        );
        assert_eq!(
            state.views["main"].runtime_binding.as_ref(),
            Some(&committed)
        );

        // Recreate the same saved key, then ensure the obsolete preference generation
        // cannot delete it, either at native dispatch or in the authoritative worker.
        route(&mut state, "main", Action::Settings(patch))?;
        pump_until(&mut state, &connection, &mut event_loop, &qh, |_| {
            std::fs::read(&preferences_path)
                .ok()
                .and_then(|bytes| preferences::Document::decode(&bytes).ok())
                .is_some_and(|doc| doc.records.len() == 1)
        })?;
        assert!(
            state
                .popup_action(
                    "main",
                    parent,
                    MenuBindings {
                        binding: Some(committed.clone()),
                        frozen: None,
                        preference: Some(frozen_preference.clone()),
                    },
                    Action::ForgetPreferences,
                    Instant::now()
                )
                .is_err()
        );
        control.forget_preferences("main", &frozen_preference)?;
        publisher.heartbeat()?;
        pump_until(&mut state, &connection, &mut event_loop, &qh, |_| {
            control.target_snapshot("main").is_ok()
        })?;
        assert_eq!(
            preferences::Document::decode(&std::fs::read(&preferences_path)?)?
                .records
                .len(),
            1
        );

        // Exercise real native commits through the same turn as production.
        // These assertions observe client-submitted margins, not absolute OS XY.
        use crate::{animation::Animation, corner::Corner};
        let original_settings = state.views["main"].settings;
        for paused in [false, true] {
            state
                .views
                .get_mut("main")
                .unwrap()
                .scene
                .set_paused(false, Instant::now());
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                !s.views["main"].native.as_ref().unwrap().frame_pending
            })?;
            let committed = defer_corner_origin(&mut state, "main")?;
            state
                .views
                .get_mut("main")
                .unwrap()
                .scene
                .set_paused(paused, Instant::now());
            let patch = crate::protocol::SettingsPatch {
                sprite_scale: Some(original_settings.sprite_scale * 0.75),
                ..Default::default()
            };
            control.settings("main", patch)?;
            // Wait for the real private worker's full canonical snapshot without
            // presenting the deferred step first. Retain source and preference data.
            let limit = Instant::now() + Duration::from_secs(2);
            let snapshot = loop {
                ensure!(Instant::now() < limit, "sprite-shrink snapshot timed out");
                if let Some(snapshot) = state.snapshots.take()
                    && snapshot
                        .iter()
                        .any(|v| v.id == "main" && v.settings == patch.apply(original_settings))
                {
                    break snapshot;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            state.reconcile(snapshot, Instant::now())?;
            {
                let view = state.views.get_mut("main").unwrap();
                let n = view.native.as_mut().unwrap();
                assert!(n.travel.is_none() && !view.scene.corner_active());
                assert_eq!(
                    n.origin, committed,
                    "sprite shrink must revoke the queued travel step, paused={paused}"
                );
                n.turn(
                    &mut view.scene,
                    Instant::now(),
                    false,
                    &state.raw_compositor,
                    &qh,
                    &mut None,
                )?;
            }
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                let view = &s.views["main"];
                let n = view.native.as_ref().unwrap();
                !n.buffers_blocked && !n.frame_pending && n.sprite_size == view.scene.sprite_size()
            })?;
            assert_eq!(
                state.views["main"]
                    .native
                    .as_ref()
                    .unwrap()
                    .committed_origin,
                committed,
                "recovered buffers must not submit cancelled travel"
            );
            control.settings(
                "main",
                crate::protocol::SettingsPatch {
                    sprite_scale: Some(original_settings.sprite_scale),
                    ..Default::default()
                },
            )?;
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                let view = &s.views["main"];
                view.settings == original_settings
                    && view.native.as_ref().is_some_and(|n| {
                        !n.frame_pending && n.sprite_size == view.scene.sprite_size()
                    })
            })?;
        }
        for paused in [false, true] {
            state
                .views
                .get_mut("main")
                .unwrap()
                .scene
                .set_paused(false, Instant::now());
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                !s.views["main"].native.as_ref().unwrap().frame_pending
            })?;
            let committed = defer_corner_origin(&mut state, "main")?;
            let (original_output, expected) = {
                let view = state.views.get_mut("main").unwrap();
                view.scene.set_paused(paused, Instant::now());
                let sprite = view.scene.sprite_size();
                let n = view.native.as_mut().unwrap();
                let original_output = n.output_size;
                let smaller = Some((
                    (committed.0 + sprite.0 as i32 - 10).max(sprite.0 as i32),
                    (committed.1 + sprite.1 as i32 - 10).max(sprite.1 as i32),
                ));
                let expected = clamp_origin(committed, smaller, sprite);
                // Invoke the production output-update handler's geometry helper;
                // this changes only fixture client state, never an OS output.
                n.update_output_size(&mut view.scene, smaller, Instant::now());
                assert_eq!(
                    n.origin, expected,
                    "resize must clamp submitted origin, not queued travel, paused={paused}"
                );
                assert!(n.travel.is_none() && !view.scene.corner_active());
                n.turn(
                    &mut view.scene,
                    Instant::now(),
                    false,
                    &state.raw_compositor,
                    &qh,
                    &mut None,
                )?;
                (original_output, expected)
            };
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                let n = s.views["main"].native.as_ref().unwrap();
                !n.buffers_blocked && !n.frame_pending
            })?;
            assert_eq!(
                state.views["main"]
                    .native
                    .as_ref()
                    .unwrap()
                    .committed_origin,
                expected
            );
            let view = state.views.get_mut("main").unwrap();
            view.native.as_mut().unwrap().update_output_size(
                &mut view.scene,
                original_output,
                Instant::now(),
            );
        }
        let other_origin = state.views[&other]
            .native
            .as_ref()
            .unwrap()
            .committed_origin;
        route(&mut state, "main", Action::SetPaused(false))?;
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            !s.views["main"].native.as_ref().unwrap().frame_pending
        })?;
        let committed = defer_corner_origin(&mut state, "main")?;
        inject_parent_density(&mut state, "main", 180, &connection, &qh);
        {
            let view = state.views.get_mut("main").unwrap();
            let n = view.native.as_mut().unwrap();
            let pending_origin = n.origin;
            n.bubble_scaling.state.preferred_changed(150);
            assert_eq!(n.origin, pending_origin);
            assert!(n.travel.is_some() && view.scene.corner_active());
            view.scene.set_paused(true, Instant::now());
            let n = view.native.as_mut().unwrap();
            n.turn(
                &mut view.scene,
                Instant::now(),
                false,
                &state.raw_compositor,
                &qh,
                &mut None,
            )?;
            assert_eq!(n.origin, committed);
            assert_eq!(
                n.committed_origin, committed,
                "buffer availability after pause must not submit deferred travel"
            );
            assert!(n.travel.is_some());
        }
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            !s.views["main"].native.as_ref().unwrap().frame_pending
        })?;
        route(&mut state, "main", Action::SetPaused(false))?;
        for menu_owner in ["main", other.as_str()] {
            let committed = defer_corner_origin(&mut state, "main")?;
            state.open_popup(menu_owner, (1.0, 1.0), 0, &qh)?;
            assert_eq!(
                state.views["main"].native.as_ref().unwrap().origin,
                committed,
                "any popup opening must roll back travel before geometry checks"
            );
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                popup_ready(s, menu_owner)
                    && !s.views["main"].native.as_ref().unwrap().frame_pending
            })?;
            assert_eq!(
                state.views["main"]
                    .native
                    .as_ref()
                    .unwrap()
                    .committed_origin,
                committed
            );
            assert!(state.views["main"].scene.corner_active());
            state.dismiss_popups();
            let view = state.views.get_mut("main").unwrap();
            view.native
                .as_mut()
                .unwrap()
                .cancel_corner(&mut view.scene, Instant::now());
        }
        // A non-travelling pending origin belongs to manual dragging, not travel.
        {
            let view = state.views.get_mut(&other).unwrap();
            let n = view.native.as_mut().unwrap();
            let requested = (n.committed_origin.0 + 1, n.committed_origin.1);
            n.origin = requested;
            n.cancel_corner(&mut view.scene, Instant::now());
            assert_eq!(n.origin, requested);
            n.origin = n.committed_origin;
        }
        state
            .views
            .get_mut("main")
            .unwrap()
            .scene
            .options
            .move_speed = 1000.0;
        for (index, corner) in [
            Corner::TopLeft,
            Corner::TopRight,
            Corner::BottomLeft,
            Corner::BottomRight,
        ]
        .into_iter()
        .enumerate()
        {
            for activity in ["a", "b"] {
                publisher.update(
                    activity,
                    2 + index as u64 * 2,
                    crate::protocol::ActivityState::Working,
                    false,
                    None,
                )?;
            }
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                s.views["main"]
                    .native
                    .as_ref()
                    .is_some_and(|n| !n.frame_pending && n.origin == n.committed_origin)
            })?;
            let (start, target) = {
                let view = state.views.get_mut("main").unwrap();
                let n = view.native.as_mut().unwrap();
                let output = n.output_size.unwrap();
                let sprite = view.scene.sprite_size();
                n.origin = clamp_origin((output.0 / 2, output.1 / 2), Some(output), sprite);
                n.layout_dirty = true;
                n.present(&mut view.scene, &state.raw_compositor, &qh)?;
                let target = corner
                    .target(
                        crate::corner::Area {
                            x: 0.0,
                            y: 0.0,
                            width: output.0.into(),
                            height: output.1.into(),
                        },
                        sprite.0.into(),
                        sprite.1.into(),
                    )
                    .unwrap();
                (
                    n.committed_origin,
                    clamp_origin(
                        (
                            (target.x - f64::from(sprite.0) / 2.0).round() as i32,
                            (target.y - f64::from(sprite.1)).round() as i32,
                        ),
                        Some(output),
                        sprite,
                    ),
                )
            };
            route(&mut state, "main", Action::GoToCorner)?;
            {
                let view = state.views.get_mut("main").unwrap();
                view.native.as_mut().unwrap().request_corner_at(
                    &mut view.scene,
                    Instant::now(),
                    corner,
                )?;
            }
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                s.views["main"].native.as_ref().unwrap().committed_origin != start
            })?;
            let first = assert_sprite_pose(
                &mut state,
                "main",
                &[Animation::RunningLeft, Animation::RunningRight],
            );
            if index == 0 {
                route(&mut state, "main", Action::SetPaused(true))?;
                let frozen_origin = state.views["main"]
                    .native
                    .as_ref()
                    .unwrap()
                    .committed_origin;
                let frozen_clip = assert_sprite_pose(
                    &mut state,
                    "main",
                    &[Animation::RunningLeft, Animation::RunningRight],
                );
                let until = Instant::now() + Duration::from_millis(150);
                pump_until(&mut state, &connection, &mut event_loop, &qh, |_| {
                    Instant::now() >= until
                })?;
                assert_eq!(
                    state.views["main"]
                        .native
                        .as_ref()
                        .unwrap()
                        .committed_origin,
                    frozen_origin
                );
                assert_eq!(
                    assert_sprite_pose(
                        &mut state,
                        "main",
                        &[Animation::RunningLeft, Animation::RunningRight]
                    ),
                    frozen_clip
                );
                route(&mut state, "main", Action::SetPaused(false))?;
                state.open_popup(&other, (1.0, 1.0), 0, &qh)?;
                let until = Instant::now() + Duration::from_millis(150);
                pump_until(&mut state, &connection, &mut event_loop, &qh, |_| {
                    Instant::now() >= until
                })?;
                assert_eq!(
                    state.views["main"]
                        .native
                        .as_ref()
                        .unwrap()
                        .committed_origin,
                    frozen_origin,
                    "any familiar's popup must hold every travel"
                );
                assert!(state.views["main"].scene.corner_active());
                state.dismiss_popups();
            }
            for activity in ["a", "b"] {
                publisher.update(
                    activity,
                    3 + index as u64 * 2,
                    crate::protocol::ActivityState::Waiting,
                    true,
                    None,
                )?;
            }
            let sample_at = Instant::now() + Duration::from_millis(120);
            pump_until(&mut state, &connection, &mut event_loop, &qh, |_| {
                Instant::now() >= sample_at
            })?;
            if state.views["main"].scene.corner_active() {
                let next = assert_sprite_pose(
                    &mut state,
                    "main",
                    &[Animation::RunningLeft, Animation::RunningRight],
                );
                assert_ne!(first, next, "running clip must advance during travel");
            }
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                !s.views["main"].scene.corner_active()
            })?;
            let n = state.views["main"].native.as_ref().unwrap();
            assert_eq!(n.committed_origin, target);
            assert_eq!(n.origin, n.committed_origin);
            assert!(n.travel.is_none());
            verify_hyprland_geometry(&mut state, &connection, &mut event_loop, &qh, corner)?;
            assert_sprite_pose(&mut state, "main", &[Animation::Waiting]);
            assert_eq!(
                state.views[&other]
                    .native
                    .as_ref()
                    .unwrap()
                    .committed_origin,
                other_origin
            );
        }

        route(&mut state, "main", Action::CloseFamiliar)?;
        assert!(state.views["main"].closing && state.views["main"].close.retry_at.is_some());
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            !s.views.contains_key("main")
        })?;
        assert_eq!(state.views.len(), 1);
        assert!(state.views.contains_key(&other));
        assert!(!state.quit.load(Ordering::Acquire));
        assert!(
            control.target_snapshot(&other).is_ok(),
            "private service must remain available after scoped close"
        );
        publisher.heartbeat()?; // A real acknowledged request after main was removed.

        // Service actions outlive the clicked parent, even with its obsolete bindings.
        state.popup_action(
            "main",
            parent,
            MenuBindings::default(),
            Action::OpenFamiliar,
            Instant::now(),
        )?;
        assert_eq!(
            state.views.len(),
            1,
            "open must not mutate the local registry"
        );
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            s.views.len() == 2
        })?;
        state.open_popup(&other, (1.0, 1.0), 0, &qh)?;
        select_native_action(
            &mut state,
            &connection,
            &mut event_loop,
            &qh,
            &other,
            Action::OpenFamiliar,
        )?;
        pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
            s.views.len() == 3
        })?;
        for _ in 3..crate::protocol::MAX_FAMILIARS {
            state.popup_action(
                "main",
                parent,
                MenuBindings::default(),
                Action::OpenFamiliar,
                Instant::now(),
            )?;
            let count = state.views.len();
            pump_until(&mut state, &connection, &mut event_loop, &qh, |s| {
                s.views.len() > count
            })?;
        }
        assert!(
            state
                .popup_action(
                    "main",
                    parent,
                    MenuBindings::default(),
                    Action::OpenFamiliar,
                    Instant::now()
                )
                .is_err()
        );
        let service_control = state.control.take();
        assert!(
            state
                .popup_action(
                    "main",
                    parent,
                    MenuBindings::default(),
                    Action::OpenFamiliar,
                    Instant::now()
                )
                .is_err()
        );
        state.control = service_control;
        state.popup_action(
            "main",
            parent,
            MenuBindings::default(),
            Action::QuitNumen,
            Instant::now(),
        )?;
        assert!(state.quit.load(Ordering::Acquire));
        assert_eq!(
            state.views.len(),
            crate::protocol::MAX_FAMILIARS,
            "quit must not queue multiple closes"
        );
        state.views.clear();
        state.control = None;
        drop(publisher);
        drop(server);
        connection.flush()?;
        // Process real teardown traffic without manufacturing any OS input.
        event_loop.dispatch(Some(Duration::from_millis(20)), &mut state)?;
        Ok(())
    }
}
