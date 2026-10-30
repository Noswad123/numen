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
    pub fn random() -> Self {
        #[cfg(target_os = "macos")]
        let index = unsafe { libc::arc4random_uniform(4) };
        #[cfg(not(target_os = "macos"))]
        let index = 0;
        match index {
            0 => Self::TopLeft,
            1 => Self::TopRight,
            2 => Self::BottomLeft,
            _ => Self::BottomRight,
        }
    }

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
