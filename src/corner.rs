//! Stable full-frame corner placement, deliberately independent of pose alpha.
//! A 12-point sprite margin keeps every animation visible; speech bubbles may
//! extend outside the work area. Oversized frames use a centered best fit.
use crate::motion::Point;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Area {
    pub fn valid(self) -> bool {
        self.width > 0.0
            && self.height > 0.0
            && [
                self.x,
                self.y,
                self.width,
                self.height,
                self.x + self.width,
                self.y + self.height,
            ]
            .iter()
            .all(|v| v.is_finite())
    }

    pub fn to_desktop(self, primary_max_y: f64) -> Option<Self> {
        let area = Self {
            y: primary_max_y - self.y - self.height,
            ..self
        };
        area.valid().then_some(area)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// Target is the full projected sprite's foot center, not the outer window.
    pub fn target(self, area: Area, width: f64, height: f64) -> Option<Point> {
        if !area.valid()
            || !width.is_finite()
            || !height.is_finite()
            || width <= 0.0
            || height <= 0.0
        {
            return None;
        }
        let axis = |start: f64, extent: f64, sprite: f64, far: bool| {
            if sprite + 24.0 > extent {
                start + (extent - sprite) / 2.0
            } else if far {
                start + extent - 12.0 - sprite
            } else {
                start + 12.0
            }
        };
        let point = Point {
            x: axis(
                area.x,
                area.width,
                width,
                matches!(self, Self::TopRight | Self::BottomRight),
            ) + width / 2.0,
            y: axis(
                area.y,
                area.height,
                height,
                matches!(self, Self::BottomLeft | Self::BottomRight),
            ) + height,
        };
        (point.x.is_finite() && point.y.is_finite()).then_some(point)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayArea {
    pub id: u32,
    pub frame: Area,
    pub work: Area,
}

/// Projection rounds both raster edges. Each density can contribute up to one
/// physical pixel of size error; changing density alone is not a sprite edit.
pub fn same_sprite_size(
    original: Point,
    original_backing: f64,
    current: Point,
    backing: f64,
) -> bool {
    if ![
        original.x,
        original.y,
        current.x,
        current.y,
        original_backing,
        backing,
    ]
    .iter()
    .all(|v| v.is_finite())
        || original_backing <= 0.0
        || backing <= 0.0
    {
        return false;
    }
    let tolerance = 1.0 / original_backing + 1.0 / backing;
    (original.x - current.x).abs() <= tolerance && (original.y - current.y).abs() <= tolerance
}

/// Uniform over all four corners of every connected, usable display. Selection
/// happens once per request; the chosen display remains fixed during travel.
pub fn random_target(
    displays: &[DisplayArea],
    width: f64,
    height: f64,
) -> Option<(DisplayArea, Point)> {
    let count = u32::try_from(displays.len().checked_mul(4)?).ok()?;
    if count == 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    let index = unsafe { libc::arc4random_uniform(count) } as usize;
    #[cfg(not(target_os = "macos"))]
    let index = 0;
    target_at(displays, index, width, height)
}

fn target_at(
    displays: &[DisplayArea],
    index: usize,
    width: f64,
    height: f64,
) -> Option<(DisplayArea, Point)> {
    let display = *displays.get(index / 4)?;
    let corner = [
        Corner::TopLeft,
        Corner::TopRight,
        Corner::BottomLeft,
        Corner::BottomRight,
    ][index % 4];
    Some((display, corner.target(display.work, width, height)?))
}

#[cfg(target_os = "macos")]
pub fn display(monitor: &winit::monitor::MonitorHandle) -> Option<DisplayArea> {
    use objc2_app_kit::NSScreen;
    use objc2_foundation::MainThreadMarker;
    use winit::platform::macos::MonitorHandleExtMacOS;
    let mtm = MainThreadMarker::new()?;
    let screens = NSScreen::screens(mtm);
    // The array contains retained NSScreen objects and is alive for this query.
    let primary = unsafe { screens.firstObject()? }.frame();
    let max_y = primary.origin.y + primary.size.height;
    // Winit resolves this display's NSScreen on the main thread. Read only while
    // the monitor and current screens array are alive; never mutate AppKit state.
    let screen = unsafe { (monitor.ns_screen()? as *const NSScreen).as_ref()? };
    let convert = |rect: objc2_foundation::NSRect| {
        Area {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        }
        .to_desktop(max_y)
    };
    Some(DisplayArea {
        id: monitor.native_id(),
        frame: convert(screen.frame())?,
        work: convert(screen.visibleFrame())?,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn display(_: &winit::monitor::MonitorHandle) -> Option<DisplayArea> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_connected_display_and_corner_is_selectable() {
        let displays: Vec<_> = [(0.0, 0.0), (-1512.0, 723.0), (-1920.0, -357.0)]
            .into_iter()
            .enumerate()
            .map(|(id, (x, y))| {
                let area = Area {
                    x,
                    y,
                    width: 1200.0,
                    height: 900.0,
                };
                DisplayArea {
                    id: id as u32,
                    frame: area,
                    work: area,
                }
            })
            .collect();
        for (index, display) in displays.iter().enumerate() {
            for (offset, corner) in [
                Corner::TopLeft,
                Corner::TopRight,
                Corner::BottomLeft,
                Corner::BottomRight,
            ]
            .into_iter()
            .enumerate()
            {
                let (selected, target) =
                    target_at(&displays, index * 4 + offset, 144.0, 156.0).unwrap();
                assert_eq!(selected, *display);
                assert_eq!(Some(target), corner.target(display.work, 144.0, 156.0));
            }
        }
        assert!(target_at(&displays, 12, 144.0, 156.0).is_none());
        assert!(random_target(&[], 144.0, 156.0).is_none());
        assert!(random_target(&displays, f64::NAN, 156.0).is_none());
    }

    #[test]
    fn corners_negative_coordinates_fractional_sizes_and_best_fit() {
        let area = Area {
            x: -1000.0,
            y: -500.0,
            width: 800.0,
            height: 600.0,
        };
        for (corner, x, y) in [
            (Corner::TopLeft, -937.75, -387.5),
            (Corner::TopRight, -262.25, -387.5),
            (Corner::BottomLeft, -937.75, 88.0),
            (Corner::BottomRight, -262.25, 88.0),
        ] {
            assert_eq!(corner.target(area, 100.5, 100.5), Some(Point { x, y }));
            assert_eq!(
                corner.target(area, 1000.0, 800.0),
                Some(Point {
                    x: -600.0,
                    y: 200.0
                })
            );
        }
        assert!(Corner::TopLeft.target(area, f64::NAN, 10.0).is_none());
        assert!(
            Corner::TopLeft
                .target(Area { width: 0.0, ..area }, 10.0, 10.0)
                .is_none()
        );
        assert_eq!(area.to_desktop(900.0).unwrap().y, 800.0);
    }
}
