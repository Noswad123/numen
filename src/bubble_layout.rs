//! Pure logical-pixel placement for a bubble surface independent of its sprite.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TailEdge {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tail {
    pub edge: TailEdge,
    /// Logical surface pixels along the edge, not a normalized fraction.
    pub position: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BubbleLayout {
    pub origin: (i32, i32),
    pub size: (u32, u32),
    pub tail: Option<Tail>,
}

/// Reflow within free space without moving, resizing, or covering the sprite.
/// `minimum` is the scene's readability floor; no readable fit means hidden.
pub(crate) fn layout(
    origin: (i32, i32),
    sprite: (u32, u32),
    preferred: (u32, u32),
    minimum: (u32, u32),
    output: Option<(i32, i32)>,
) -> Option<BubbleLayout> {
    if sprite.0 == 0
        || sprite.1 == 0
        || minimum.0 == 0
        || minimum.1 == 0
        || minimum.0 > preferred.0
        || minimum.1 > preferred.1
    {
        return None;
    }

    let (sx, sy) = (i64::from(origin.0), i64::from(origin.1));
    let (sw, sh) = (i64::from(sprite.0), i64::from(sprite.1));
    let (pw, ph) = (i64::from(preferred.0), i64::from(preferred.1));
    let (mw, mh) = (i64::from(minimum.0), i64::from(minimum.1));
    let (cx, cy) = (sx + sw / 2, sy + sh / 2);

    let (x, y, w, h, edge) = if let Some((ow, oh)) = output {
        if ow <= 0 || oh <= 0 {
            return None;
        }
        let (ow, oh) = (i64::from(ow), i64::from(oh));
        // Clip only the free rectangles, never the sprite. This also handles
        // sprites partially or wholly outside a newly resized output.
        let candidates = [
            (0, 0, ow, sy.clamp(0, oh), TailEdge::Bottom),
            (0, (sy + sh).clamp(0, oh), ow, oh, TailEdge::Top),
            ((sx + sw).clamp(0, ow), 0, ow, oh, TailEdge::Left),
            (0, 0, sx.clamp(0, ow), oh, TailEdge::Right),
        ];
        let mut best = None;
        let mut best_area = 0;
        for (left, top, right, bottom, edge) in candidates {
            let (w, h) = (pw.min(right - left), ph.min(bottom - top));
            if w < mw || h < mh {
                continue;
            }
            // Full preferred size has the maximum possible area. Strictly
            // greater preserves above/below/right/left order for every tie.
            let area = w * h;
            if area <= best_area {
                continue;
            }
            let (x, y) = match edge {
                TailEdge::Bottom => ((cx - w / 2).clamp(left, right - w), bottom - h),
                TailEdge::Top => ((cx - w / 2).clamp(left, right - w), top),
                TailEdge::Left => (left, (cy - h / 2).clamp(top, bottom - h)),
                TailEdge::Right => (right - w, (cy - h / 2).clamp(top, bottom - h)),
            };
            best = Some((x, y, w, h, edge));
            best_area = area;
        }
        best?
    } else {
        let x = (cx - pw / 2).clamp(0, i64::from(i32::MAX));
        if sy >= ph {
            (x, sy - ph, pw, ph, TailEdge::Bottom)
        } else {
            (x, (sy + sh).max(0), pw, ph, TailEdge::Top)
        }
    };

    let (length, target) = match edge {
        TailEdge::Top | TailEdge::Bottom => (w, cx - x),
        TailEdge::Left | TailEdge::Right => (h, cy - y),
    };
    // Leave a pixel on either side of the target for safe interior dots.
    let tail = (length >= 3).then(|| Tail {
        edge,
        position: target.clamp(1, length - 2) as u32,
    });
    Some(BubbleLayout {
        origin: (i32::try_from(x).ok()?, i32::try_from(y).ok()?),
        size: (w as u32, h as u32),
        tail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFERRED: (u32, u32) = (576, 216);
    const MINIMUM: (u32, u32) = (120, 80);

    fn assert_safe(
        bubble: BubbleLayout,
        origin: (i32, i32),
        sprite: (u32, u32),
        preferred: (u32, u32),
        minimum: (u32, u32),
        output: Option<(i32, i32)>,
    ) {
        let (x, y) = (i64::from(bubble.origin.0), i64::from(bubble.origin.1));
        let (w, h) = (i64::from(bubble.size.0), i64::from(bubble.size.1));
        let (sx, sy) = (i64::from(origin.0), i64::from(origin.1));
        let (sw, sh) = (i64::from(sprite.0), i64::from(sprite.1));
        assert!(bubble.size.0 >= minimum.0 && bubble.size.0 <= preferred.0);
        assert!(bubble.size.1 >= minimum.1 && bubble.size.1 <= preferred.1);
        assert!(x + w <= sx || sx + sw <= x || y + h <= sy || sy + sh <= y);
        assert!(x >= 0 && y >= 0);
        if let Some((ow, oh)) = output {
            assert!(x + w <= i64::from(ow) && y + h <= i64::from(oh));
        }
        if let Some(tail) = bubble.tail {
            let length = match tail.edge {
                TailEdge::Top | TailEdge::Bottom => bubble.size.0,
                TailEdge::Left | TailEdge::Right => bubble.size.1,
            };
            assert!(tail.position > 0 && tail.position < length - 1);
            match tail.edge {
                TailEdge::Bottom => assert!(y + h <= sy),
                TailEdge::Top => assert!(y >= sy + sh),
                TailEdge::Left => assert!(x >= sx + sw),
                TailEdge::Right => assert!(x + w <= sx),
            }
        }
    }

    #[test]
    fn corners_and_edges_preserve_full_size() {
        for (origin, edge) in [
            ((0, 0), TailEdge::Top),
            ((1180, 0), TailEdge::Top),
            ((0, 700), TailEdge::Bottom),
            ((1180, 700), TailEdge::Bottom),
            ((590, 0), TailEdge::Top),
            ((590, 700), TailEdge::Bottom),
            ((0, 350), TailEdge::Bottom),
            ((1180, 350), TailEdge::Bottom),
        ] {
            let bubble = layout(origin, (100, 100), PREFERRED, MINIMUM, Some((1280, 800))).unwrap();
            assert_eq!(bubble.size, PREFERRED);
            assert_eq!(bubble.tail.unwrap().edge, edge);
            assert_safe(
                bubble,
                origin,
                (100, 100),
                PREFERRED,
                MINIMUM,
                Some((1280, 800)),
            );
        }
    }

    #[test]
    fn narrow_output_reflows_instead_of_scaling() {
        let bubble = layout((100, 400), (100, 100), PREFERRED, MINIMUM, Some((320, 600))).unwrap();
        assert_eq!(bubble.origin, (0, 184));
        assert_eq!(bubble.size, (320, 216));
        assert_eq!(
            bubble.tail,
            Some(Tail {
                edge: TailEdge::Bottom,
                position: 150
            })
        );
    }

    #[test]
    fn lateral_fallback_and_preferred_size_beat_vertical_reflow() {
        for (origin, edge, x) in [
            ((0, 70), TailEdge::Left, 100),
            ((1100, 70), TailEdge::Right, 524),
        ] {
            let bubble = layout(origin, (100, 100), PREFERRED, MINIMUM, Some((1200, 250))).unwrap();
            assert_eq!(bubble.origin, (x, 12));
            assert_eq!(bubble.size, PREFERRED);
            assert_eq!(bubble.tail.unwrap().edge, edge);
        }
        let bubble = layout((0, 0), (100, 100), PREFERRED, MINIMUM, Some((800, 180))).unwrap();
        assert_eq!(bubble.size, (576, 180));
        assert_eq!(bubble.tail.unwrap().edge, TailEdge::Left);
    }

    #[test]
    fn no_readable_space_and_oversized_sprite_hide() {
        for (origin, sprite) in [
            ((0, 0), (300, 200)),
            ((-10, -10), (400, 300)),
            ((80, 50), (150, 100)),
        ] {
            assert_eq!(
                layout(origin, sprite, PREFERRED, MINIMUM, Some((300, 200))),
                None
            );
        }
    }

    #[test]
    fn output_changes_are_recomputed_without_state() {
        let origin = (100, 300);
        let large = layout(origin, (100, 100), PREFERRED, MINIMUM, Some((1000, 800))).unwrap();
        let small = layout(origin, (100, 100), PREFERRED, MINIMUM, Some((320, 400))).unwrap();
        assert_eq!(large.size, PREFERRED);
        assert_eq!(small.size, (320, 216));
        assert_eq!(
            layout(origin, (100, 100), PREFERRED, MINIMUM, Some((100, 60))),
            None
        );
        assert_eq!(
            layout(origin, (100, 100), PREFERRED, MINIMUM, Some((1000, 800))),
            Some(large)
        );
    }

    #[test]
    fn unknown_output_prefers_above_otherwise_below() {
        for (origin, expected, edge) in [
            ((400, 216), (162, 0), TailEdge::Bottom),
            ((0, 215), (0, 315), TailEdge::Top),
            ((-100, -200), (0, 0), TailEdge::Top),
        ] {
            let bubble = layout(origin, (100, 100), PREFERRED, MINIMUM, None).unwrap();
            assert_eq!(bubble.origin, expected);
            assert_eq!(bubble.size, PREFERRED);
            assert_eq!(bubble.tail.unwrap().edge, edge);
            assert_safe(bubble, origin, (100, 100), PREFERRED, MINIMUM, None);
        }
    }

    #[test]
    fn invalid_dimensions_hide_and_tiny_edges_omit_tail() {
        for (sprite, preferred, minimum, output) in [
            ((0, 1), (2, 2), (1, 1), None),
            ((1, 0), (2, 2), (1, 1), None),
            ((1, 1), (0, 2), (1, 1), None),
            ((1, 1), (2, 2), (0, 1), None),
            ((1, 1), (2, 2), (1, 0), None),
            ((1, 1), (2, 2), (3, 1), None),
            ((1, 1), (2, 2), (1, 3), None),
            ((1, 1), (2, 2), (1, 1), Some((0, 10))),
            ((1, 1), (2, 2), (1, 1), Some((10, -1))),
        ] {
            assert_eq!(layout((0, 0), sprite, preferred, minimum, output), None);
        }
        assert_eq!(
            layout((0, 2), (1, 1), (2, 2), (1, 1), None).unwrap().tail,
            None
        );
    }

    #[test]
    fn exhaustive_small_geometry_matches_free_space_policy() {
        for ow in 1..=6 {
            for oh in 1..=6 {
                for sx in -1..=ow {
                    for sy in -1..=oh {
                        for sw in 1..=3 {
                            for sh in 1..=3 {
                                for preferred in [(1, 1), (3, 2), (2, 4)] {
                                    for minimum in [(1, 1), preferred] {
                                        let result = layout(
                                            (sx, sy),
                                            (sw, sh),
                                            preferred,
                                            minimum,
                                            Some((ow, oh)),
                                        );
                                        // Independent brute-force oracle over all readable sizes
                                        // and positions, ranked by area then direction.
                                        let mut expected = None;
                                        let mut score = (0, 0);
                                        for w in minimum.0..=preferred.0 {
                                            for h in minimum.1..=preferred.1 {
                                                for x in 0..=(ow - w as i32) {
                                                    for y in 0..=(oh - h as i32) {
                                                        let direction = if y + h as i32 <= sy {
                                                            4
                                                        } else if y >= sy + sh as i32 {
                                                            3
                                                        } else if x >= sx + sw as i32 {
                                                            2
                                                        } else if x + w as i32 <= sx {
                                                            1
                                                        } else {
                                                            continue;
                                                        };
                                                        let candidate = (w * h, direction);
                                                        if candidate > score {
                                                            score = candidate;
                                                            expected = Some((w, h));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        assert_eq!(result.map(|b| b.size), expected);
                                        if let Some(bubble) = result {
                                            assert_safe(
                                                bubble,
                                                (sx, sy),
                                                (sw, sh),
                                                preferred,
                                                minimum,
                                                Some((ow, oh)),
                                            );
                                            if let Some(tail) = bubble.tail {
                                                let direction = match tail.edge {
                                                    TailEdge::Bottom => 4,
                                                    TailEdge::Top => 3,
                                                    TailEdge::Left => 2,
                                                    TailEdge::Right => 1,
                                                };
                                                assert_eq!(direction, score.1);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn extreme_integers_and_deterministic_random_inputs_are_safe() {
        for x in [i32::MIN, -1, 0, i32::MAX] {
            for y in [i32::MIN, -1, 0, i32::MAX] {
                for sprite in [(1, 1), (u32::MAX, u32::MAX)] {
                    for preferred in [PREFERRED, (u32::MAX, u32::MAX)] {
                        for output in [None, Some((i32::MAX, i32::MAX)), Some((1, 1))] {
                            if let Some(bubble) = layout((x, y), sprite, preferred, (1, 1), output)
                            {
                                assert_safe(bubble, (x, y), sprite, preferred, (1, 1), output);
                            }
                        }
                    }
                }
            }
        }
        // Dependency-free, reproducible full-width fuzzing, including invalid outputs.
        let mut seed = 0x1234_5678_u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..20_000 {
            let origin = (next() as i32, next() as i32);
            let sprite = (next(), next());
            let preferred = (next(), next());
            let output = Some((next() as i32, next() as i32));
            if let Some(bubble) = layout(origin, sprite, preferred, (1, 1), output) {
                assert_safe(bubble, origin, sprite, preferred, (1, 1), output);
            }
        }
        assert_eq!(
            layout((0, i32::MAX), (1, u32::MAX), (1, u32::MAX), (1, 1), None),
            None
        );
    }
}
