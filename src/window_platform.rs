//! Desktop surface policy shared by the Linux and macOS window paths.

use pixels::wgpu::{BlendState, CompositeAlphaMode};
use winit::event_loop::ActiveEventLoop;
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::{Window, WindowAttributes};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DesktopBackend {
    Wayland,
    X11,
    Other,
}

impl DesktopBackend {
    pub(super) fn for_window(window: &Window) -> Self {
        match window.display_handle().map(|handle| handle.as_raw()) {
            Ok(RawDisplayHandle::Wayland(_)) => Self::Wayland,
            Ok(RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_)) => Self::X11,
            _ => Self::Other,
        }
    }

    pub(super) fn absolute_position(self) -> bool {
        // An ordinary Wayland toplevel has no global coordinate/placement API.
        self != Self::Wayland
    }
}

pub(super) fn identify(
    attributes: WindowAttributes,
    _event_loop: &ActiveEventLoop,
) -> WindowAttributes {
    #[cfg(target_os = "linux")]
    {
        use winit::platform::{
            wayland::{ActiveEventLoopExtWayland, WindowAttributesExtWayland},
            x11::WindowAttributesExtX11,
        };
        // Both extension methods write the same platform-specific name field.
        // X11's argument order is (general_class, instance), not the reverse.
        if _event_loop.is_wayland() {
            WindowAttributesExtWayland::with_name(attributes, "numen", "numen")
        } else {
            WindowAttributesExtX11::with_name(attributes, "Numen", "numen")
        }
    }
    #[cfg(not(target_os = "linux"))]
    attributes
}

pub(super) fn blend_state(alpha: CompositeAlphaMode) -> BlendState {
    match alpha {
        CompositeAlphaMode::Opaque => BlendState::ALPHA_BLENDING,
        _ => BlendState::REPLACE,
    }
}

/// Wayland buffers use premultiplication of encoded RGB, not linear-light RGB.
/// Apply only to a freshly composed frame; subsequent redraws reuse those bytes.
/// The sRGB texture/target and REPLACE blend preserve the encoded values.
pub(super) fn premultiply_rgba(frame: &mut [u8]) {
    for rgba in frame.as_chunks_mut::<4>().0 {
        let alpha = u16::from(rgba[3]);
        for channel in &mut rgba[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

/// Retry only an unsupported alpha mode, not device/surface creation failures.
/// Never silently replace a requested transparent window with an opaque one.
pub(super) fn negotiate_alpha<T>(
    opaque: bool,
    mut create: impl FnMut(CompositeAlphaMode, BlendState) -> Result<T, pixels::Error>,
) -> Result<(T, CompositeAlphaMode), pixels::Error> {
    let preferred = if opaque {
        CompositeAlphaMode::Opaque
    } else {
        CompositeAlphaMode::PostMultiplied
    };
    match create(preferred, blend_state(preferred)) {
        Err(pixels::Error::InvalidAlphaMode(_)) if !opaque => {
            let fallback = CompositeAlphaMode::PreMultiplied;
            create(fallback, blend_state(fallback)).map(|value| (value, fallback))
        }
        result => result.map(|value| (value, preferred)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_leaves_absolute_placement_to_compositor() {
        assert!(!DesktopBackend::Wayland.absolute_position());
        assert!(DesktopBackend::X11.absolute_position());
        assert!(DesktopBackend::Other.absolute_position());
    }

    #[test]
    fn straight_alpha_is_preferred_when_supported() {
        let ((), alpha) = negotiate_alpha(false, |alpha, blend| {
            assert_eq!(alpha, CompositeAlphaMode::PostMultiplied);
            assert_eq!(blend, BlendState::REPLACE);
            Ok(())
        })
        .unwrap();
        assert_eq!(alpha, CompositeAlphaMode::PostMultiplied);
    }

    #[test]
    fn premultiplied_surface_preserves_prepared_bytes() {
        let mut attempts = Vec::new();
        let (_, alpha) = negotiate_alpha(false, |alpha, blend| {
            attempts.push(alpha);
            if alpha == CompositeAlphaMode::PostMultiplied {
                Err(pixels::Error::InvalidAlphaMode(alpha))
            } else {
                assert_eq!(blend, BlendState::REPLACE);
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(alpha, CompositeAlphaMode::PreMultiplied);
        assert_eq!(
            attempts,
            [
                CompositeAlphaMode::PostMultiplied,
                CompositeAlphaMode::PreMultiplied
            ]
        );
    }

    #[test]
    fn premultiplication_uses_encoded_rgb_and_preserves_alpha() {
        let mut frame = [
            255, 255, 255, 128, 200, 100, 50, 128, 255, 200, 100, 0, 200, 100, 50, 255,
        ];
        premultiply_rgba(&mut frame);
        assert_eq!(
            frame,
            [
                128, 128, 128, 128, 100, 50, 25, 128, 0, 0, 0, 0, 200, 100, 50, 255
            ]
        );
    }

    #[test]
    fn opaque_and_device_errors_do_not_fall_back() {
        let mut attempts = 0;
        assert!(
            negotiate_alpha::<()>(true, |alpha, blend| {
                attempts += 1;
                assert_eq!(alpha, CompositeAlphaMode::Opaque);
                assert_eq!(blend, BlendState::ALPHA_BLENDING);
                Err(pixels::Error::InvalidAlphaMode(alpha))
            })
            .is_err()
        );
        assert_eq!(attempts, 1);
        attempts = 0;
        assert!(matches!(
            negotiate_alpha::<()>(false, |_, _| {
                attempts += 1;
                Err(pixels::Error::AdapterNotFound)
            }),
            Err(pixels::Error::AdapterNotFound)
        ));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn no_transparent_modes_is_an_error_not_an_opaque_window() {
        let mut attempts = 0;
        assert!(
            negotiate_alpha::<()>(false, |alpha, _| {
                attempts += 1;
                Err(pixels::Error::InvalidAlphaMode(alpha))
            })
            .is_err()
        );
        assert_eq!(attempts, 2);
    }
}
