//! Pure software conversion and logical-coordinate helpers, also testable with rustc.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};

/// Only an explicit all request may broaden a subset. At capacity, discard the
/// oldest request, never reinterpret targeted requests as all.
#[derive(Default)]
pub struct Reveals(Mutex<VecDeque<Option<Vec<String>>>>);
impl Reveals {
    pub fn post(&self, ids: Option<Vec<String>>) {
        if ids.as_ref().is_some_and(|ids| ids.len() > 16) {
            return;
        }
        let mut pending = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if pending.iter().any(Option::is_none) {
            return;
        }
        if ids.is_none() {
            pending.clear();
        } else if pending.len() == 32 {
            pending.pop_front();
        }
        pending.push_back(ids);
    }
    pub fn take(&self) -> Option<Option<BTreeSet<String>>> {
        let requests = std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()));
        if requests.is_empty() {
            None
        } else if requests.iter().any(Option::is_none) {
            Some(None)
        } else {
            Some(Some(requests.into_iter().flatten().flatten().collect()))
        }
    }
}

#[derive(Default)]
pub struct CloseRequest {
    pub retry_at: Option<Instant>,
    submitted: bool,
}
impl CloseRequest {
    pub fn request(&mut self, now: Instant) {
        if !self.submitted && self.retry_at.is_none() {
            self.retry_at = Some(now);
        }
    }
    pub fn retry(&mut self, now: Instant, submit: impl FnOnce() -> bool) {
        if self.retry_at.is_some_and(|at| now >= at) {
            self.submitted = submit();
            self.retry_at = (!self.submitted).then_some(now + Duration::from_millis(100));
        }
    }
}

pub fn clamp_origin(
    origin: (i32, i32),
    output: Option<(i32, i32)>,
    sprite: (u32, u32),
) -> (i32, i32) {
    let (w, h) = output.unwrap_or((i32::MAX, i32::MAX));
    let bound = |extent: i32, size: u32| {
        extent
            .saturating_sub(size.min(i32::MAX as u32) as i32)
            .max(0)
    };
    (
        origin.0.clamp(0, bound(w, sprite.0)),
        origin.1.clamp(0, bound(h, sprite.1)),
    )
}

pub fn initial_origin(output: Option<(i32, i32)>, sprite: (u32, u32), index: usize) -> (i32, i32) {
    let stagger = (index % 8) as i32 * 36;
    output.map_or((24 + stagger, 24 + stagger), |(w, h)| {
        (
            w.saturating_sub(sprite.0.min(i32::MAX as u32) as i32)
                .saturating_sub(32 + stagger)
                .max(0),
            h.saturating_sub(sprite.1.min(i32::MAX as u32) as i32)
                .saturating_sub(32 + stagger)
                .max(0),
        )
    })
}

pub fn framebuffer_fits((width, height): (u32, u32)) -> bool {
    width > 0
        && height > 0
        && width <= 8192
        && height <= 8192
        && u64::from(width) * u64::from(height) <= 16 * 1024 * 1024
}

/// Reduce density, never logical geometry, before allocating either surface.
pub fn raster_scale(requested: i32, sprite: (u32, u32), bubble: (u32, u32)) -> Option<i32> {
    (1..=requested.clamp(1, 8192)).rev().find(|&scale| {
        [sprite, bubble].into_iter().all(|(w, h)| {
            w.checked_mul(scale as u32)
                .zip(h.checked_mul(scale as u32))
                .is_some_and(framebuffer_fits)
        })
    })
}

