//! CPU thought bubble with display-density text. Text is always producer metadata.

use crate::{
    activity::Presentation, animation::Animation, protocol::ActivityState, typography::Typeface,
};

const SPRITE_WIDTH: usize = 192;
const SPRITE_HEIGHT: usize = 208;
const WIDTH: usize = 384;
const BUBBLE_HEIGHT: usize = 144;
const HEIGHT: usize = BUBBLE_HEIGHT + SPRITE_HEIGHT;
const SPRITE_LEFT: usize = (WIDTH - SPRITE_WIDTH) / 2;
const IVORY: [u8; 4] = [255, 250, 235, 255];
const BORDER: [u8; 4] = [194, 177, 143, 255];
const INK: [u8; 4] = [49, 43, 37, 255];
const MUTED: [u8; 4] = [100, 85, 65, 255];

#[derive(Debug, Clone, PartialEq)]
pub struct BubbleText {
    pub identity: String,
    pub summary: String,
    pub status: String,
}

pub fn dimensions(show: bool) -> (u32, u32) {
    if show {
        (WIDTH as u32, HEIGHT as u32)
    } else {
        (192, 208)
    }
}

/// Full model strings for logging; clipping and whitespace cleanup happen only in paint.
pub fn text(view: &Presentation, preview: Option<Animation>) -> BubbleText {
    let Some(following) = &view.following else {
        return BubbleText {
            identity: "No conversation selected".into(),
            summary: if preview.is_some() {
                "Animation preview"
            } else {
                "Choose a conversation"
            }
            .into(),
            status: preview.map_or_else(
                || "Not following".into(),
                |animation| format!("Preview: {}", animation.label()),
            ),
        };
    };
    let live_status = if !following.available {
        "Unavailable"
    } else {
        match view.base.as_ref().map(|base| (base.state, base.attention)) {
            Some((ActivityState::Idle, _)) => "Idle",
            Some((ActivityState::Working, _)) => "Working",
            Some((ActivityState::Waiting, true)) => "Needs your input",
            Some((ActivityState::Waiting, false)) => "Waiting on dependency",
            Some((ActivityState::Succeeded, _)) => "Completed",
            Some((ActivityState::Failed, _)) => "Failed",
            Some((ActivityState::Cancelled, _)) => "Cancelled",
            None => "Unavailable",
        }
    };
    BubbleText {
        identity: if following.instance.is_empty() {
            format!("{} / {}", following.source, following.activity)
        } else {
            format!(
                "{} / {} / {}",
                following.source, following.activity, following.instance
            )
        },
        summary: following
            .summary
            .as_ref()
            .filter(|summary| {
                summary
                    .chars()
                    .any(|c| !c.is_whitespace() && !c.is_control())
            })
            .cloned()
            .unwrap_or_else(|| following.activity.clone()),
        status: if preview.is_some() {
            format!("Preview: {live_status}")
        } else {
            live_status.into()
        },
    }
}

pub struct Renderer {
    typeface: Option<Typeface>,
    font_size: f32,
}

impl Renderer {
    pub fn new(typeface: Option<Typeface>, font_size: f32) -> Self {
        Self {
            typeface,
            font_size: if font_size.is_finite() {
                font_size.clamp(8.0, 32.0)
            } else {
                16.0
            },
        }
    }

