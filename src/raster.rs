//! Wayland-independent raster geometry and allocation limits.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Density(u32);

impl Density {
    pub const ONE: Self = Self(120);

    /// Protocol preferences below 1x are valid; `fit` still allocates at least 1x.
    pub fn from_preferred_scale(units_120: u32) -> Option<Self> {
        (units_120 > 0).then_some(Self(units_120))
    }

    pub fn from_integer(scale: i32) -> Self {
        Self((scale.max(1) as u32).min(u32::MAX / 120) * 120)
    }

    pub fn units_120(self) -> u32 {
        self.0
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 120.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RasterMode {
    IntegerBufferScale,
    ViewportDestination,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RasterSpec {
    logical: (u32, u32),
    physical: (u32, u32),
    density: Density,
    mode: RasterMode,
}

fn fits((w, h): (u32, u32)) -> bool {
    w > 0 && h > 0 && w <= 8192 && h <= 8192 && u64::from(w) * u64::from(h) <= 16 * 1024 * 1024
}

impl RasterSpec {
    /// Reduce density, never logical size. Search takes at most 32 steps even
    /// for untrusted preferences. Integer mode floors preferences to whole steps.
    pub fn fit(logical: (u32, u32), requested: Density, mode: RasterMode) -> Option<Self> {
        if !fits(logical) {
            return None;
        }
        let step = match mode {
            RasterMode::IntegerBufferScale => 120,
            RasterMode::ViewportDestination => 1,
        };
        let physical = |n: u32| -> Option<(u32, u32)> {
            let axis = |size: u32| {
                let product = u64::from(size).checked_mul(u64::from(n) * u64::from(step))?;
                u32::try_from(product.checked_add(119)? / 120).ok()
            };
            let size = (axis(logical.0)?, axis(logical.1)?);
            fits(size).then_some(size)
        };
        let mut low = 120 / step;
        let mut high = requested.0.max(120) / step;
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if physical(mid).is_some() {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        Some(Self {
            logical,
            physical: physical(low)?,
            density: Density(low * step),
            mode,
        })
    }

    /// Compatibility painter geometry, NOT preference authority. Protocol
    /// adapters must use `fit`. Preserve both physical axes exactly, even when
    /// no single density could reproduce them; density is informational only.
    #[cfg(test)]
    pub fn from_sizes(logical: (u32, u32), physical: (u32, u32)) -> Option<Self> {
        if !fits(logical) || !fits(physical) {
            return None;
        }
        let units = |p, l| (u64::from(p) * 120 / u64::from(l)) as u32;
        Some(Self {
            logical,
            physical,
            density: Density(
                units(physical.0, logical.0)
                    .min(units(physical.1, logical.1))
                    .max(1),
            ),
            mode: RasterMode::ViewportDestination,
        })
    }

    pub fn logical_size(self) -> (u32, u32) {
        self.logical
    }
    pub fn physical_size(self) -> (u32, u32) {
        self.physical
    }
    pub fn density(self) -> Density {
        self.density
    }
    pub fn mode(self) -> RasterMode {
        self.mode
    }
    #[cfg(test)]
    pub fn scale_x(self) -> f64 {
        f64::from(self.physical.0) / f64::from(self.logical.0)
    }
    #[cfg(test)]
    pub fn scale_y(self) -> f64 {
        f64::from(self.physical.1) / f64::from(self.logical.1)
    }

    pub fn logical_to_buffer(self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            x * f64::from(self.physical.0) / f64::from(self.logical.0),
            y * f64::from(self.physical.1) / f64::from(self.logical.1),
        )
    }

    pub fn buffer_to_logical(self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            x * f64::from(self.logical.0) / f64::from(self.physical.0),
            y * f64::from(self.logical.1) / f64::from(self.physical.1),
        )
    }

    pub fn covering_buffer_rect(self, rect: (u32, u32, u32, u32)) -> (u32, u32, u32, u32) {
        project_rect(rect, self.logical, self.physical)
    }

    #[cfg(test)]
    pub fn covering_logical_rect(self, rect: (u32, u32, u32, u32)) -> (u32, u32, u32, u32) {
        project_rect(rect, self.physical, self.logical)
    }
}

/// XYWH bounds use exact rational floor/ceil, not floating point density.
fn project_rect(
    (x, y, w, h): (u32, u32, u32, u32),
    source: (u32, u32),
    dest: (u32, u32),
) -> (u32, u32, u32, u32) {
    let axis = |start: u32, length: u32, src: u32, dst: u32| {
        let start = start.min(src);
        let end = start.saturating_add(length).min(src);
        let floor = (u64::from(start) * u64::from(dst) / u64::from(src)) as u32;
        let ceil = (u64::from(end) * u64::from(dst)).div_ceil(u64::from(src)) as u32;
        (
            floor,
            if length == 0 || start == end {
                0
            } else {
                ceil - floor
            },
        )
    };
    let (x, w) = axis(x, w, source.0, dest.0);
    let (y, h) = axis(y, h, source.1, dest.1);
    (x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_odd_geometry_and_integer_equivalence() {
        for (units, expected) in [
            (120, (101, 79)),
            (150, (127, 99)),
            (180, (152, 119)),
            (210, (177, 139)),
            (240, (202, 158)),
        ] {
            let raster =
                RasterSpec::fit((101, 79), Density(units), RasterMode::ViewportDestination)
                    .unwrap();
            assert_eq!(raster.physical_size(), expected);
            assert_eq!(raster.logical_size(), (101, 79));
            assert_eq!(raster.density().units_120(), units);
            assert_eq!(raster.mode(), RasterMode::ViewportDestination);
        }
        for scale in 1..=8 {
            let a = RasterSpec::fit(
                (101, 79),
                Density::from_integer(scale),
                RasterMode::IntegerBufferScale,
            )
            .unwrap();
            let b = RasterSpec::fit(
                (101, 79),
                Density::from_integer(scale),
                RasterMode::ViewportDestination,
            )
            .unwrap();
            assert_eq!(a.physical_size(), (101 * scale as u32, 79 * scale as u32));
            assert_eq!(a.physical_size(), b.physical_size());
        }
    }

    #[test]
    fn preferences_limits_and_largest_fitting_density() {
        assert_eq!(Density::from_preferred_scale(0), None);
        assert_eq!(Density::from_integer(i32::MIN), Density::ONE);
        assert_eq!(
            Density::from_integer(i32::MAX).units_120(),
            (u32::MAX / 120) * 120
        );
        assert_eq!(Density(150).as_f64(), 1.25);
        for mode in [
            RasterMode::ViewportDestination,
            RasterMode::IntegerBufferScale,
        ] {
            for size in [
                (0, 1),
                (1, 0),
                (8193, 1),
                (4097, 4096),
                (u32::MAX, u32::MAX),
            ] {
                assert!(RasterSpec::fit(size, Density::ONE, mode).is_none());
            }
            for size in [(101, 79), (1536, 1664), (8192, 1), (4096, 4096)] {
                let raster = RasterSpec::fit(size, Density(u32::MAX), mode).unwrap();
                assert_eq!(raster.logical_size(), size);
                assert!(fits(raster.physical_size()));
                let next = raster.density.0
                    + if mode == RasterMode::IntegerBufferScale {
                        120
                    } else {
                        1
                    };
                assert_eq!(RasterSpec::fit(size, Density(next), mode).unwrap(), raster);
                assert_eq!(
                    RasterSpec::fit(size, Density(90), mode)
                        .unwrap()
                        .physical_size(),
                    size
                );
            }
        }
        assert!(RasterSpec::from_sizes((1, 1), (0, 1)).is_none());
        assert!(RasterSpec::from_sizes((1, 1), (8192, 8192)).is_none());
    }

    #[test]
    fn exact_axes_edges_roundtrips_and_rectangle_bounds() {
        let raster = RasterSpec::from_sizes((101, 79), (127, 103)).unwrap();
        assert_eq!(raster.physical_size(), (127, 103));
        assert_eq!(raster.density(), Density(150));
        assert_ne!(raster.scale_x(), raster.scale_y());
        assert_eq!(raster.logical_to_buffer((101.0, 79.0)), (127.0, 103.0));
        assert_eq!(raster.buffer_to_logical((127.0, 103.0)), (101.0, 79.0));
        let mut previous = (-1.0, -1.0);
        for i in 0..=1000 {
            let p = (101.0 * i as f64 / 1000.0, 79.0 * i as f64 / 1000.0);
            let mapped = raster.logical_to_buffer(p);
            assert!(mapped.0 >= previous.0 && mapped.1 >= previous.1);
            let back = raster.buffer_to_logical(mapped);
            assert!((p.0 - back.0).abs() < 1e-12 && (p.1 - back.1).abs() < 1e-12);
            previous = mapped;
        }
        assert_eq!(
            raster.covering_buffer_rect((0, 0, 101, 79)),
            (0, 0, 127, 103)
        );
        assert_eq!(
            raster.covering_logical_rect((0, 0, 127, 103)),
            (0, 0, 101, 79)
        );
        assert_eq!(
            raster.covering_buffer_rect((u32::MAX, u32::MAX, u32::MAX, u32::MAX)),
            (127, 103, 0, 0)
        );
        for x in 0..101 {
            for y in 0..79 {
                let (px, py, pw, ph) = raster.covering_buffer_rect((x, y, 1, 1));
                assert!(px + pw <= 127 && py + ph <= 103);
                let (lx, ly, lw, lh) = raster.covering_logical_rect((px, py, pw, ph));
                assert!(lx <= x && ly <= y && lx + lw > x && ly + lh > y);
            }
        }
        assert_eq!(raster.covering_buffer_rect((1, 1, 0, 0)).2, 0);
    }
}