/// Output-relative origin: prefer above, then below, then clamp without resizing.
pub fn bubble_origin(
    origin: (i32, i32),
    sprite: (u32, u32),
    bubble: (u32, u32),
    output: Option<(i32, i32)>,
) -> (i32, i32) {
    let (x, y) = (i64::from(origin.0), i64::from(origin.1));
    let (sw, sh) = (i64::from(sprite.0), i64::from(sprite.1));
    let (bw, bh) = (i64::from(bubble.0), i64::from(bubble.1));
    let above = y - bh;
    let below = y + sh;
    let fits_y = |top: i64| top >= 0 && output.is_none_or(|(_, h)| top + bh <= i64::from(h));
    let top = if fits_y(above) {
        above
    } else if fits_y(below) {
        below
    } else {
        above
    };
    let desired = (
        (x + (sw - bw) / 2).clamp(0, i64::from(i32::MAX)) as i32,
        top.clamp(0, i64::from(i32::MAX)) as i32,
    );
    clamp_origin(desired, output, bubble)
}

/// Immutable emergency cursor when neither cursor-shape nor an installed theme works.
pub fn fallback_cursor() -> Vec<u8> {
    let mut rgba = vec![0; 16 * 24 * 4];
    for y in 0..20 {
        for x in 0..=y.min(12) / 2 {
            let edge = x == 0 || x == y.min(12) / 2 || y == 19;
            let color = if edge { 0 } else { 255 };
            rgba[(y * 16 + x) * 4..(y * 16 + x + 1) * 4]
                .copy_from_slice(&[color, color, color, 255]);
        }
    }
    rgba
}

pub fn write_argb(rgba: &[u8], canvas: &mut [u8], opaque: bool) {
    for (src, dst) in rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(canvas.as_chunks_mut::<4>().0)
    {
        let alpha = u32::from(src[3]);
        let channel = |c: u8| (u32::from(c) * alpha + 127) / 255;
        let argb = ((if opaque { 255 } else { alpha }) << 24)
            | (channel(src[0]) << 16)
            | (channel(src[1]) << 8)
            | channel(src[2]);
        dst.copy_from_slice(&argb.to_ne_bytes());
    }
}

/// Project actual nonzero framebuffer alpha to logical cells, then merge equal
/// horizontal runs vertically. Diagnostic black fill is deliberately excluded.
pub fn input_rectangles(
    rgba: &[u8],
    physical: (u32, u32),
    scale: u32,
) -> Vec<(i32, i32, i32, i32)> {
    if scale == 0
        || physical.0 == 0
        || physical.1 == 0
        || !physical.0.is_multiple_of(scale)
        || !physical.1.is_multiple_of(scale)
        || physical.0 > i32::MAX as u32
        || physical.1 > i32::MAX as u32
        || u64::from(physical.0) * u64::from(physical.1) * 4 != rgba.len() as u64
    {
        return Vec::new();
    }
    let (width, height) = (physical.0 / scale, physical.1 / scale);
    let mut result: Vec<(i32, i32, i32, i32)> = Vec::new();
    let mut previous: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for y in 0..height {
        let filled = |x: u32| {
            (y * scale..(y + 1) * scale).any(|py| {
                (x * scale..(x + 1) * scale)
                    .any(|px| rgba[(py as usize * physical.0 as usize + px as usize) * 4 + 3] != 0)
            })
        };
        let mut current = BTreeMap::new();
        let mut x = 0;
        while x < width {
            if !filled(x) {
                x += 1;
                continue;
            }
            let start = x;
            while x < width && filled(x) {
                x += 1;
            }
            let key = (start, x - start);
            let index = if let Some(&index) = previous.get(&key) {
                result[index].3 += 1;
                index
            } else {
                result.push((start as i32, y as i32, (x - start) as i32, 1));
                result.len() - 1
            };
            current.insert(key, index);
        }
        previous = current;
    }
    result
}