    /// `size` is the physical framebuffer size, not the logical window size.
    pub fn compose(
        &mut self,
        frame: &mut [u8],
        size: (u32, u32),
        sprite: &[u8],
        view: &Presentation,
        preview: Option<Animation>,
        show: bool,
    ) {
        assert_eq!(frame.len(), size.0 as usize * size.1 as usize * 4);
        assert_eq!(sprite.len(), SPRITE_WIDTH * SPRITE_HEIGHT * 4);
        frame.fill(0);
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        let canvas = Canvas::new(size, show);
        if show {
            for (rect, radius, color) in [
                ((4.0, 4.0, 376.0, 122.0), 13.0, BORDER),
                ((5.0, 5.0, 374.0, 120.0), 12.0, IVORY),
                ((171.0, 127.0, 11.0, 11.0), 5.0, BORDER),
                ((172.0, 128.0, 9.0, 9.0), 4.0, IVORY),
                ((184.0, 139.0, 5.0, 5.0), 2.0, BORDER),
                ((185.0, 140.0, 3.0, 3.0), 1.0, IVORY),
            ] {
                canvas.rounded(frame, rect, radius, color);
            }
            if let Some(face) = self.typeface.as_mut() {
                let content = text(view, preview);
                let px = self.font_size * canvas.scale;
                let metrics = face.line_metrics(px);
                let line_height = metrics.new_line_size.max(metrics.ascent - metrics.descent);
                let left = canvas.x + 16.0 * canvas.scale;
                let width = 352.0 * canvas.scale;
                let top = canvas.y + 12.0 * canvas.scale;
                let bottom = canvas.y + 120.0 * canvas.scale;
                let identity = clip(face, &clean(&content.identity), px, width);
                let status = clip(face, &clean(&content.status), px, width);
                let (identity_above, identity_below) = ink_extents(face, &identity, px);
                let (status_above, status_below) = ink_extents(face, &status, px);
                let status_top = bottom - status_above - status_below;
                let summary_top = top + identity_above + identity_below + 3.0 * canvas.scale;
                let lines = ((status_top - 3.0 * canvas.scale - summary_top) / line_height)
                    .floor()
                    .clamp(0.0, 3.0) as usize;
                canvas.paint_text(
                    frame,
                    face,
                    (left, top + identity_above),
                    &identity,
                    px,
                    MUTED,
                );
                for (index, line) in wrap(face, &content.summary, px, width, lines)
                    .iter()
                    .enumerate()
                {
                    canvas.paint_text(
                        frame,
                        face,
                        (
                            left,
                            summary_top + index as f32 * line_height + metrics.ascent,
                        ),
                        line,
                        px,
                        INK,
                    );
                }
                let accent = match content
                    .status
                    .strip_prefix("Preview: ")
                    .unwrap_or(&content.status)
                {
                    "Needs your input" | "Waiting on dependency" => [130, 83, 25, 255],
                    "Failed" => [151, 53, 48, 255],
                    "Working" | "Completed" => [51, 103, 83, 255],
                    _ => MUTED,
                };
                canvas.rounded(frame, (7.0, 108.0, 5.0, 5.0), 2.0, accent);
                canvas.paint_text(
                    frame,
                    face,
                    (left, bottom - status_below),
                    &status,
                    px,
                    accent,
                );
            }
        }
        // Only the artwork is nearest-resampled. Copy straight RGBA, including hidden RGB.
        let (left, top) = if show {
            (SPRITE_LEFT as f32, BUBBLE_HEIGHT as f32)
        } else {
            (0.0, 0.0)
        };
        let (x0, y0, x1, y1) = canvas.bounds((left, top, 192.0, 208.0));
        for y in y0..y1 {
            for x in x0..x1 {
                let sx = (((x as f32 + 0.5 - canvas.x) / canvas.scale - left).floor() as usize)
                    .min(SPRITE_WIDTH - 1);
                let sy = (((y as f32 + 0.5 - canvas.y) / canvas.scale - top).floor() as usize)
                    .min(SPRITE_HEIGHT - 1);
                let source = (sy * SPRITE_WIDTH + sx) * 4;
                let target = (y * size.0 as usize + x) * 4;
                frame[target..target + 4].copy_from_slice(&sprite[source..source + 4]);
            }
        }
    }
}

