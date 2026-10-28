//! Read-only live pointer snapshots, without event posting, taps, or permission
//! requests. Poll only for active motion, once per frame.
//!
//! Quartz event locations and winit desktop logical coordinates both use the
//! main display's top-left origin, including negative virtual-desktop positions.
//! Pair with `outer_position().to_logical(window.scale_factor())` and
//! `set_outer_position(LogicalPosition)`; do not flip Y or apply another DPI scale.

use crate::motion::Point;
use anyhow::Result;

#[cfg(target_os = "macos")]
use core_graphics::{
    event::CGEvent,
    event_source::{CGEventSource, CGEventSourceStateID},
};

pub struct PointerReader {
    #[cfg(target_os = "macos")]
    source: CGEventSource,
}

pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

impl PointerReader {
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
                .map_err(|()| anyhow::anyhow!("could not create the pointer snapshot source"))?;
            Ok(Self { source })
        }
        #[cfg(not(target_os = "macos"))]
        {
            anyhow::bail!("live pointer reading is supported only on macOS")
        }
    }

    pub fn position(&self) -> Result<Point> {
        #[cfg(target_os = "macos")]
        {
            // A retained event would retain an old location. Create a fresh
            // snapshot every poll, while reusing the session source.
            let event = CGEvent::new(self.source.clone())
                .map_err(|()| anyhow::anyhow!("could not snapshot the live pointer"))?;
            let location = event.location();
            anyhow::ensure!(
                location.x.is_finite() && location.y.is_finite(),
                "pointer snapshot contains nonfinite coordinates"
            );
            Ok(Point {
                x: location.x,
                y: location.y,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            anyhow::bail!("live pointer reading is supported only on macOS")
        }
    }
}