#[derive(Default)]
pub struct Gesture {
    owner: Option<u32>,
    start: Option<(f64, f64)>,
    origin: (i32, i32),
    dragging: bool,
}
impl Gesture {
    pub fn press(&mut self, pointer: u32, local: (f64, f64), origin: (i32, i32)) {
        if self.owner.is_some() {
            return;
        }
        if !local.0.is_finite() || !local.1.is_finite() {
            return;
        }
        self.owner = Some(pointer);
        self.origin = origin;
        self.start = Some((local.0 + f64::from(origin.0), local.1 + f64::from(origin.1)));
        self.dragging = false;
    }
    pub fn moved(
        &mut self,
        pointer: u32,
        local: (f64, f64),
        current_origin: (i32, i32),
    ) -> Option<(i32, i32)> {
        if self.owner != Some(pointer) {
            return None;
        }
        let start = self.start?;
        let dx = local.0 + f64::from(current_origin.0) - start.0;
        let dy = local.1 + f64::from(current_origin.1) - start.1;
        if !dx.is_finite() || !dy.is_finite() {
            *self = Self::default();
            return None;
        }
        self.dragging |= dx.hypot(dy) >= 5.0;
        self.dragging.then(|| {
            (
                (f64::from(self.origin.0) + dx).round().max(0.0) as i32,
                (f64::from(self.origin.1) + dy).round().max(0.0) as i32,
            )
        })
    }
    pub fn cancel(&mut self, pointer: u32) {
        if self.owner == Some(pointer) {
            *self = Self::default();
        }
    }
    pub fn release(&mut self, pointer: u32) -> bool {
        if self.owner != Some(pointer) {
            return false;
        }
        let click = self.start.is_some() && !self.dragging;
        *self = Self::default();
        click
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximum_valid_scene_geometry_fits_outputs_one_through_eight_without_resizing() {
        let sprite = (1536, 1664);
        let bubble = (1536, 576);
        for output_scale in 1..=8 {
            let density = raster_scale(output_scale, sprite, bubble).unwrap() as u32;
            assert!(density >= 1 && density <= output_scale as u32);
            for size in [sprite, bubble] {
                let physical = (size.0 * density, size.1 * density);
                assert!(framebuffer_fits(physical));
                assert_eq!((physical.0 / density, physical.1 / density), size);
            }
            assert_eq!(density, output_scale.min(2) as u32);
        }
        assert_eq!(raster_scale(8, (96, 104), (4096, 4096)), Some(1));
        assert_eq!(raster_scale(i32::MAX, sprite, bubble), Some(2));
        assert_eq!(raster_scale(8, (8193, 1), bubble), None);
    }

    #[test]
    fn bubble_placement_prefers_above_then_below_then_clamps_at_edges() {
        let output = Some((800, 600));
        let sprite = (100, 100);
        let bubble = (300, 150);
        assert_eq!(
            bubble_origin((350, 400), sprite, bubble, output),
            (250, 250)
        );
        assert_eq!(bubble_origin((350, 20), sprite, bubble, output), (250, 120));
        assert_eq!(bubble_origin((0, 400), sprite, bubble, output), (0, 250));
        assert_eq!(
            bubble_origin((700, 400), sprite, bubble, output),
            (500, 250)
        );
        assert_eq!(
            bubble_origin((20, 70), sprite, bubble, Some((200, 200))),
            (0, 0)
        );
        assert_eq!(
            bubble_origin((20, 0), sprite, bubble, Some((80, 80))),
            (0, 0)
        );
        assert_eq!(bubble_origin((350, 20), sprite, bubble, None), (250, 120));
    }

    #[test]
    fn emergency_cursor_has_visible_opaque_ink_and_transparent_padding() {
        let cursor = fallback_cursor();
        assert_eq!(cursor.len(), 16 * 24 * 4);
        assert_eq!(&cursor[..4], &[0, 0, 0, 255]);
        assert!(
            cursor
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel == &[255; 4])
        );
        assert_eq!(&cursor[cursor.len() - 4..], &[0; 4]);
    }

