//! Optional per-surface density, independent of logical geometry and input state.
use super::*;
use smithay_client_toolkit::reexports::{
    client::{Dispatch, globals::GlobalList},
    protocols::{
        wp::fractional_scale::v1::client::{
            wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
            wp_fractional_scale_v1::{self, WpFractionalScaleV1},
        },
        wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
    },
};

pub(super) struct ScalingGlobals {
    manager: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
}

impl ScalingGlobals {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Self {
        Self {
            manager: globals.bind(qh, 1..=1, ()).ok(),
            viewporter: globals.bind(qh, 1..=1, ()).ok(),
        }
    }
}

impl Drop for ScalingGlobals {
    fn drop(&mut self) {
        if let Some(manager) = &self.manager {
            manager.destroy();
        }
        if let Some(viewporter) = &self.viewporter {
            viewporter.destroy();
        }
    }
}

pub(super) struct ScaleState {
    fractional: bool,
    output: i32,
    integer: Option<i32>,
    preferred: Option<Density>,
    inherited_density: Option<Density>,
    pub committed: Option<RasterSpec>,
}

impl ScaleState {
    pub(super) fn new(manager: bool, viewporter: bool, output: i32) -> Self {
        Self {
            fractional: manager && viewporter,
            output: output.max(1),
            integer: None,
            preferred: None,
            inherited_density: None,
            committed: None,
        }
    }

    pub fn fallback(&self) -> i32 {
        self.integer.unwrap_or(self.output)
    }

    pub fn preferred_changed(&mut self, units: u32) {
        if self.fractional
            && let Some(density) = Density::from_preferred_scale(units)
        {
            self.preferred = Some(density);
        }
    }

    pub fn integer_changed(&mut self, factor: i32) {
        self.integer = Some(factor.max(1));
    }
    pub fn output_changed(&mut self, factor: i32) {
        self.output = factor.max(1);
    }

    pub fn inherit_density(&mut self, density: Option<Density>) {
        self.inherited_density = density.filter(|_| self.fractional);
    }

    /// Requested density is inherited before each surface fits its own budget.
    pub fn requested_density(&self) -> Density {
        self.preferred
            .or(self.inherited_density)
            .unwrap_or_else(|| {
                if self.fallback() == 1 {
                    Density::ONE
                } else {
                    Density::from_integer(self.fallback())
                }
            })
    }

    pub fn plan(&self, size: (u32, u32)) -> Option<RasterSpec> {
        RasterSpec::fit(
            size,
            self.requested_density(),
            if self.fractional {
                RasterMode::ViewportDestination
            } else {
                RasterMode::IntegerBufferScale
            },
        )
    }
}

pub(super) struct SurfaceScaling {
    pub state: ScaleState,
    fractional: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
}

impl SurfaceScaling {
    #[cfg(test)]
    pub(super) fn fractional(&self) -> Option<&WpFractionalScaleV1> {
        self.fractional.as_ref()
    }

    pub fn new(
        globals: &ScalingGlobals,
        surface: &wl_surface::WlSurface,
        fallback: i32,
        qh: &QueueHandle<State>,
    ) -> Self {
        let mut result = Self {
            state: ScaleState::new(
                globals.manager.is_some(),
                globals.viewporter.is_some(),
                fallback,
            ),
            fractional: None,
            viewport: None,
        };
        if let (Some(manager), Some(viewporter)) = (&globals.manager, &globals.viewporter) {
            result.fractional = Some(manager.get_fractional_scale(surface, qh, surface.clone()));
            result.viewport = Some(viewporter.get_viewport(surface, qh, ()));
        }
        result
    }

    pub fn apply(&self, surface: &wl_surface::WlSurface, spec: RasterSpec) {
        if let Some(viewport) = &self.viewport {
            surface.set_buffer_scale(1);
            let (w, h) = spec.logical_size();
            viewport.set_destination(w as i32, h as i32);
        } else {
            surface.set_buffer_scale((spec.density().units_120() / 120) as i32);
        }
    }

    pub fn committed(&mut self, spec: RasterSpec, kind: &str) {
        debug!(surface_kind = kind, preferred_units = ?self.state.preferred.map(Density::units_120), inherited_units = ?self.state.inherited_density.map(Density::units_120), committed_units = spec.density().units_120(), logical = ?spec.logical_size(), physical = ?spec.physical_size(), mode = ?spec.mode(), "Surface raster committed");
        self.state.committed = Some(spec);
    }

    pub fn destroy(&mut self) {
        if let Some(fractional) = self.fractional.take() {
            fractional.destroy();
        }
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
    }
}

impl Drop for SurfaceScaling {
    fn drop(&mut self) {
        self.destroy();
    }
}