fn clean(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn ink_extents(face: &mut Typeface, value: &str, px: f32) -> (f32, f32) {
    let mut above = 0.0_f32;
    let mut below = 0.0_f32;
    for c in value.chars() {
        let m = &face.glyph(c, px).metrics;
        above = above.max(m.ymin as f32 + m.height as f32);
        below = below.max(-(m.ymin as f32));
    }
    (above, below)
}

/// UTF-8 byte boundary of the longest fitting prefix, with real advances and kerning.
fn prefix(face: &Typeface, value: &str, px: f32, width: f32) -> usize {
    let mut advance = 0.0;
    let mut previous = None;
    for (index, c) in value.char_indices() {
        let mut bytes = [0; 4];
        advance += previous.map_or(0.0, |p| face.kern(p, c, px))
            + face.measure(c.encode_utf8(&mut bytes), px);
        if advance > width {
            return index;
        }
        previous = Some(c);
    }
    value.len()
}

fn clip(face: &Typeface, value: &str, px: f32, width: f32) -> String {
    if prefix(face, value, px, width) == value.len() {
        return value.into();
    }
    let ellipsis = "\u{2026}";
    let suffix_width = face.measure(ellipsis, px);
    if suffix_width > width {
        return String::new();
    }
    let mut advance = 0.0;
    let mut previous = None;
    let mut end = 0;
    for (index, c) in value.char_indices() {
        let mut bytes = [0; 4];
        advance += previous.map_or(0.0, |p| face.kern(p, c, px))
            + face.measure(c.encode_utf8(&mut bytes), px);
        if advance + face.kern(c, '\u{2026}', px) + suffix_width > width {
            break;
        }
        if !c.is_whitespace() {
            end = index + c.len_utf8();
        }
        previous = Some(c);
    }
    format!("{}{}", &value[..end], ellipsis)
}

fn wrap(face: &Typeface, value: &str, px: f32, width: f32, limit: usize) -> Vec<String> {
    let cleaned = clean(value);
    let mut rest = cleaned.as_str();
    let mut lines = Vec::new();
    while !rest.is_empty() && lines.len() < limit.min(3) {
        if lines.len() + 1 == limit.min(3) {
            lines.push(clip(face, rest, px, width));
            break;
        }
        let fits = prefix(face, rest, px, width);
        if fits == rest.len() {
            lines.push(rest.into());
            break;
        }
        if fits == 0 {
            // Even a single glyph cannot fit; don't loop forever on tiny viewports.
            lines.push(clip(face, rest, px, width));
            break;
        }
        let end = if rest[fits..].starts_with(' ') {
            fits
        } else {
            rest[..fits]
                .rfind(' ')
                .filter(|end| *end > 0)
                .unwrap_or(fits)
        };
        lines.push(rest[..end].into());
        rest = rest[end..].trim_start();
    }
    lines
}

struct Canvas {
    size: (u32, u32),
    scale: f32,
    x: f32,
    y: f32,
}

impl Canvas {
    fn new(size: (u32, u32), show: bool) -> Self {
        let logical = dimensions(show);
        let scale = (size.0 as f32 / logical.0 as f32).min(size.1 as f32 / logical.1 as f32);
        Self {
            size,
            scale,
            x: (size.0 as f32 - logical.0 as f32 * scale) / 2.0,
            y: (size.1 as f32 - logical.1 as f32 * scale) / 2.0,
        }
    }

    fn bounds(&self, (x, y, w, h): (f32, f32, f32, f32)) -> (usize, usize, usize, usize) {
        // Pixel centers determine inclusion, including at fractional display scales.
        let edge = |v: f32, max: u32| (v - 0.5).ceil().clamp(0.0, max as f32) as usize;
        (
            edge(self.x + x * self.scale, self.size.0),
            edge(self.y + y * self.scale, self.size.1),
            edge(self.x + (x + w) * self.scale, self.size.0),
            edge(self.y + (y + h) * self.scale, self.size.1),
        )
    }

    fn rounded(&self, frame: &mut [u8], rect: (f32, f32, f32, f32), radius: f32, color: [u8; 4]) {
        let (left, top, width, height) = rect;
        let (x0, y0, x1, y1) = self.bounds(rect);
        for y in y0..y1 {
            for x in x0..x1 {
                let lx = (x as f32 + 0.5 - self.x) / self.scale;
                let ly = (y as f32 + 0.5 - self.y) / self.scale;
                let cx = lx.clamp(left + radius, left + width - radius);
                let cy = ly.clamp(top + radius, top + height - radius);
                if (lx - cx).powi(2) + (ly - cy).powi(2) <= radius * radius {
                    let offset = (y * self.size.0 as usize + x) * 4;
                    frame[offset..offset + 4].copy_from_slice(&color);
                }
            }
        }
    }

    fn paint_text(
        &self,
        frame: &mut [u8],
        face: &mut Typeface,
        (mut pen, baseline): (f32, f32),
        value: &str,
        px: f32,
        color: [u8; 4],
    ) {
        let (x0, y0, x1, y1) = self.bounds((16.0, 12.0, 352.0, 108.0));
        let mut previous = None;
        for c in value.chars() {
            pen += previous.map_or(0.0, |p| face.kern(p, c, px));
            let glyph = face.glyph(c, px);
            let m = &glyph.metrics;
            let left = pen.round() as i64 + i64::from(m.xmin);
            // fontdue coverage is top-to-bottom; ymin is relative to the baseline.
            let top = (baseline - (m.ymin as f32 + m.height as f32)).round() as i64;
            for row in 0..m.height {
                let y = top + row as i64;
                if y < y0 as i64 || y >= y1 as i64 {
                    continue;
                }
                for column in 0..m.width {
                    let x = left + column as i64;
                    if x < x0 as i64 || x >= x1 as i64 {
                        continue;
                    }
                    let alpha = u32::from(glyph.coverage[row * m.width + column])
                        * u32::from(color[3])
                        / 255;
                    if alpha == 0 {
                        continue;
                    }
                    let offset = (y as usize * self.size.0 as usize + x as usize) * 4;
                    let dest = &mut frame[offset..offset + 4];
                    let dest_alpha = u32::from(dest[3]);
                    let denominator = alpha * 255 + dest_alpha * (255 - alpha);
                    for channel in 0..3 {
                        dest[channel] = ((u32::from(color[channel]) * alpha * 255
                            + u32::from(dest[channel]) * dest_alpha * (255 - alpha)
                            + denominator / 2)
                            / denominator) as u8;
                    }
                    dest[3] = ((denominator + 127) / 255) as u8;
                }
            }
            pen += m.advance_width;
            previous = Some(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{Cue, Following, Selection};

    fn view(state: ActivityState, attention: bool) -> Presentation {
        Presentation {
            base: Some(Selection {
                source: "opencode".into(),
                instance: "session-1".into(),
                activity: "conversation-7".into(),
                state,
                attention,
                reason: None,
                summary: Some("Not the following summary".into()),
            }),
            following: Some(Following {
                source: "opencode".into(),
                instance: "session-1".into(),
                activity: "conversation-7".into(),
                summary: Some("Refining the thought bubble".into()),
                available: true,
            }),
            cue: None,
        }
    }

    #[test]
    #[ignore = "writes target/thought-bubble-preview.png for manual visual inspection"]
    fn write_visual_preview() {
        let atlas = crate::assets::SpriteAtlas::load().unwrap();
        let mut preview = view(ActivityState::Waiting, true);
        let following = preview.following.as_mut().unwrap();
        following.source = "djinn".into();
        following.instance = "local".into();
        following.activity = "chat-7".into();
        following.summary =
            Some("Investigating retry backoff and fixing stalled conversations".into());
        let (width, height) = dimensions(true);
        let mut frame = vec![0; width as usize * height as usize * 4];
        let mut renderer = Renderer::new(Some(Typeface::load(None, None).unwrap()), 16.0);
        renderer.compose(
            &mut frame,
            (width, height),
            atlas.frame(Animation::Waiting, 0),
            &preview,
            None,
            true,
        );
        image::save_buffer(
            "target/thought-bubble-preview.png",
            &frame,
            width,
            height,
            image::ColorType::Rgba8,
        )
        .unwrap();
        renderer.compose(
            &mut frame,
            (width, height),
            atlas.frame(Animation::Idle, 0),
            &Presentation::default(),
            None,
            true,
        );
        image::save_buffer(
            "target/thought-bubble-empty.png",
            &frame,
            width,
            height,
            image::ColorType::Rgba8,
        )
        .unwrap();
        let size = (width * 2, height * 2);
        let mut retina = vec![0; size.0 as usize * size.1 as usize * 4];
        renderer.compose(
            &mut retina,
            size,
            atlas.frame(Animation::Waiting, 0),
            &preview,
            None,
            true,
        );
        image::save_buffer(
            "target/thought-bubble-retina.png",
            &retina,
            size.0,
            size.1,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }

    fn render(view: &Presentation, sprite: &[u8]) -> Vec<u8> {
        let mut frame = vec![99; WIDTH * HEIGHT * 4];
        Renderer::new(Some(Typeface::bundled().unwrap()), 16.0).compose(
            &mut frame,
            dimensions(true),
            sprite,
            view,
            None,
            true,
        );
        frame
    }

    #[test]
    fn all_live_statuses_and_waiting_attention() {
        let face = Typeface::bundled().unwrap();
        for (state, attention, expected) in [
            (ActivityState::Idle, false, "Idle"),
            (ActivityState::Working, false, "Working"),
            (ActivityState::Waiting, false, "Waiting on dependency"),
            (ActivityState::Waiting, true, "Needs your input"),
            (ActivityState::Succeeded, false, "Completed"),
            (ActivityState::Failed, false, "Failed"),
            (ActivityState::Cancelled, false, "Cancelled"),
        ] {
            let view = view(state, attention);
            assert_eq!(text(&view, None).status, expected);
            let preview = text(&view, Some(Animation::RunningLeft));
            assert_eq!(preview.status, format!("Preview: {expected}"));
            assert!(face.measure(&preview.status, 16.0) <= 352.0);
        }
    }

    #[test]
    fn unavailable_preserves_identity_and_summary_over_stale_base() {
        let mut view = view(ActivityState::Working, false);
        let before = text(&view, None);
        view.following.as_mut().unwrap().available = false;
        let after = text(&view, None);
        assert_eq!(after.identity, before.identity);
        assert_eq!(after.summary, before.summary);
        assert_eq!(after.status, "Unavailable");
        view.base = None;
        assert_eq!(text(&view, None), after);
        assert_eq!(
            text(&view, Some(Animation::Working)).status,
            "Preview: Unavailable"
        );
    }

    #[test]
    fn missing_or_blank_summary_falls_back_to_conversation_id() {
        let mut view = view(ActivityState::Idle, false);
        for summary in [None, Some(" \n\t\0 ".into())] {
            view.following.as_mut().unwrap().summary = summary;
            assert_eq!(text(&view, None).summary, "conversation-7");
        }
        let full = "long instance ".repeat(30);
        view.following.as_mut().unwrap().instance = full.clone();
        assert!(text(&view, None).identity.ends_with(&full));
    }

    #[test]
    fn preview_without_focus_is_not_agent_activity() {
        let view = Presentation::default();
        assert_eq!(text(&view, None).identity, "No conversation selected");
        assert_eq!(text(&view, None).summary, "Choose a conversation");
        assert_eq!(text(&view, None).status, "Not following");
        for animation in Animation::ALL {
            let model = text(&view, Some(animation));
            assert_eq!(model.summary, "Animation preview");
            assert_eq!(model.status, format!("Preview: {}", animation.label()));
        }
    }

    #[test]
    fn transient_cue_cannot_change_live_text() {
        let mut view = view(ActivityState::Working, false);
        let before = text(&view, None);
        let mut selection = view.base.clone().unwrap();
        selection.state = ActivityState::Failed;
        view.cue = Some(Cue { id: 7, selection });
        assert_eq!(text(&view, None), before);
    }

    #[test]
    fn wrapping_handles_words_controls_utf8_and_truncation() {
        let face = Typeface::bundled().unwrap();
        assert!(wrap(&face, "", 16.0, 352.0, 3).is_empty());
        assert_eq!(
            wrap(&face, " hello\n\tworld\0test ", 16.0, 352.0, 3),
            ["hello world test"]
        );
        let words = "word ".repeat(40);
        let width = face.measure("word word word", 16.0) + 0.1;
        let lines = wrap(&face, &words, 16.0, width, 3);
        assert_eq!(lines.len(), 3);
        assert!(lines[2].ends_with('\u{2026}'));
        assert_eq!(lines[0], "word word word");
        for value in [
            "x".repeat(20_000),
            "\u{1f9de}".repeat(10_000),
            "\u{e9}\u{754c}\u{301} ".repeat(10_000),
        ] {
            let lines = wrap(&face, &value, 16.0, width, 3);
            assert_eq!(lines.len(), 3);
            assert!(
                lines
                    .iter()
                    .all(|line| face.measure(line, 16.0) <= width + 0.01)
            );
            assert!(lines[2].ends_with('\u{2026}'));
            assert_eq!(wrap(&face, &value, 16.0, width, 2).len(), 2);
            assert!(wrap(&face, &value, 16.0, width, 0).is_empty());
            assert!(wrap(&face, &value, 16.0, 0.0, 3).len() <= 3);
        }
        let exact_width = face.measure("xxxxxxxx", 16.0);
        assert_eq!(
            wrap(&face, &"x".repeat(24), 16.0, exact_width + 0.01, 3),
            vec!["x".repeat(8); 3]
        );
        assert!(clip(&face, "long string", 16.0, 0.0).is_empty());
        let short = clip(&face, &"\u{754c}".repeat(40), 16.0, width);
        assert!(short.ends_with('\u{2026}'));
        assert!(face.measure(&short, 16.0) <= width + 0.01);
        assert!(
            prefix(&face, "iiiiiiiiiiii", 16.0, width)
                >= prefix(&face, "WWWWWWWWWWWW", 16.0, width)
        );
    }

    #[test]
    fn density_rasterizes_real_coverage_and_uses_baseline_metrics() {
        assert_eq!((SPRITE_WIDTH, SPRITE_HEIGHT), (192, 208));
        let mut face = Typeface::bundled().unwrap();
        let mut frames = Vec::new();
        for density in [1, 2] {
            let canvas = Canvas::new((384 * density, 352 * density), true);
            let px = 16.0 * density as f32;
            let baseline = 40.0 * density as f32;
            let mut frame = IVORY.repeat(canvas.size.0 as usize * canvas.size.1 as usize);
            canvas.paint_text(
                &mut frame,
                &mut face,
                (32.0 * density as f32, baseline),
                "Ag",
                px,
                INK,
            );
            let glyph = face.glyph('A', px);
            let m = &glyph.metrics;
            let left = 32 * density as i64 + i64::from(m.xmin);
            let top = (baseline - (m.ymin as f32 + m.height as f32)).round() as i64;
            for row in 0..m.height {
                for col in 0..m.width {
                    let coverage = u32::from(glyph.coverage[row * m.width + col]);
                    let offset =
                        ((top as usize + row) * canvas.size.0 as usize + left as usize + col) * 4;
                    for channel in 0..3 {
                        let expected = (u32::from(INK[channel]) * coverage
                            + u32::from(IVORY[channel]) * (255 - coverage)
                            + 127)
                            / 255;
                        assert_eq!(u32::from(frame[offset + channel]), expected);
                    }
                    assert_eq!(frame[offset + 3], 255);
                }
            }
            assert!(
                frame
                    .chunks_exact(4)
                    .any(|p| p[0] > INK[0] && p[0] < IVORY[0])
            );
            frames.push(frame);
        }
        let nearest = nearest_double(&frames[0], WIDTH, HEIGHT);
        assert_ne!(
            frames[1], nearest,
            "2x text must not be an enlarged 1x bitmap"
        );
    }

    fn nearest_double(frame: &[u8], width: usize, height: usize) -> Vec<u8> {
        let mut result = vec![0; width * height * 16];
        for y in 0..height * 2 {
            for x in 0..width * 2 {
                let source = ((y / 2) * width + x / 2) * 4;
                let dest = (y * width * 2 + x) * 4;
                result[dest..dest + 4].copy_from_slice(&frame[source..source + 4]);
            }
        }
        result
    }

    #[test]
    fn text_is_clipped_to_content_and_blends_straight_alpha() {
        let canvas = Canvas::new(dimensions(true), true);
        let mut face = Typeface::bundled().unwrap();
        let background = [240, 220, 180, 100];
        let mut frame = background.repeat(WIDTH * HEIGHT);
        for baseline in [14.0, 118.0] {
            canvas.paint_text(
                &mut frame,
                &mut face,
                (8.0, baseline),
                &"Ag".repeat(100),
                32.0,
                [30, 50, 70, 128],
            );
        }
        let mut changed = false;
        for (index, pixel) in frame.chunks_exact(4).enumerate() {
            let (x, y) = (index % WIDTH, index / WIDTH);
            if !(16..368).contains(&x) || !(12..120).contains(&y) {
                assert_eq!(pixel, background);
            } else if pixel != background {
                changed = true;
                assert!((101..=178).contains(&pixel[3]));
                assert!(pixel[0] >= 30 && pixel[0] < 240);
            }
        }
        assert!(changed);
    }

    #[test]
    fn letterboxing_centers_shapes_and_sprite_with_blank_margins() {
        let sprite = vec![127; SPRITE_WIDTH * SPRITE_HEIGHT * 4];
        let mut renderer = Renderer::new(None, 16.0);
        for (size, show) in [((424, 352), true), ((384, 392), true), ((232, 208), false)] {
            let logical = dimensions(show);
            let left = (size.0 - logical.0) as usize / 2;
            let top = (size.1 - logical.1) as usize / 2;
            let mut padded = vec![99; size.0 as usize * size.1 as usize * 4];
            let mut normal = vec![0; logical.0 as usize * logical.1 as usize * 4];
            renderer.compose(
                &mut normal,
                logical,
                &sprite,
                &Presentation::default(),
                None,
                show,
            );
            renderer.compose(
                &mut padded,
                size,
                &sprite,
                &Presentation::default(),
                None,
                show,
            );
            for y in 0..size.1 as usize {
                for x in 0..size.0 as usize {
                    let offset = (y * size.0 as usize + x) * 4;
                    let pixel = &padded[offset..offset + 4];
                    if x >= left
                        && x < left + logical.0 as usize
                        && y >= top
                        && y < top + logical.1 as usize
                    {
                        let source = ((y - top) * logical.0 as usize + x - left) * 4;
                        assert_eq!(pixel, &normal[source..source + 4]);
                    } else {
                        assert_eq!(pixel, [0; 4]);
                    }
                }
            }
        }
    }

    #[test]
    fn font_size_limits_and_large_font_layout_reserve_status_space() {
        assert_eq!(Renderer::new(None, f32::NAN).font_size, 16.0);
        assert_eq!(Renderer::new(None, 1.0).font_size, 8.0);
        assert_eq!(Renderer::new(None, 99.0).font_size, 32.0);
        let mut face = Typeface::bundled().unwrap();
        for px in [8.0, 16.0, 32.0] {
            for status in ["Waiting on dependency", "Preview: Waiting on dependency"] {
                let fitted = clip(&face, status, px, 352.0);
                assert!(!fitted.is_empty());
                assert!(face.measure(&fitted, px) <= 352.01);
                let identity = clip(&face, "No conversation selected", px, 352.0);
                let (above, below) = ink_extents(&mut face, &identity, px);
                let (status_above, status_below) = ink_extents(&mut face, &fitted, px);
                let metrics = face.line_metrics(px);
                let line_height = metrics.new_line_size.max(metrics.ascent - metrics.descent);
                let available = 108.0 - above - below - status_above - status_below - 6.0;
                let count = (available / line_height).floor().clamp(0.0, 3.0) as usize;
                assert!(count >= 1);
                assert_eq!(
                    wrap(&face, &"summary ".repeat(100), px, 352.0, count).len(),
                    count
                );
                assert!(count as f32 * line_height <= available);
            }
        }
    }

    #[test]
    fn composition_preserves_sprite_bytes_and_transparent_background() {
        assert_eq!(dimensions(true), (384, 352));
        assert_eq!(dimensions(false), (192, 208));
        let sprite: Vec<_> = (0..SPRITE_WIDTH * SPRITE_HEIGHT * 4)
            .map(|i| (i % 251) as u8)
            .collect();
        let view = view(ActivityState::Working, false);
        let frame = render(&view, &sprite);
        assert_eq!(&frame[..4], &[0; 4]);
        assert_eq!(
            &frame[(4 * WIDTH + 4) * 4..(4 * WIDTH + 4) * 4 + 4],
            &[0; 4]
        );
        assert_eq!(
            &frame[(6 * WIDTH + 160) * 4..(6 * WIDTH + 160) * 4 + 4],
            &IVORY
        );
        assert_eq!(
            &frame[(4 * WIDTH + 160) * 4..(4 * WIDTH + 160) * 4 + 4],
            &BORDER
        );
        for y in 0..SPRITE_HEIGHT {
            let start = ((y + BUBBLE_HEIGHT) * WIDTH + SPRITE_LEFT) * 4;
            assert_eq!(
                &frame[start..start + 192 * 4],
                &sprite[y * 192 * 4..(y + 1) * 192 * 4]
            );
            assert!(
                frame[(y + BUBBLE_HEIGHT) * WIDTH * 4..start]
                    .iter()
                    .all(|b| *b == 0)
            );
            assert!(
                frame[start + 192 * 4..(y + BUBBLE_HEIGHT + 1) * WIDTH * 4]
                    .iter()
                    .all(|b| *b == 0)
            );
        }
        let mut legacy = vec![0; sprite.len()];
        let mut renderer = Renderer::new(None, 16.0);
        renderer.compose(
            &mut legacy,
            dimensions(false),
            &sprite,
            &view,
            Some(Animation::Failed),
            false,
        );
        assert_eq!(legacy, sprite);
        let mut doubled = vec![0; sprite.len() * 4];
        renderer.compose(&mut doubled, (384, 416), &sprite, &view, None, false);
        assert_eq!(
            doubled,
            nearest_double(&sprite, SPRITE_WIDTH, SPRITE_HEIGHT)
        );
        let size = (768, 704);
        let mut doubled = vec![0; size.0 as usize * size.1 as usize * 4];
        renderer.compose(&mut doubled, size, &sprite, &view, None, true);
        let expected = nearest_double(&sprite, SPRITE_WIDTH, SPRITE_HEIGHT);
        for y in 0..SPRITE_HEIGHT * 2 {
            let start = ((y + BUBBLE_HEIGHT * 2) * WIDTH * 2 + SPRITE_LEFT * 2) * 4;
            assert_eq!(
                &doubled[start..start + SPRITE_WIDTH * 8],
                &expected[y * SPRITE_WIDTH * 8..(y + 1) * SPRITE_WIDTH * 8]
            );
        }
    }

    #[test]
    fn summary_only_update_changes_render_without_animation_change() {
        let mut view = view(ActivityState::Working, false);
        let sprite = vec![0; SPRITE_WIDTH * SPRITE_HEIGHT * 4];
        let before = render(&view, &sprite);
        view.following.as_mut().unwrap().summary = Some("Checking the finished rendering".into());
        let after = render(&view, &sprite);
        assert_ne!(before, after);
        assert_eq!(
            &before[BUBBLE_HEIGHT * WIDTH * 4..],
            &after[BUBBLE_HEIGHT * WIDTH * 4..]
        );
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn rejects_wrong_frame_length() {
        Renderer::new(None, 16.0).compose(
            &mut [],
            dimensions(true),
            &vec![0; 192 * 208 * 4],
            &Presentation::default(),
            None,
            true,
        );
    }

    #[test]
    #[should_panic(expected = "assertion `left == right` failed")]
    fn rejects_wrong_sprite_length() {
        Renderer::new(None, 16.0).compose(
            &mut vec![0; 192 * 208 * 4],
            dimensions(false),
            &[],
            &Presentation::default(),
            None,
            false,
        );
    }
}