    #[test]
    fn reveal_subsets_remain_targeted_on_overflow_and_explicit_all_dominates() {
        let queue = Reveals::default();
        for n in 0..32 {
            queue.post(Some(vec![format!("id-{n}")]));
        }
        let ids = queue.take().unwrap().unwrap();
        assert_eq!(ids.len(), 32);
        assert!(ids.contains("id-0") && ids.contains("id-31"));
        queue.post(Some(vec!["first".into()]));
        queue.post(None);
        queue.post(Some(vec!["last".into()]));
        assert_eq!(queue.take(), Some(None));
        assert_eq!(queue.take(), None);
        for n in 0..513 {
            queue.post(Some(vec![format!("id-{n}")]));
        }
        let ids = queue.take().unwrap().unwrap();
        assert_eq!(ids.len(), 32);
        assert!(ids.contains("id-481") && ids.contains("id-512"));
        assert!(!ids.contains("id-480"));
    }

    #[test]
    fn oversized_reveal_requests_do_not_mutate_pending_requests() {
        let queue = Reveals::default();
        queue.post(Some(vec!["kept".into()]));
        queue.post(Some((0..17).map(|i| i.to_string()).collect()));
        assert_eq!(queue.take(), Some(Some(BTreeSet::from(["kept".into()]))));
        queue.post(Some(vec!["bad".into(); 17]));
        assert_eq!(queue.take(), None);
        for n in 0..33 {
            queue.post(Some((0..16).map(|i| format!("{n}-{i}")).collect()));
        }
        let ids = queue.take().unwrap().unwrap();
        assert_eq!(ids.len(), 512);
        assert!(!ids.contains("0-0") && ids.contains("32-15"));
    }

    #[test]
    fn close_retries_only_after_backoff_and_stops_on_queue_acceptance() {
        let now = Instant::now();
        let mut pending = CloseRequest::default();
        pending.request(now);
        pending.retry(now, || false);
        assert_eq!(pending.retry_at, Some(now + Duration::from_millis(100)));
        pending.request(now);
        pending.retry(now + Duration::from_millis(99), || {
            panic!("retried too soon")
        });
        pending.retry(now + Duration::from_millis(100), || true);
        assert_eq!(pending.retry_at, None);
        pending.request(now + Duration::from_secs(1));
        pending.retry(now + Duration::from_secs(1), || {
            panic!("resubmitted accepted close")
        });
    }

    #[test]
    fn premultiplied_argb_is_native_endian_and_opaque_keeps_artwork_alpha() {
        let rgba = [255, 128, 64, 128, 99, 88, 77, 0];
        let mut bytes = [0; 8];
        write_argb(&rgba, &mut bytes, false);
        assert_eq!(&bytes[..4], &0x8080_4020u32.to_ne_bytes());
        assert_eq!(&bytes[4..], &[0; 4]);
        write_argb(&rgba, &mut bytes, true);
        assert_eq!(&bytes[..4], &0xff80_4020u32.to_ne_bytes());
        assert_eq!(&bytes[4..], &0xff00_0000u32.to_ne_bytes());
    }

    #[test]
    fn alpha_projection_and_vertical_run_merging() {
        let mut rgba = vec![0; 8 * 6 * 4];
        for (x, y) in [(2, 0), (3, 3), (6, 4)] {
            rgba[(y * 8 + x) * 4 + 3] = 1;
        }
        assert_eq!(
            input_rectangles(&rgba, (8, 6), 2),
            vec![(1, 0, 1, 2), (3, 2, 1, 1)]
        );
        let before = input_rectangles(&rgba, (8, 6), 2);
        let mut diagnostic = vec![0; rgba.len()];
        write_argb(&rgba, &mut diagnostic, true);
        assert_eq!(input_rectangles(&rgba, (8, 6), 2), before);
        assert!(input_rectangles(&[0; 64], (4, 4), 1).is_empty());
    }

