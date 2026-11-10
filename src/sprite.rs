//! Borrowed source pixels; facing is applied exactly once by the painter.
use std::ops::Deref;

use crate::{animation::Animation, motion::Facing};

pub const LOGICAL_WIDTH: u32 = 192;
pub const LOGICAL_HEIGHT: u32 = 208;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpriteFrame<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub mirror_x: bool,
}

impl<'a> SpriteFrame<'a> {
    pub fn new(rgba: &'a [u8], width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0);
        assert_eq!(
            (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4)),
            Some(rgba.len())
        );
        Self {
            rgba,
            width,
            height,
            mirror_x: false,
        }
    }

    pub fn with_facing(mut self, facing: Facing) -> Self {
        self.mirror_x = facing == Facing::Left;
        self
    }

    pub fn source_offset(self, u: f64, v: f64) -> usize {
        source_offset(self.width, self.height, self.mirror_x, u, v)
    }
}

/// Quantize before mirroring so RGB and alpha select the same source pixel,
/// including exact boundaries when downsampling a high-frequency HD frame.
pub fn source_offset(width: u32, height: u32, mirror_x: bool, u: f64, v: f64) -> usize {
    assert!(width > 0 && height > 0);
    let mut x = ((u * width as f64) as u32).min(width - 1);
    let y = ((v * height as f64) as u32).min(height - 1);
    if mirror_x {
        x = width - 1 - x;
    }
    (y as usize * width as usize + x as usize) * 4
}

impl Deref for SpriteFrame<'_> {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        self.rgba
    }
}

#[cfg(test)]
impl<'a> From<&'a [u8]> for SpriteFrame<'a> {
    fn from(rgba: &'a [u8]) -> Self {
        Self::new(rgba, LOGICAL_WIDTH, LOGICAL_HEIGHT)
    }
}

#[cfg(test)]
impl<'a> From<&'a Vec<u8>> for SpriteFrame<'a> {
    fn from(rgba: &'a Vec<u8>) -> Self {
        Self::from(rgba.as_slice())
    }
}

#[cfg(test)]
impl<'a, const N: usize> From<&'a [u8; N]> for SpriteFrame<'a> {
    fn from(rgba: &'a [u8; N]) -> Self {
        Self::from(rgba.as_slice())
    }
}

/// FNV-1a is explicitly fixed, unlike process-seeded standard map hashers.
pub fn initial_facing(id: &str) -> Facing {
    let hash = id.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    if hash & 1 == 0 {
        Facing::Left
    } else {
        Facing::Right
    }
}

pub fn selected_facing(animation: Animation, current: Facing) -> Facing {
    match animation {
        Animation::RunningLeft => Facing::Left,
        Animation::RunningRight => Facing::Right,
        _ => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantizes_before_mirroring_all_channels_without_copying() {
        let rgba: Vec<u8> = (0..8).flat_map(|i| [i, 255 - i, i * 7, i * 31]).collect();
        let frame = SpriteFrame::new(&rgba, 4, 2);
        let left = frame.with_facing(Facing::Left);
        assert_eq!(left.rgba.as_ptr(), frame.rgba.as_ptr());
        for (u, x) in [
            (0.0, 3),
            (0.249, 3),
            (0.25, 2),
            (0.5, 1),
            (0.75, 0),
            (1.0, 0),
        ] {
            for (v, y) in [(0.0, 0), (0.5, 1), (1.0, 1)] {
                let offset = left.source_offset(u, v);
                assert_eq!(offset, (y * 4 + x) * 4);
                assert_eq!(&left[offset..offset + 4], &rgba[offset..offset + 4]);
            }
        }
        assert_eq!(left.with_facing(Facing::Left), left);
        assert_eq!(left.with_facing(Facing::Right), frame);
        assert_ne!(left, frame);
        assert_ne!(frame, SpriteFrame::new(&rgba, 2, 4));
    }

    #[test]
    fn deterministic_varied_initial_and_explicit_selection() {
        let facings: Vec<_> = (0..100)
            .map(|i| initial_facing(&format!("generated-{i}")))
            .collect();
        assert!(facings.contains(&Facing::Left) && facings.contains(&Facing::Right));
        for (i, facing) in facings.into_iter().enumerate() {
            assert_eq!(initial_facing(&format!("generated-{i}")), facing);
            assert_eq!(
                selected_facing(Animation::RunningLeft, facing),
                Facing::Left
            );
            assert_eq!(
                selected_facing(Animation::RunningRight, facing),
                Facing::Right
            );
            for animation in Animation::ALL {
                if !matches!(animation, Animation::RunningLeft | Animation::RunningRight) {
                    assert_eq!(selected_facing(animation, facing), facing);
                }
            }
        }
    }

    #[test]
    fn hd_sampling_keeps_asymmetric_rgb_and_alpha_together() {
        let rgba: Vec<u8> = (0..416)
            .flat_map(|y| {
                (0..384).flat_map(move |x| {
                    [
                        x as u8,
                        y as u8,
                        (x ^ y) as u8,
                        ((x * 71 + y * 13) % 256) as u8,
                    ]
                })
            })
            .collect();
        let frame = SpriteFrame::new(&rgba, 384, 416).with_facing(Facing::Left);
        for y in 0..208 {
            for x in 0..192 {
                let u = x as f64 / 192.0;
                let v = y as f64 / 208.0;
                let sx = 383 - (u * 384.0).floor() as usize;
                let sy = (v * 416.0).floor() as usize;
                let offset = frame.source_offset(u, v);
                assert_eq!(
                    &frame[offset..offset + 4],
                    &rgba[(sy * 384 + sx) * 4..(sy * 384 + sx + 1) * 4]
                );
            }
        }
    }

    #[test]
    #[should_panic]
    fn rejects_wrong_fixture_length() {
        let _ = SpriteFrame::from(&[0; 4]);
    }
}
