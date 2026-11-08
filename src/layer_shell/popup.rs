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
    scale: i32,
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
        if !self.configured || self.frame_pending || !self.dirty {
            return Ok(true);
        }
        let physical = (
            self.size.0 * self.scale as u32,
            self.size.1 * self.scale as u32,
        );
        let Some(index) = available_buffer(&mut self.pool, &mut self.buffers, physical)? else {
            return Ok(false);
        };
        let rgba = self.input.model.paint(&mut self.font, physical, self.size);
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
        surface.set_buffer_scale(self.scale);
        self.role
            .xdg_surface()
            .set_window_geometry(0, 0, self.size.0 as i32, self.size.1 as i32);
        let buffer = &self.buffers[index].buffer;
        write_argb(&rgba, self.pool.canvas(buffer).unwrap(), false);
        buffer.attach_to(surface)?;
        surface.damage_buffer(0, 0, physical.0 as i32, physical.1 as i32);
        surface.frame(qh, surface.clone());
        surface.commit();
        self.input.committed();
        self.frame_pending = true;
        self.dirty = false;
        Ok(true)
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
            scale: native.raster_scale.clamp(1, 8),
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
            Action::RevealStatus => {
                ensure!(!view.scene.options.no_bubble, "status bubbles unavailable");
                view.scene.reveal(now);
            }
            Action::SetPaused(paused) => view.scene.set_paused(paused, now),
            Action::CloseFamiliar => {
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
                || !framebuffer_fits((
                    config.width as u32 * p.scale as u32,
                    config.height as u32 * p.scale as u32,
                ))
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
        let limit = Instant::now() + Duration::from_secs(2);
        loop {
            let now = Instant::now();
            ensure!(now < limit, "private fixture condition timed out");
            if let Some(snapshot) = state.snapshots.take() {
                state.reconcile(snapshot, now)?;
            }
            state.refresh_surfaces(qh, now)?;
            state.service_popups(qh, now, &mut None);
            for view in state.views.values_mut() {
                if let Some(control) = &state.control {
                    view.close
                        .retry(now, || control.close(&view.scene.id).is_ok());
                }
                view.scene.tick(now);
                if let Some(n) = &mut view.native
                    && n.configured
                    && !n.frame_pending
                {
                    n.present(&mut view.scene, &state.raw_compositor, qh)?;
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

    fn popup_ready(state: &State, id: &str) -> bool {
        state
            .views
            .get(id)
            .and_then(|v| v.native.as_ref()?.popup.as_ref())
            .is_some_and(|p| p.configured && !p.dirty && !p.frame_pending)
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
        let rgba = p.input.model.paint(&mut p.font, p.size, p.size);
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