    #[test]
    fn input_rectangles_exactly_cover_logical_alpha_at_integer_scales() {
        for scale in 1..=3 {
            let physical = (3 * scale, 3 * scale);
            for mask in 0u32..512 {
                let mut rgba = vec![0; (physical.0 * physical.1 * 4) as usize];
                for cell in 0..9 {
                    if mask & (1 << cell) != 0 {
                        let x = cell % 3 * scale + scale - 1;
                        let y = cell / 3 * scale + scale - 1;
                        rgba[((y * physical.0 + x) * 4 + 3) as usize] = 1;
                    }
                }
                let rectangles = input_rectangles(&rgba, physical, scale);
                for cell in 0..9 {
                    let (x, y) = (cell % 3, cell / 3);
                    let coverage = rectangles
                        .iter()
                        .filter(|&&(rx, ry, w, h)| x >= rx && x < rx + w && y >= ry && y < ry + h)
                        .count();
                    assert_eq!(coverage, usize::from(mask & (1 << cell) != 0));
                }
            }
        }
        assert!(input_rectangles(&[], (0, i32::MAX as u32), 1).is_empty());
    }

    #[test]
    fn drag_uses_moving_origin_and_five_logical_pixel_threshold() {
        let mut gesture = Gesture::default();
        gesture.press(1, (10.0, 12.0), (100, 200));
        assert_eq!(gesture.moved(1, (14.0, 12.0), (100, 200)), None);
        assert_eq!(gesture.moved(1, (15.0, 12.0), (100, 200)), Some((105, 200)));
        assert_eq!(gesture.moved(1, (10.0, 12.0), (105, 200)), Some((105, 200)));
        assert_eq!(gesture.moved(1, (13.0, 14.0), (105, 200)), Some((108, 202)));
        assert!(!gesture.release(1));
        gesture.press(1, (10.0, 12.0), (100, 200));
        assert!(gesture.release(1));
    }

    #[test]
    fn other_seats_cannot_hijack_release_or_cancel_a_drag() {
        let mut gesture = Gesture::default();
        gesture.press(1, (10.0, 10.0), (100, 100));
        gesture.press(2, (500.0, 500.0), (0, 0));
        assert_eq!(gesture.moved(2, (600.0, 600.0), (0, 0)), None);
        assert!(!gesture.release(2));
        gesture.cancel(2);
        assert_eq!(gesture.moved(1, (16.0, 10.0), (100, 100)), Some((106, 100)));
        gesture.cancel(1);
        assert_eq!(gesture.moved(1, (20.0, 10.0), (100, 100)), None);
        assert!(!gesture.release(1));
    }

    #[test]
    fn invalid_coordinates_never_start_a_click_or_survive_motion() {
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut gesture = Gesture::default();
            gesture.press(1, (invalid, 1.0), (0, 0));
            assert!(!gesture.release(1));
            gesture.press(1, (1.0, 1.0), (0, 0));
            assert_eq!(gesture.moved(1, (1.0, invalid), (0, 0)), None);
            assert!(!gesture.release(1));
        }
    }

    #[test]
    fn geometry_clamps_to_outputs_and_handles_oversize_or_invalid_dimensions() {
        assert_eq!(
            clamp_origin((-1, 1000), Some((1920, 1080)), (144, 156)),
            (0, 924)
        );
        assert_eq!(clamp_origin((100, 100), Some((40, 40)), (144, 156)), (0, 0));
        assert_eq!(
            clamp_origin(
                (i32::MAX, i32::MAX),
                Some((i32::MIN, 0)),
                (u32::MAX, u32::MAX)
            ),
            (0, 0)
        );
        assert_eq!(
            initial_origin(Some((i32::MIN, i32::MIN)), (u32::MAX, u32::MAX), 0),
            (0, 0)
        );
        assert!(input_rectangles(&[0; 64], (4, 4), 0).is_empty());
        assert!(input_rectangles(&[0; 64], (4, 4), 3).is_empty());
        assert!(input_rectangles(&[0; 63], (4, 4), 1).is_empty());
    }

    #[test]
    fn bottom_right_placement_staggers_and_bounds_small_outputs() {
        assert_eq!(
            initial_origin(Some((1920, 1080)), (144, 156), 0),
            (1744, 892)
        );
        assert_eq!(
            initial_origin(Some((1920, 1080)), (144, 156), 1),
            (1708, 856)
        );
        assert_eq!(initial_origin(Some((40, 40)), (144, 156), 0), (0, 0));
    }
}