impl Dispatch<WpFractionalScaleV1, wl_surface::WlSurface> for State {
    fn event(
        state: &mut Self,
        fractional: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        surface: &wl_surface::WlSurface,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            for view in state.views.values_mut() {
                let Some(native) = &mut view.native else {
                    continue;
                };
                let scaling = if native.layer.wl_surface() == surface {
                    Some(&mut native.sprite_scaling)
                } else if &native.child == surface {
                    Some(&mut native.bubble_scaling)
                } else if let Some(popup) = &mut native.popup
                    && popup.role.wl_surface() == surface
                {
                    Some(&mut popup.scaling)
                } else {
                    None
                };
                if let Some(scaling) = scaling
                    && scaling.fractional.as_ref() == Some(fractional)
                {
                    scaling.state.preferred_changed(scale);
                    native.sync_density_hints();
                }
            }
        }
    }
}

smithay_client_toolkit::reexports::client::delegate_noop!(State: ignore WpFractionalScaleManagerV1);
smithay_client_toolkit::reexports::client::delegate_noop!(State: ignore WpViewporter);
smithay_client_toolkit::reexports::client::delegate_noop!(State: ignore WpViewport);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_and_fallback_precedence() {
        for manager in [false, true] {
            for viewport in [false, true] {
                let mut state = ScaleState::new(manager, viewport, 2);
                state.inherit_density(Density::from_preferred_scale(180));
                assert_eq!(
                    state.requested_density().units_120(),
                    if manager && viewport { 180 } else { 240 }
                );
                state.preferred_changed(150);
                assert_eq!(
                    state.plan((101, 79)).unwrap().density().units_120(),
                    if manager && viewport { 150 } else { 240 }
                );
                state.integer_changed(3);
                state.output_changed(4);
                assert_eq!(state.fallback(), 3);
                assert_eq!(
                    state.plan((101, 79)).unwrap().density().units_120(),
                    if manager && viewport { 150 } else { 360 }
                );
            }
        }
    }

    #[test]
    fn late_duplicate_and_pending_preferences_do_not_change_committed() {
        let mut state = ScaleState::new(true, true, 1);
        let initial = state.plan((101, 79)).unwrap();
        state.committed = Some(initial);
        for units in [150, 150, 0, 180, 210, 240, u32::MAX] {
            state.preferred_changed(units);
            assert_eq!(state.committed, Some(initial));
            assert!(state.plan((101, 79)).is_some());
        }
        state.preferred_changed(210);
        assert_eq!(state.plan((101, 79)).unwrap().physical_size(), (177, 139));
    }

    #[test]
    fn independent_surfaces_fit_their_own_budget_and_odd_axes() {
        let mut sprite = ScaleState::new(true, true, 1);
        let mut bubble = ScaleState::new(true, true, 1);
        for (units, physical) in [
            (120, (101, 79)),
            (150, (127, 99)),
            (180, (152, 119)),
            (210, (177, 139)),
            (240, (202, 158)),
        ] {
            sprite.preferred_changed(units);
            bubble.preferred_changed(u32::MAX);
            assert_eq!(sprite.plan((101, 79)).unwrap().physical_size(), physical);
            assert_eq!(bubble.plan((4096, 4096)).unwrap().density(), Density::ONE);
        }
    }

    #[test]
    fn inherited_parent_request_yields_to_late_own_preference() {
        let mut parent = ScaleState::new(true, true, 2);
        let mut child = ScaleState::new(true, true, 2);
        parent.preferred_changed(150);
        child.inherit_density(Some(parent.requested_density()));
        assert_eq!(child.preferred, None);
        assert_eq!(child.plan((101, 79)).unwrap().density().units_120(), 150);
        child.committed = child.plan((101, 79));
        let committed = child.committed;
        for units in [0, 210, 210, 0] {
            child.preferred_changed(units);
            assert_eq!(child.committed, committed);
        }
        parent.preferred_changed(180);
        child.inherit_density(Some(parent.requested_density()));
        assert_eq!(child.requested_density().units_120(), 210);
        child.inherit_density(None);
        assert_eq!(child.requested_density().units_120(), 210);

        let mut delayed = ScaleState::new(true, true, 1);
        delayed.inherit_density(Some(parent.requested_density()));
        delayed.preferred_changed(0);
        assert_eq!(delayed.requested_density().units_120(), 180);
        delayed.inherit_density(None);
        delayed.integer_changed(2);
        delayed.output_changed(4);
        assert_eq!(delayed.requested_density().units_120(), 240);
        parent.integer_changed(3);
        parent.output_changed(4);
        assert_eq!(parent.requested_density().units_120(), 180);
    }

    #[test]
    fn inheritance_is_before_parent_budget_reduction() {
        let mut parent = ScaleState::new(true, true, 1);
        let mut child = ScaleState::new(true, true, 1);
        parent.integer_changed(3);
        child.inherit_density(Some(parent.requested_density()));
        assert_eq!(
            parent.plan((2048, 2048)).unwrap().density().units_120(),
            240
        );
        assert_eq!(child.plan((101, 79)).unwrap().density().units_120(), 360);
        assert_eq!(parent.inherited_density, None);
        parent.output_changed(4);
        assert_eq!(parent.requested_density().units_120(), 360);
    }
}
