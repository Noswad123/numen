//! CPU thought bubble with display-density text. Text is always producer metadata.

use crate::{
    activity::Presentation, animation::Animation, protocol::ActivityState, sprite::SpriteFrame,
    typography::Typeface,
};

const SPRITE_WIDTH: usize = 192;
const SPRITE_HEIGHT: usize = 208;
const WIDTH: usize = 384;
const BUBBLE_HEIGHT: usize = 144;
#[cfg(test)]
const HEIGHT: usize = BUBBLE_HEIGHT + SPRITE_HEIGHT;
#[cfg(test)]
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

fn bounded_sprite_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(0.25, 2.0)
    } else {
        0.5
    }
}

pub fn dimensions(show: bool, sprite_scale: f32) -> (u32, u32) {
    let sprite_scale = bounded_sprite_scale(sprite_scale);
    let width = (SPRITE_WIDTH as f32 * sprite_scale).ceil() as u32;
    let height = (SPRITE_HEIGHT as f32 * sprite_scale).ceil() as u32;
    if show {
        (WIDTH as u32, BUBBLE_HEIGHT as u32 + height)
    } else {
        (width, height)
    }
}

#[cfg(any(target_os = "linux", test))]
pub fn bubble_dimensions() -> (u32, u32) {
    (WIDTH as u32, BUBBLE_HEIGHT as u32)
}

/// Display strings; clipping and whitespace cleanup happen only in paint.
pub fn text(view: &Presentation, preview: Option<Animation>) -> BubbleText {
    let Some(following) = &view.following else {
        return BubbleText {
            identity: "Unattached familiar".into(),
            summary: if preview.is_some() {
                "Animation preview"
            } else {
                "Click for actions"
            }
            .into(),
            status: preview.map_or_else(
                || "Unattached".into(),
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
        identity: match following.source.as_str() {
            "djinn" => "Djinn".into(),
            source => source.into(),
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
            .unwrap_or_else(|| "Untitled session".into()),
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
    sprite_scale: f32,
}

/// The exact physical pixel grid painted by the compositor, excluding bubbles.
pub struct SpriteProjection {
    canvas: Canvas,
    sprite_scale: f32,
    left: f32,
    top: f32,
    pub bounds: (usize, usize, usize, usize),
}

impl SpriteProjection {
    pub fn new(size: (u32, u32), show: bool, sprite_scale: f32) -> Self {
        let sprite_scale = bounded_sprite_scale(sprite_scale);
        let canvas = Canvas::new(size, show, sprite_scale);
        let width = SPRITE_WIDTH as f32 * sprite_scale;
        let height = SPRITE_HEIGHT as f32 * sprite_scale;
        let (left, top) = if show {
            ((WIDTH as f32 - width) / 2.0, BUBBLE_HEIGHT as f32)
        } else {
            (0.0, 0.0)
        };
        let bounds = canvas.bounds((left, top, width, height));
        Self {
            canvas,
            sprite_scale,
            left,
            top,
            bounds,
        }
    }

    /// Coordinates must be inside `bounds`. Returns a source RGBA byte offset.
    pub fn source_offset_for(&self, source: SpriteFrame<'_>, x: usize, y: usize) -> usize {
        let (u, v) = self.source_position(x, y);
        source.source_offset(u, v)
    }

    fn source_position(&self, x: usize, y: usize) -> (f64, f64) {
        (
            ((x as f64 + 0.5 - f64::from(self.canvas.x)) / f64::from(self.canvas.scale)
                - f64::from(self.left))
                / (f64::from(self.sprite_scale) * SPRITE_WIDTH as f64),
            ((y as f64 + 0.5 - f64::from(self.canvas.y)) / f64::from(self.canvas.scale)
                - f64::from(self.top))
                / (f64::from(self.sprite_scale) * SPRITE_HEIGHT as f64),
        )
    }

    pub fn alpha<'a>(&self, sprite: impl Into<SpriteFrame<'a>>) -> Vec<u8> {
        let sprite = sprite.into();
        let (x0, y0, x1, y1) = self.bounds;
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| sprite[self.source_offset_for(sprite, x, y) + 3]))
            .collect()
    }
}

impl Renderer {
    pub fn new(typeface: Option<Typeface>, font_size: f32) -> Self {
        Self {
            typeface,
            font_size: if font_size.is_finite() {
                font_size.clamp(8.0, 32.0)
            } else {
                12.0
            },
            sprite_scale: 1.0,
        }
    }

    pub fn with_sprite_scale(mut self, scale: f32) -> Self {
        self.set_sprite_scale(scale);
        self
    }

    pub fn set_sprite_scale(&mut self, scale: f32) {
        self.sprite_scale = bounded_sprite_scale(scale);
    }

    pub fn set_font_size(&mut self, size: f32) {
        self.font_size = if size.is_finite() {
            size.clamp(8.0, 32.0)
        } else {
            12.0
        };
    }

    /// `size` is the physical framebuffer size, not the logical window size.
    pub fn compose<'a>(
        &mut self,
        frame: &mut [u8],
        size: (u32, u32),
        sprite: impl Into<SpriteFrame<'a>>,
        view: &Presentation,
        preview: Option<Animation>,
        show: bool,
    ) {
        assert_eq!(frame.len(), size.0 as usize * size.1 as usize * 4);
        let sprite = sprite.into();
        frame.fill(0);
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        let canvas = Canvas::new(size, show, self.sprite_scale);
        if show {
            self.paint_bubble(frame, &canvas, view, preview);
        }
        self.paint_sprite(frame, size, sprite, show);
    }

    #[cfg(test)]
    pub fn compose_sprite<'a>(
        &mut self,
        frame: &mut [u8],
        size: (u32, u32),
        sprite: impl Into<SpriteFrame<'a>>,
    ) {
        assert_eq!(frame.len(), size.0 as usize * size.1 as usize * 4);
        let sprite = sprite.into();
        frame.fill(0);
        if size.0 != 0 && size.1 != 0 {
            self.paint_sprite(frame, size, sprite, false);
        }
    }

    #[cfg(test)]
    pub fn compose_bubble(
        &mut self,
        frame: &mut [u8],
        size: (u32, u32),
        view: &Presentation,
        preview: Option<Animation>,
    ) {
        assert_eq!(frame.len(), size.0 as usize * size.1 as usize * 4);
        frame.fill(0);
        if size.0 != 0 && size.1 != 0 {
            self.paint_bubble(
                frame,
                &Canvas::for_dimensions(size, bubble_dimensions()),
                view,
                preview,
            );
        }
    }

    /// Reflow in scene units; only display density and scene scale affect glyph size.
    #[cfg(test)]
    pub fn compose_bubble_layout(
        &mut self,
        frame: &mut [u8],
        size: (u32, u32),
        layout: crate::bubble_layout::BubbleLayout,
        scene_scale: f64,
        content: (&Presentation, Option<Animation>),
    ) {
        self.compose_bubble_mapped(frame, size, layout, scene_scale, content, None);
    }

    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn compose_bubble_raster(
        &mut self,
        frame: &mut [u8],
        raster: crate::raster::RasterSpec,
        layout: crate::bubble_layout::BubbleLayout,
        scene_scale: f64,
        content: (&Presentation, Option<Animation>),
    ) {
        self.compose_bubble_mapped(
            frame,
            raster.physical_size(),
            layout,
            scene_scale,
            content,
            Some(raster),
        );
    }

    #[cfg(any(target_os = "linux", test))]
    fn compose_bubble_mapped(
        &mut self,
        frame: &mut [u8],
        size: (u32, u32),
        layout: crate::bubble_layout::BubbleLayout,
        scene_scale: f64,
        content: (&Presentation, Option<Animation>),
        raster: Option<crate::raster::RasterSpec>,
    ) {
        use crate::bubble_layout::TailEdge;
        assert_eq!(frame.len(), size.0 as usize * size.1 as usize * 4);
        frame.fill(0);
        if size.0 == 0
            || size.1 == 0
            || layout.size.0 == 0
            || layout.size.1 == 0
            || !scene_scale.is_finite()
            || scene_scale <= 0.0
        {
            return;
        }
        let scale = if raster.is_some() {
            1.0
        } else {
            let density = (size.0 / layout.size.0).min(size.1 / layout.size.1);
            scene_scale as f32 * density as f32
        };
        if !scale.is_finite() || scale <= 0.0 {
            return;
        }
        let (width, height) = (
            layout.size.0 as f32 / scene_scale as f32,
            layout.size.1 as f32 / scene_scale as f32,
        );
        if !width.is_finite() || !height.is_finite() || !(self.font_size * scale).is_finite() {
            return;
        }
        let mut canvas = Canvas {
            size,
            scale,
            x: 0.0,
            y: 0.0,
            content_clip: None,
            raster: raster.map(|r| (r, scene_scale)),
        };
        let tail = layout.tail.filter(|_| width >= 32.0 && height >= 32.0);
        let (mut left, mut top, mut right, mut bottom) = (4.0, 4.0, width - 4.0, height - 4.0);
        if let Some(tail) = tail {
            match tail.edge {
                TailEdge::Top => top = 18.0,
                TailEdge::Bottom => bottom = height - 18.0,
                TailEdge::Left => left = 18.0,
                TailEdge::Right => right = width - 18.0,
            }
        }
        if right <= left + 2.0 || bottom <= top + 2.0 {
            return;
        }
        let radius = 13.0_f32.min((right - left) / 2.0).min((bottom - top) / 2.0);
        canvas.rounded(
            frame,
            (left, top, right - left, bottom - top),
            radius,
            BORDER,
        );
        canvas.rounded(
            frame,
            (
                left + 1.0,
                top + 1.0,
                right - left - 2.0,
                bottom - top - 2.0,
            ),
            (radius - 1.0).max(0.0),
            IVORY,
        );
        if let Some(tail) = tail {
            let horizontal = matches!(tail.edge, TailEdge::Top | TailEdge::Bottom);
            let length = if horizontal { width } else { height };
            let position = (tail.position as f32 / scene_scale as f32).clamp(6.0, length - 6.0);
            for (depth, radius) in [(6.0, 5.5), (15.0, 2.5)] {
                let (x, y) = match tail.edge {
                    TailEdge::Top => (position, top - depth),
                    TailEdge::Bottom => (position, bottom + depth),
                    TailEdge::Left => (left - depth, position),
                    TailEdge::Right => (right + depth, position),
                };
                canvas.rounded(
                    frame,
                    (x - radius, y - radius, radius * 2.0, radius * 2.0),
                    radius,
                    BORDER,
                );
                let inner = radius - 1.0;
                canvas.rounded(
                    frame,
                    (x - inner, y - inner, inner * 2.0, inner * 2.0),
                    inner,
                    IVORY,
                );
            }
        }
        let rect = (
            left + 12.0,
            top + 8.0,
            right - left - 24.0,
            bottom - top - 16.0,
        );
        if rect.2 <= 0.0 || rect.3 <= 0.0 {
            return;
        }
        canvas.content_clip = Some(rect);
        let Some(face) = self.typeface.as_mut() else {
            return;
        };
        let content = text(content.0, content.1);
        let px = self.font_size * scale;
        let metrics = face.line_metrics(px);
        let line_height = metrics.new_line_size.max(metrics.ascent - metrics.descent);
        let width = rect.2 * scale;
        let left = rect.0 * scale;
        let mut top = rect.1 * scale;
        let mut bottom = (rect.1 + rect.3) * scale;
        let identity = clip(face, &clean(&content.identity), px, width);
        let status = clip(face, &clean(&content.status), px, width);
        let (above, below) = ink_extents(face, &identity, px);
        let (status_above, status_below) = ink_extents(face, &status, px);
        let gap = 3.0 * scale;
        let mut text_runs = Vec::new();
        // Status is essential; summary and identity use only the remaining space.
        if bottom - top >= status_above + status_below {
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
            canvas.rounded(
                frame,
                (
                    rect.0 - 9.0,
                    bottom / scale - status_above / scale - 1.0,
                    5.0,
                    5.0,
                ),
                2.0,
                accent,
            );
            text_runs.push(((left, bottom - status_below), status, accent));
            bottom -= status_above + status_below + gap;
        }
        if bottom - top >= above + below + gap + line_height {
            text_runs.push(((left, top + above), identity, MUTED));
            top += above + below + gap;
        }
        let lines = ((bottom - top) / line_height).floor().clamp(0.0, 3.0) as usize;
        for (index, line) in wrap(face, &content.summary, px, width, lines)
            .iter()
            .enumerate()
        {
            text_runs.push((
                (left, top + index as f32 * line_height + metrics.ascent),
                line.clone(),
                INK,
            ));
        }
        for (origin, value, color) in text_runs {
            canvas.paint_text(frame, face, origin, &value, px, color);
        }
    }

    fn paint_bubble(
        &mut self,
        frame: &mut [u8],
        canvas: &Canvas,
        view: &Presentation,
        preview: Option<Animation>,
    ) {
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

    fn paint_sprite(
        &self,
        frame: &mut [u8],
        size: (u32, u32),
        sprite: SpriteFrame<'_>,
        show: bool,
    ) {
        // Only the artwork is nearest-resampled. Copy straight RGBA, including hidden RGB.
        let projection = SpriteProjection::new(size, show, self.sprite_scale);
        let (x0, y0, x1, y1) = projection.bounds;
        for y in y0..y1 {
            for x in x0..x1 {
                let source = projection.source_offset_for(sprite, x, y);
                let target = (y * size.0 as usize + x) * 4;
                frame[target..target + 4].copy_from_slice(&sprite[source..source + 4]);
            }
        }
    }
}

/// Layout advances stay logical; only glyph coverage uses the effective density.
#[cfg(any(target_os = "linux", test))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_raster_text(
    frame: &mut [u8],
    face: &mut Typeface,
    raster: crate::raster::RasterSpec,
    scene_scale: f64,
    (mut pen, baseline): (f32, f32),
    value: &str,
    logical_px: f32,
    color: [u8; 4],
    clip: (f32, f32, f32, f32),
) {
    let density = raster.density().as_f64() * scene_scale;
    let px = (f64::from(logical_px) * density) as f32;
    let canvas = Canvas {
        size: raster.physical_size(),
        scale: 1.0,
        x: 0.0,
        y: 0.0,
        content_clip: None,
        raster: Some((raster, scene_scale)),
    };
    let (cx0, cy0, cx1, cy1) = canvas.bounds(clip);
    // Raster bounds may extend a fraction beyond logical ink metrics. Keep the
    // full vertical ink inside its clip without feeding hinting back into layout.
    let (above, below) = ink_extents(face, value, px);
    let minimum = f64::from(clip.1) + f64::from(above) / density;
    let maximum = f64::from(clip.1 + clip.3) - f64::from(below) / density;
    let baseline = if minimum <= maximum {
        f64::from(baseline).clamp(minimum, maximum)
    } else {
        f64::from(baseline)
    };
    let mut previous = None;
    for c in value.chars() {
        pen += previous.map_or(0.0, |p| face.kern(p, c, logical_px));
        let mut bytes = [0; 4];
        let advance = face.measure(c.encode_utf8(&mut bytes), logical_px);
        let glyph = face.glyph(c, px);
        let m = &glyph.metrics;
        let left = f64::from(pen) + f64::from(m.xmin) / density;
        let top = baseline - (m.ymin as f64 + m.height as f64) / density;
        let rect = (
            left as f32,
            top as f32,
            (m.width as f64 / density) as f32,
            (m.height as f64 / density) as f32,
        );
        let (x0, y0, x1, y1) = canvas.bounds(rect);
        for y in y0.max(cy0)..y1.min(cy1) {
            for x in x0.max(cx0)..x1.min(cx1) {
                let (lx, ly) = raster.buffer_to_logical((x as f64 + 0.5, y as f64 + 0.5));
                let column = (((lx / scene_scale - left) * density).floor() as usize)
                    .min(m.width.saturating_sub(1));
                let row = (((ly / scene_scale - top) * density).floor() as usize)
                    .min(m.height.saturating_sub(1));
                let alpha =
                    u32::from(glyph.coverage[row * m.width + column]) * u32::from(color[3]) / 255;
                if alpha == 0 {
                    continue;
                }
                let offset = (y * raster.physical_size().0 as usize + x) * 4;
                let dest = &mut frame[offset..offset + 4];
                let da = u32::from(dest[3]);
                let denominator = alpha * 255 + da * (255 - alpha);
                for channel in 0..3 {
                    dest[channel] = ((u32::from(color[channel]) * alpha * 255
                        + u32::from(dest[channel]) * da * (255 - alpha)
                        + denominator / 2)
                        / denominator) as u8;
                }
                dest[3] = ((denominator + 127) / 255) as u8;
            }
        }
        pen += advance;
        previous = Some(c);
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
    content_clip: Option<(f32, f32, f32, f32)>,
    #[cfg(any(target_os = "linux", test))]
    raster: Option<(crate::raster::RasterSpec, f64)>,
}

impl Canvas {
    fn new(size: (u32, u32), show: bool, sprite_scale: f32) -> Self {
        Self::for_dimensions(size, dimensions(show, sprite_scale))
    }

    fn for_dimensions(size: (u32, u32), logical: (u32, u32)) -> Self {
        let scale = (size.0 as f32 / logical.0 as f32).min(size.1 as f32 / logical.1 as f32);
        Self {
            size,
            scale,
            x: (size.0 as f32 - logical.0 as f32 * scale) / 2.0,
            y: (size.1 as f32 - logical.1 as f32 * scale) / 2.0,
            content_clip: None,
            #[cfg(any(target_os = "linux", test))]
            raster: None,
        }
    }

    fn bounds(&self, (x, y, w, h): (f32, f32, f32, f32)) -> (usize, usize, usize, usize) {
        // Pixel centers determine inclusion, including at fractional display scales.
        let edge = |v: f32, max: u32| (v - 0.5).ceil().clamp(0.0, max as f32) as usize;
        #[cfg(any(target_os = "linux", test))]
        if let Some((raster, scale)) = self.raster {
            let (left, top) =
                raster.logical_to_buffer((f64::from(x) * scale, f64::from(y) * scale));
            let (right, bottom) =
                raster.logical_to_buffer((f64::from(x + w) * scale, f64::from(y + h) * scale));
            let edge = |v: f64, max: u32| (v - 0.5).ceil().clamp(0.0, f64::from(max)) as usize;
            return (
                edge(left, self.size.0),
                edge(top, self.size.1),
                edge(right, self.size.0),
                edge(bottom, self.size.1),
            );
        }
        (
            edge(self.x + x * self.scale, self.size.0),
            edge(self.y + y * self.scale, self.size.1),
            edge(self.x + (x + w) * self.scale, self.size.0),
            edge(self.y + (y + h) * self.scale, self.size.1),
        )
    }

    fn rounded(&self, frame: &mut [u8], rect: (f32, f32, f32, f32), radius: f32, color: [u8; 4]) {
        let (left, top, width, height) = rect;
        if ![left, top, width, height, radius]
            .iter()
            .all(|v| v.is_finite())
            || width <= 0.0
            || height <= 0.0
        {
            return;
        }
        let radius = radius.max(0.0).min(width / 2.0).min(height / 2.0);
        // At a circular tail dot, the two centers are mathematically equal.
        // Different f32 addition order can invert them by an ULP as it moves.
        let min_x = left + radius;
        let max_x = (left + width - radius).max(min_x);
        let min_y = top + radius;
        let max_y = (top + height - radius).max(min_y);
        let (x0, y0, x1, y1) = self.bounds(rect);
        for y in y0..y1 {
            for x in x0..x1 {
                let (lx, ly) = (
                    (x as f32 + 0.5 - self.x) / self.scale,
                    (y as f32 + 0.5 - self.y) / self.scale,
                );
                #[cfg(any(target_os = "linux", test))]
                let (lx, ly) = if let Some((raster, scale)) = self.raster {
                    let (lx, ly) = raster.buffer_to_logical((x as f64 + 0.5, y as f64 + 0.5));
                    ((lx / scale) as f32, (ly / scale) as f32)
                } else {
                    (lx, ly)
                };
                let cx = lx.clamp(min_x, max_x);
                let cy = ly.clamp(min_y, max_y);
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
        #[cfg(any(target_os = "linux", test))]
        if let Some((raster, scale)) = self.raster {
            paint_raster_text(
                frame,
                face,
                raster,
                scale,
                (pen, baseline),
                value,
                px,
                color,
                self.content_clip.unwrap_or((16.0, 12.0, 352.0, 108.0)),
            );
            return;
        }
        let (x0, y0, x1, y1) = self.bounds(self.content_clip.unwrap_or((16.0, 12.0, 352.0, 108.0)));
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

    #[test]
    fn hd_facings_sample_rgb_and_contact_alpha_without_flipping_bubbles() {
        use crate::motion::Facing;
        let rgba: Vec<u8> = (0..416)
            .flat_map(|y| {
                (0..384).flat_map(move |x| {
                    [x as u8, y as u8, (x * 17 + y * 31) as u8, (x + y * 7) as u8]
                })
            })
            .collect();
        let source = SpriteFrame::new(&rgba, 384, 416);
        let face = Typeface::bundled().unwrap();
        for density in [1.0_f32, 1.25, 1.5, 1.75, 2.0] {
            for scale in [0.5, 0.713, 1.0] {
                for show in [false, true] {
                    let logical = dimensions(show, scale);
                    // Extra width exercises letterboxing as well as fractional density.
                    let size = (
                        (logical.0 as f32 * density).ceil() as u32 + 13,
                        (logical.1 as f32 * density).ceil() as u32,
                    );
                    let projection = SpriteProjection::new(size, show, scale);
                    let (x0, y0, x1, y1) = projection.bounds;
                    let mut painted = Vec::new();
                    for facing in [Facing::Left, Facing::Right] {
                        let sprite = source.with_facing(facing);
                        let mut frame = vec![99; size.0 as usize * size.1 as usize * 4];
                        Renderer::new(Some(face.fork()), 12.0)
                            .with_sprite_scale(scale)
                            .compose(
                                &mut frame,
                                size,
                                sprite,
                                &Presentation::default(),
                                None,
                                show,
                            );
                        let alpha = projection.alpha(sprite);
                        // Independent destination-to-source oracle; do not use projection sampling.
                        let canvas_scale = (size.0 as f32 / logical.0 as f32)
                            .min(size.1 as f32 / logical.1 as f32);
                        let ox = (size.0 as f32 - logical.0 as f32 * canvas_scale) / 2.0;
                        let oy = (size.1 as f32 - logical.1 as f32 * canvas_scale) / 2.0;
                        let left = if show {
                            (384.0 - 192.0 * scale) / 2.0
                        } else {
                            0.0
                        };
                        let top = if show { 144.0 } else { 0.0 };
                        for y in y0..y1 {
                            for x in x0..x1 {
                                let lx = (x as f64 + 0.5 - f64::from(ox)) / f64::from(canvas_scale);
                                let ly = (y as f64 + 0.5 - f64::from(oy)) / f64::from(canvas_scale);
                                let sx = (((lx - f64::from(left)) / (192.0 * f64::from(scale))
                                    * 384.0)
                                    .floor() as usize)
                                    .min(383);
                                let sx = if facing == Facing::Left { 383 - sx } else { sx };
                                let sy = (((ly - top) / (208.0 * f64::from(scale)) * 416.0).floor()
                                    as usize)
                                    .min(415);
                                let expected = (sy * 384 + sx) * 4;
                                let target = (y * size.0 as usize + x) * 4;
                                assert_eq!(
                                    &frame[target..target + 4],
                                    &rgba[expected..expected + 4]
                                );
                                assert_eq!(
                                    alpha[(y - y0) * (x1 - x0) + x - x0],
                                    rgba[expected + 3]
                                );
                                assert_eq!(projection.source_offset_for(sprite, x, y), expected);
                            }
                        }
                        painted.push(frame);
                    }
                    assert_ne!(painted[0], painted[1]);
                    if show {
                        let bubble_end = y0 * size.0 as usize * 4;
                        assert_eq!(painted[0][..bubble_end], painted[1][..bubble_end]);
                    }
                }
            }
        }
    }

    #[test]
    fn fractional_raster_axes_text_clips_and_logical_wraps() {
        use crate::raster::{Density, RasterMode, RasterSpec};
        let mut face = Typeface::bundled().unwrap();
        let logical = (301, 149);
        let layout = crate::bubble_layout::BubbleLayout {
            origin: (0, 0),
            size: logical,
            tail: None,
        };
        let mut presentation = view(ActivityState::Waiting, true);
        presentation.following.as_mut().unwrap().summary = Some("word ".repeat(90));
        let expected = wrap(&face, &text(&presentation, None).summary, 12.0, 269.0, 3);
        assert!(expected[2].ends_with('\u{2026}'));
        for units in [120, 150, 180, 210, 240] {
            let raster = RasterSpec::fit(
                logical,
                Density::from_preferred_scale(units).unwrap(),
                RasterMode::ViewportDestination,
            )
            .unwrap();
            let size = raster.physical_size();
            let mut actual = vec![99; size.0 as usize * size.1 as usize * 4];
            let mut blank = actual.clone();
            Renderer::new(Some(face.fork()), 12.0).compose_bubble_raster(
                &mut actual,
                raster,
                layout,
                1.0,
                (&presentation, None),
            );
            Renderer::new(None, 12.0).compose_bubble_raster(
                &mut blank,
                raster,
                layout,
                1.0,
                (&presentation, None),
            );
            let canvas = Canvas {
                size,
                scale: 1.0,
                x: 0.0,
                y: 0.0,
                content_clip: None,
                raster: Some((raster, 1.0)),
            };
            let (x0, y0, x1, y1) = canvas.bounds((16.0, 12.0, 269.0, 125.0));
            let mut ink = 0;
            for y in 0..size.1 as usize {
                for x in 0..size.0 as usize {
                    let offset = (y * size.0 as usize + x) * 4;
                    if actual[offset..offset + 4] != blank[offset..offset + 4]
                        && actual[offset..offset + 4] != MUTED
                    {
                        // The small status accent is outside the text clip.
                        if x >= x0 {
                            assert!(x < x1 && y >= y0 && y < y1);
                            ink += 1;
                        }
                    }
                }
            }
            assert!(ink > 0);
            assert_eq!(
                wrap(&face, &text(&presentation, None).summary, 12.0, 269.0, 3),
                expected
            );
            let mut transparent = vec![0; actual.len()];
            paint_raster_text(
                &mut transparent,
                &mut face,
                raster,
                1.0,
                (16.0, 35.0),
                "Fgj",
                12.0,
                INK,
                (16.0, 12.0, 269.0, 125.0),
            );
            assert!(transparent.chunks_exact(4).any(|p| p[3] > 0 && p[3] < 255));
            assert!(
                transparent
                    .chunks_exact(4)
                    .filter(|p| p[3] > 0)
                    .all(|p| p[..3] == INK[..3])
            );
        }
    }

    #[test]
    fn raster_minimum_bubbles_keep_full_status_ink_at_fractional_density() {
        use crate::{
            bubble_layout::{BubbleLayout, Tail, TailEdge},
            raster::{Density, RasterMode, RasterSpec},
        };
        let mut face = Typeface::bundled().unwrap();
        for font in [8.0_f32, 12.0, 24.0, 32.0] {
            let scene_scale = 1.375;
            let logical = (
                (f64::from((font * 6.0 + 32.0).clamp(160.0, 384.0)) * scene_scale).ceil() as u32,
                (f64::from((font * 2.5 + 24.0).clamp(80.0, 144.0)) * scene_scale).ceil() as u32,
            );
            for units in [120, 150, 180, 210, 240] {
                let raster = RasterSpec::fit(
                    logical,
                    Density::from_preferred_scale(units).unwrap(),
                    RasterMode::ViewportDestination,
                )
                .unwrap();
                for edge in [
                    TailEdge::Top,
                    TailEdge::Bottom,
                    TailEdge::Left,
                    TailEdge::Right,
                ] {
                    let layout = BubbleLayout {
                        origin: (0, 0),
                        size: logical,
                        tail: Some(Tail { edge, position: 40 }),
                    };
                    let width = logical.0 as f32 / scene_scale as f32;
                    let height = logical.1 as f32 / scene_scale as f32;
                    let left = if edge == TailEdge::Left { 30.0 } else { 16.0 };
                    let right = width - if edge == TailEdge::Right { 30.0 } else { 16.0 };
                    let top = if edge == TailEdge::Top { 26.0 } else { 12.0 };
                    let bottom = height - if edge == TailEdge::Bottom { 26.0 } else { 12.0 };
                    let rect = (left, top, right - left, bottom - top);
                    let canvas = Canvas {
                        size: raster.physical_size(),
                        scale: 1.0,
                        x: 0.0,
                        y: 0.0,
                        content_clip: Some(rect),
                        raster: Some((raster, scene_scale)),
                    };
                    for (state, attention, accent) in [
                        (ActivityState::Waiting, true, [130, 83, 25, 255]),
                        (ActivityState::Waiting, false, [130, 83, 25, 255]),
                        (ActivityState::Failed, false, [151, 53, 48, 255]),
                    ] {
                        let view = view(state, attention);
                        let status = clip(&face, &text(&view, None).status, font, right - left);
                        let (_, below) = ink_extents(&mut face, &status, font);
                        let size = raster.physical_size();
                        let mut expected = IVORY.repeat(size.0 as usize * size.1 as usize);
                        canvas.paint_text(
                            &mut expected,
                            &mut face,
                            (left, bottom - below),
                            &status,
                            font,
                            accent,
                        );
                        let mut actual = vec![0; expected.len()];
                        Renderer::new(Some(face.fork()), font).compose_bubble_raster(
                            &mut actual,
                            raster,
                            layout,
                            scene_scale,
                            (&view, None),
                        );
                        let mut ink = 0;
                        for (index, pixel) in expected.chunks_exact(4).enumerate() {
                            if pixel != IVORY {
                                assert_eq!(
                                    &actual[index * 4..index * 4 + 4],
                                    pixel,
                                    "font {font}, density {units}, edge {edge:?}, status {status}"
                                );
                                ink += 1;
                            }
                        }
                        assert!(ink > 0);
                    }
                }
            }
        }
    }

    #[test]
    fn moving_fractional_tail_dots_cannot_invert_rounded_clamp_bounds() {
        let canvas = Canvas::for_dimensions((192, 192), (128, 128));
        let mut frame = vec![0; 192 * 192 * 4];
        let left = 53.833_332_f32;
        assert!(
            left + 5.5 > left + 11.0 - 5.5,
            "retain the ULP regression case"
        );
        for offset in 0..192 {
            let position = offset as f32 / 1.5;
            canvas.rounded(&mut frame, (position, left, 11.0, 11.0), 5.5, IVORY);
            canvas.rounded(&mut frame, (left, position, 5.0, 5.0), 2.5, BORDER);
        }
        assert!(frame.as_chunks::<4>().0.iter().any(|rgba| rgba[3] != 0));
    }

    fn responsive_frame(
        renderer: &mut Renderer,
        size: (u32, u32),
        scale: f64,
        tail: Option<crate::bubble_layout::Tail>,
        view: &Presentation,
    ) -> Vec<u8> {
        let mut frame = vec![99; size.0 as usize * size.1 as usize * 4];
        renderer.compose_bubble_layout(
            &mut frame,
            size,
            crate::bubble_layout::BubbleLayout {
                origin: (0, 0),
                size,
                tail,
            },
            scale,
            (view, None),
        );
        frame
    }

    #[test]
    fn responsive_reflow_keeps_glyph_density_and_line_metrics() {
        let face = Typeface::bundled().unwrap();
        let mut renderer = Renderer::new(Some(face.fork()), 16.0);
        let mut view = view(ActivityState::Working, false);
        view.following.as_mut().unwrap().summary = Some("word ".repeat(80));
        for (scale, density) in [(1.0, 1), (1.375, 2), (2.0, 3)] {
            let px = 16.0 * scale as f32 * density as f32;
            let wide = wrap(
                &face,
                &text(&view, None).summary,
                px,
                352.0 * scale as f32 * density as f32,
                3,
            );
            let narrow = wrap(
                &face,
                &text(&view, None).summary,
                px,
                168.0 * scale as f32 * density as f32,
                3,
            );
            assert!(wide[0].len() > narrow[0].len());
            assert!(narrow[2].ends_with('\u{2026}'));
            let mut frames = Vec::new();
            for width in [384, 200] {
                let logical = (
                    (f64::from(width) * scale).ceil() as u32,
                    (144.0 * scale).ceil() as u32,
                );
                let size = (logical.0 * density, logical.1 * density);
                let mut frame = vec![0; size.0 as usize * size.1 as usize * 4];
                renderer.compose_bubble_layout(
                    &mut frame,
                    size,
                    crate::bubble_layout::BubbleLayout {
                        origin: (9, -20),
                        size: logical,
                        tail: None,
                    },
                    scale,
                    (&view, None),
                );
                // The title and first summary word use the same raster and baselines.
                let unit = scale as f32 * density as f32;
                let crop: Vec<_> = ((12.0 * unit).ceil() as usize..(55.0 * unit).floor() as usize)
                    .flat_map(|y| {
                        let frame = &frame;
                        ((16.0 * unit).ceil() as usize..(48.0 * unit).floor() as usize).flat_map(
                            move |x| frame[(y * size.0 as usize + x) * 4..][..4].iter().copied(),
                        )
                    })
                    .collect();
                frames.push(crop);
            }
            assert_eq!(frames[0], frames[1], "reflow must not shrink glyphs");
        }
    }

    #[test]
    fn responsive_tails_repaint_each_edge_without_flipping_text() {
        use crate::bubble_layout::{Tail, TailEdge};
        let view = view(ActivityState::Working, false);
        let mut renderer = Renderer::new(Some(Typeface::bundled().unwrap()), 12.0);
        let mut background = Renderer::new(None, 12.0);
        let size = (384, 144);
        for edges in [
            [TailEdge::Top, TailEdge::Bottom],
            [TailEdge::Left, TailEdge::Right],
        ] {
            let mut glyphs = Vec::new();
            for edge in edges {
                let horizontal = matches!(edge, TailEdge::Top | TailEdge::Bottom);
                let position = if horizontal { 190 } else { 70 };
                let tail = Some(Tail { edge, position });
                let frame = responsive_frame(&mut renderer, size, 1.0, tail, &view);
                let blank = responsive_frame(&mut background, size, 1.0, tail, &view);
                let (dot_x, dot_y) = match edge {
                    TailEdge::Top => (190, 3),
                    TailEdge::Bottom => (190, 141),
                    TailEdge::Left => (3, 70),
                    TailEdge::Right => (381, 70),
                };
                assert_eq!(&frame[(dot_y * 384 + dot_x) * 4..][..4], IVORY);
                assert_eq!(&frame[..4], &[0; 4]);
                let (left, top) = (
                    if edge == TailEdge::Left { 30 } else { 16 },
                    if edge == TailEdge::Top { 26 } else { 12 },
                );
                let ink: Vec<_> = (top..top + 70)
                    .flat_map(|y| {
                        let frame = &frame;
                        let blank = &blank;
                        (left..left + 310).filter_map(move |x| {
                            let offset = (y * 384 + x) * 4;
                            if frame[offset..offset + 4] != blank[offset..offset + 4] {
                                Some((x - left, y - top, frame[offset..offset + 4].to_vec()))
                            } else {
                                None
                            }
                        })
                    })
                    .collect();
                assert!(!ink.is_empty());
                glyphs.push(ink);
                let shifted = responsive_frame(
                    &mut renderer,
                    size,
                    1.0,
                    Some(Tail {
                        edge,
                        position: position + 20,
                    }),
                    &view,
                );
                assert_ne!(frame, shifted);
                assert_eq!(
                    frame,
                    responsive_frame(&mut renderer, size, 1.0, tail, &view)
                );
                let clamped = responsive_frame(
                    &mut renderer,
                    size,
                    1.0,
                    Some(Tail {
                        edge,
                        position: u32::MAX,
                    }),
                    &view,
                );
                assert_eq!(
                    clamped,
                    responsive_frame(
                        &mut renderer,
                        size,
                        1.0,
                        Some(Tail {
                            edge,
                            position: if horizontal { size.0 } else { size.1 }
                        }),
                        &view
                    )
                );
            }
            assert_eq!(glyphs[0], glyphs[1], "tail orientation cannot flip text");
        }
    }

    #[test]
    fn responsive_minimum_sizes_preserve_status_raster_at_requested_density() {
        use crate::bubble_layout::{BubbleLayout, Tail, TailEdge};
        let mut face = Typeface::bundled().unwrap();
        for font in [8.0_f32, 12.0, 24.0, 32.0] {
            for (scene_scale, density) in [(1.0, 1), (1.375, 2)] {
                let unit = scene_scale as f32 * density as f32;
                let logical = (
                    (f64::from((font * 6.0 + 32.0).clamp(160.0, 384.0)) * scene_scale).ceil()
                        as u32,
                    (f64::from((font * 2.5 + 24.0).clamp(80.0, 144.0)) * scene_scale).ceil() as u32,
                );
                let size = (logical.0 * density, logical.1 * density);
                for edge in [
                    TailEdge::Top,
                    TailEdge::Bottom,
                    TailEdge::Left,
                    TailEdge::Right,
                ] {
                    let layout = BubbleLayout {
                        origin: (0, 0),
                        size: logical,
                        tail: Some(Tail { edge, position: 40 }),
                    };
                    let (width, height) = (
                        logical.0 as f32 / scene_scale as f32,
                        logical.1 as f32 / scene_scale as f32,
                    );
                    let left = if edge == TailEdge::Left { 30.0 } else { 16.0 };
                    let right = width - if edge == TailEdge::Right { 30.0 } else { 16.0 };
                    let top = if edge == TailEdge::Top { 26.0 } else { 12.0 };
                    let bottom = height - if edge == TailEdge::Bottom { 26.0 } else { 12.0 };
                    let canvas = Canvas {
                        size,
                        scale: unit,
                        x: 0.0,
                        y: 0.0,
                        content_clip: Some((left, top, right - left, bottom - top)),
                        raster: None,
                    };
                    let mut frames = Vec::new();
                    for (state, attention, accent) in [
                        (ActivityState::Waiting, true, [130, 83, 25, 255]),
                        (ActivityState::Waiting, false, [130, 83, 25, 255]),
                        (ActivityState::Failed, false, [151, 53, 48, 255]),
                    ] {
                        let view = view(state, attention);
                        let px = font * unit;
                        let status =
                            clip(&face, &text(&view, None).status, px, (right - left) * unit);
                        let (above, below) = ink_extents(&mut face, &status, px);
                        assert!(above + below <= (bottom - top) * unit);
                        let mut expected = IVORY.repeat(size.0 as usize * size.1 as usize);
                        canvas.paint_text(
                            &mut expected,
                            &mut face,
                            (left * unit, bottom * unit - below),
                            &status,
                            px,
                            accent,
                        );
                        let mut actual = vec![0; expected.len()];
                        Renderer::new(Some(face.fork()), font).compose_bubble_layout(
                            &mut actual,
                            size,
                            layout,
                            scene_scale,
                            (&view, None),
                        );
                        let (x0, _, x1, y1) =
                            canvas.bounds((left, top, right - left, bottom - top));
                        let y0 = (bottom * unit - above - below).round() as usize;
                        let mut ink_pixels = 0;
                        for y in y0..y1 {
                            for x in x0..x1 {
                                let offset = (y * size.0 as usize + x) * 4;
                                assert_eq!(
                                    &actual[offset..offset + 4],
                                    &expected[offset..offset + 4],
                                    "status ink clipped, omitted, or rescaled: font {font}, edge {edge:?}, status {status}"
                                );
                                ink_pixels += usize::from(expected[offset..offset + 4] != IVORY);
                            }
                        }
                        assert!(
                            ink_pixels > 0,
                            "must compare status glyphs, not just the accent"
                        );
                        frames.push(actual);
                    }
                    assert_ne!(
                        frames[0], frames[1],
                        "attention changes must remain visible"
                    );
                    assert_ne!(frames[0], frames[2], "failure must remain visible");
                }
            }
        }
    }

    #[test]
    fn responsive_accented_identity_keeps_full_ink_inside_content() {
        let mut face = Typeface::bundled().unwrap();
        let mut view = view(ActivityState::Waiting, true);
        view.following.as_mut().unwrap().source = "\u{c9}gj\u{e9}".into();
        for font in [8.0, 12.0, 24.0, 32.0] {
            let size = (384, 200);
            let actual = responsive_frame(
                &mut Renderer::new(Some(face.fork()), font),
                size,
                1.0,
                None,
                &view,
            );
            let identity = text(&view, None).identity;
            let (above, below) = ink_extents(&mut face, &identity, font);
            let canvas = Canvas {
                size,
                scale: 1.0,
                x: 0.0,
                y: 0.0,
                content_clip: Some((16.0, 12.0, 352.0, 176.0)),
                raster: None,
            };
            let mut expected = IVORY.repeat(size.0 as usize * size.1 as usize);
            canvas.paint_text(
                &mut expected,
                &mut face,
                (16.0, 12.0 + above),
                &identity,
                font,
                MUTED,
            );
            let mut ink_pixels = 0;
            for y in 12..(12.0 + above + below).ceil() as usize {
                for x in 16..368 {
                    let offset = (y * 384 + x) * 4;
                    assert_eq!(&actual[offset..offset + 4], &expected[offset..offset + 4]);
                    ink_pixels += usize::from(expected[offset..offset + 4] != IVORY);
                }
            }
            assert!(above > 0.0 && below > 0.0 && ink_pixels > 0);
        }
    }

    #[test]
    fn responsive_tiny_and_invalid_geometry_stays_safe_and_transparent() {
        use crate::bubble_layout::{Tail, TailEdge};
        let mut renderer = Renderer::new(Some(Typeface::bundled().unwrap()), 32.0);
        for size in [(0, 0), (0, 144), (1, 1), (8, 8), (16, 31), (32, 32)] {
            for edge in [
                TailEdge::Top,
                TailEdge::Bottom,
                TailEdge::Left,
                TailEdge::Right,
            ] {
                for scale in [
                    0.0,
                    -1.0,
                    f64::NAN,
                    f64::INFINITY,
                    f64::MIN_POSITIVE,
                    f64::MAX,
                    1.0,
                ] {
                    let frame = responsive_frame(
                        &mut renderer,
                        size,
                        scale,
                        Some(Tail {
                            edge,
                            position: u32::MAX,
                        }),
                        &Presentation::default(),
                    );
                    assert_eq!(frame.len(), size.0 as usize * size.1 as usize * 4);
                    assert!(
                        frame
                            .chunks_exact(4)
                            .filter(|p| p[3] == 0)
                            .all(|p| p == [0; 4])
                    );
                }
            }
        }
        let mut frame = vec![99; 16 * 16 * 4];
        renderer.compose_bubble_layout(
            &mut frame,
            (16, 16),
            crate::bubble_layout::BubbleLayout {
                origin: (0, 0),
                size: (0, 144),
                tail: None,
            },
            1.0,
            (&Presentation::default(), None),
        );
        assert!(frame.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn split_composition_matches_combined_at_equal_density() {
        let face = Typeface::bundled().unwrap();
        let sprite: Vec<_> = (0..SPRITE_WIDTH * SPRITE_HEIGHT * 4)
            .map(|i| (i % 251) as u8)
            .collect();
        let view = view(ActivityState::Working, false);
        for scale in [0.25, 0.5, 1.0, 2.0] {
            for density in [1, 2, 3] {
                let mut renderer = Renderer::new(Some(face.fork()), 12.0).with_sprite_scale(scale);
                let logical = dimensions(true, scale);
                let size = (logical.0 * density, logical.1 * density);
                let mut combined = vec![99; size.0 as usize * size.1 as usize * 4];
                renderer.compose(
                    &mut combined,
                    size,
                    &sprite,
                    &view,
                    Some(Animation::Review),
                    true,
                );
                let bubble_size = (384 * density, 144 * density);
                let mut bubble = vec![99; bubble_size.0 as usize * bubble_size.1 as usize * 4];
                renderer.compose_bubble(&mut bubble, bubble_size, &view, Some(Animation::Review));
                assert_eq!(bubble, combined[..bubble.len()]);
                let logical_sprite = dimensions(false, scale);
                let sprite_size = (logical_sprite.0 * density, logical_sprite.1 * density);
                let mut split = vec![99; sprite_size.0 as usize * sprite_size.1 as usize * 4];
                let mut hidden = split.clone();
                renderer.compose_sprite(&mut split, sprite_size, &sprite);
                renderer.compose(&mut hidden, sprite_size, &sprite, &view, None, false);
                assert_eq!(split, hidden);
                let left = (size.0 - sprite_size.0) / 2;
                for y in 0..sprite_size.1 {
                    let source = (y * sprite_size.0) as usize * 4;
                    let target = ((y + bubble_size.1) * size.0 + left) as usize * 4;
                    let width = sprite_size.0 as usize * 4;
                    assert_eq!(
                        split[source..source + width],
                        combined[target..target + width]
                    );
                }
            }
        }
    }

    #[test]
    fn split_canvases_keep_alpha_and_sprite_input_independent() {
        let mut renderer = Renderer::new(None, 12.0).with_sprite_scale(0.5);
        let sprite = [80, 60, 40, 0].repeat(SPRITE_WIDTH * SPRITE_HEIGHT);
        let size = (197, 208);
        let mut frame = vec![99; size.0 as usize * size.1 as usize * 4];
        renderer.compose_sprite(&mut frame, size, &sprite);
        assert!(frame.chunks_exact(4).all(|p| p[3] == 0));
        let projection = SpriteProjection::new(size, false, 0.5);
        assert!(projection.alpha(&sprite).iter().all(|&a| a == 0));
        let mut bubble = vec![99; WIDTH * BUBBLE_HEIGHT * 4];
        renderer.compose_bubble(
            &mut bubble,
            bubble_dimensions(),
            &Presentation::default(),
            None,
        );
        assert_eq!(&bubble[..4], &[0; 4]);
        assert!(bubble.chunks_exact(4).any(|p| p[3] == 255));
        renderer.set_sprite_scale(2.0);
        let mut other = bubble.clone();
        renderer.compose_bubble(
            &mut other,
            bubble_dimensions(),
            &Presentation::default(),
            None,
        );
        assert_eq!(bubble, other, "sprite scaling cannot change bubble artwork");
        assert!(projection.alpha(&sprite).iter().all(|&a| a == 0));
        renderer.compose_bubble(&mut [], (0, 144), &Presentation::default(), None);
        renderer.compose_sprite(&mut [], (192, 0), &sprite);
    }

    #[test]
    fn mutable_settings_repaint_at_actual_font_and_sprite_sizes() {
        let face = Typeface::bundled().unwrap();
        let mut renderer = Renderer::new(Some(face.fork()), 12.0).with_sprite_scale(0.5);
        let sprite = [100, 80, 60, 255].repeat(SPRITE_WIDTH * SPRITE_HEIGHT);
        let view = Presentation::default();
        for density in [1.0, 1.375, 2.0] {
            for (font, scale) in [(8.0, 0.25), (32.0, 2.0), (19.5, 0.713), (12.0, 0.5)] {
                renderer.set_font_size(font);
                renderer.set_sprite_scale(scale);
                for show in [false, true] {
                    let logical = dimensions(show, scale);
                    let size = (
                        (logical.0 as f32 * density).round() as u32,
                        (logical.1 as f32 * density).round() as u32,
                    );
                    let mut actual = vec![0; size.0 as usize * size.1 as usize * 4];
                    let mut expected = actual.clone();
                    renderer.compose(&mut actual, size, &sprite, &view, None, show);
                    Renderer::new(Some(face.fork()), font)
                        .with_sprite_scale(scale)
                        .compose(&mut expected, size, &sprite, &view, None, show);
                    assert_eq!(actual, expected);
                    let (x0, y0, x1, y1) = SpriteProjection::new(size, show, scale).bounds;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let offset = (y * size.0 as usize + x) * 4;
                            assert_eq!(&actual[offset..offset + 4], &[100, 80, 60, 255]);
                        }
                    }
                    if show {
                        renderer.set_font_size(if font == 32.0 { 8.0 } else { 32.0 });
                        renderer.compose(&mut expected, size, &sprite, &view, None, show);
                        assert_ne!(actual, expected, "font size must change the painted text");
                        renderer.set_font_size(font);
                    }
                }
            }
        }
    }

    #[test]
    fn disjoint_pose_frames_use_only_the_current_compositor_alpha() {
        use crate::motion::{Point, RasterMask, SpriteGeometry};
        let geometry = SpriteGeometry {
            origin: Point::default(),
            pixel_scale: 1.0,
        };
        let projection = SpriteProjection::new((192, 208), false, 1.0);
        let points = [(10, 10), (150, 10), (150, 150)];
        for &(x, y) in &points {
            let mut sprite = vec![0; SPRITE_WIDTH * SPRITE_HEIGHT * 4];
            sprite[(y * SPRITE_WIDTH + x) * 4 + 3] = 255;
            let mask = RasterMask::new(192, 208, projection.alpha(&sprite));
            let mut painted = vec![0; sprite.len()];
            Renderer::new(None, 12.0).compose(
                &mut painted,
                (192, 208),
                &sprite,
                &Presentation::default(),
                None,
                false,
            );
            for &(px, py) in &points {
                let contact = mask.contact(
                    Point {
                        x: px as f64 + 0.5,
                        y: py as f64 + 0.5,
                    },
                    geometry,
                );
                assert_eq!(contact, (px, py) == (x, y));
                assert_eq!(contact, painted[(py * SPRITE_WIDTH + px) * 4 + 3] >= 32);
            }
        }
    }

    #[test]
    fn pursuit_contact_matches_composed_sprite_raster_on_fractional_grids() {
        use crate::motion::{Navigator, Point, PursuitStep, RasterMask, SpriteGeometry};
        use std::time::Instant;
        let now = Instant::now();
        for sprite_scale in [0.25, 0.5] {
            for backing in [1.0_f32, 2.0] {
                for scene in [1.0_f32, 1.375] {
                    for show in [false, true] {
                        let logical = dimensions(show, sprite_scale);
                        let size = (
                            (logical.0 as f32 * scene * backing).round() as u32 + 3,
                            (logical.1 as f32 * scene * backing).round() as u32,
                        );
                        let projection = SpriteProjection::new(size, show, sprite_scale);
                        let (x0, y0, x1, y1) = projection.bounds;
                        // Alternating threshold-adjacent alpha exposes discarded
                        // source pixels and pixel-center/rounding discrepancies.
                        let mut sprite = vec![0; SPRITE_WIDTH * SPRITE_HEIGHT * 4];
                        for (index, rgba) in sprite.chunks_exact_mut(4).enumerate() {
                            rgba.copy_from_slice(&[
                                110,
                                80,
                                40,
                                [0, 31, 32, 255][(index + index / SPRITE_WIDTH) % 4],
                            ]);
                        }
                        let mask = RasterMask::new(x1 - x0, y1 - y0, projection.alpha(&sprite));
                        let mut rendered = vec![0; size.0 as usize * size.1 as usize * 4];
                        Renderer::new(None, 12.0)
                            .with_sprite_scale(sprite_scale)
                            .compose(
                                &mut rendered,
                                size,
                                &sprite,
                                &Presentation::default(),
                                None,
                                show,
                            );
                        let backing = f64::from(backing);
                        let world = Point {
                            x: -200.0,
                            y: -100.0,
                        };
                        let geometry = SpriteGeometry {
                            origin: Point {
                                x: world.x + x0 as f64 / backing,
                                y: world.y + y0 as f64 / backing,
                            },
                            pixel_scale: 1.0 / backing,
                        };
                        for y in y0..y1 {
                            for x in x0..x1 {
                                let cursor = Point {
                                    x: world.x + (x as f64 + 0.5) / backing,
                                    y: world.y + (y as f64 + 0.5) / backing,
                                };
                                let contact = Navigator::new(40.0, now)
                                    .step_raster(now, cursor, geometry, &mask);
                                let alpha = rendered[(y * size.0 as usize + x) * 4 + 3];
                                assert_eq!(
                                    contact == PursuitStep::Arrived,
                                    alpha >= 32,
                                    "pixel ({x}, {y}), scene {scene}, backing {backing}, sprite {sprite_scale}, bubble {show}"
                                );
                            }
                        }
                        if show {
                            let cursor = Point {
                                x: world.x + 20.0 * f64::from(scene),
                                y: world.y + 20.0 * f64::from(scene),
                            };
                            assert_ne!(
                                Navigator::new(40.0, now).step_raster(now, cursor, geometry, &mask),
                                PursuitStep::Arrived
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fully_discarded_opaque_source_pixel_blocks_instead_of_chasing_invisible_art() {
        use crate::motion::{Navigator, Point, PursuitStep, RasterMask, SpriteGeometry};
        let mut sprite = vec![0; SPRITE_WIDTH * SPRITE_HEIGHT * 4];
        sprite[3] = 255;
        let size = dimensions(false, 0.25);
        let projection = SpriteProjection::new(size, false, 0.25);
        let alpha = projection.alpha(&sprite);
        assert!(alpha.iter().all(|&a| a == 0));
        let mask = RasterMask::new(size.0 as usize, size.1 as usize, alpha);
        let now = std::time::Instant::now();
        assert!(matches!(
            Navigator::new(40.0, now).step_raster(
                now,
                Point { x: 0.125, y: 0.125 },
                SpriteGeometry {
                    origin: Point::default(),
                    pixel_scale: 1.0
                },
                &mask
            ),
            PursuitStep::Blocked(_)
        ));
        let mut frame = vec![0; size.0 as usize * size.1 as usize * 4];
        Renderer::new(None, 12.0).with_sprite_scale(0.25).compose(
            &mut frame,
            size,
            &sprite,
            &Presentation::default(),
            None,
            false,
        );
        assert!(frame.chunks_exact(4).all(|p| p[3] == 0));
    }

    #[test]
    fn transparent_sprite_never_uses_bubble_alpha_for_contact() {
        use crate::motion::{Navigator, Point, PursuitStep, RasterMask, SpriteGeometry};
        let sprite = vec![0; SPRITE_WIDTH * SPRITE_HEIGHT * 4];
        let size = dimensions(true, 0.25);
        let projection = SpriteProjection::new(size, true, 0.25);
        let (x0, y0, x1, y1) = projection.bounds;
        let mask = RasterMask::new(x1 - x0, y1 - y0, projection.alpha(&sprite));
        let mut frame = vec![0; size.0 as usize * size.1 as usize * 4];
        Renderer::new(None, 12.0).with_sprite_scale(0.25).compose(
            &mut frame,
            size,
            &sprite,
            &Presentation::default(),
            None,
            true,
        );
        assert!(frame.chunks_exact(4).any(|p| p[3] >= 32));
        let now = std::time::Instant::now();
        assert!(matches!(
            Navigator::new(40.0, now).step_raster(
                now,
                Point { x: 20.0, y: 20.0 },
                SpriteGeometry {
                    origin: Point {
                        x: x0 as f64,
                        y: y0 as f64
                    },
                    pixel_scale: 1.0
                },
                &mask
            ),
            PursuitStep::Blocked(_)
        ));
    }
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
        let (width, height) = dimensions(true, 0.5);
        let mut frame = vec![0; width as usize * height as usize * 4];
        let mut renderer =
            Renderer::new(Some(Typeface::load(None, None).unwrap()), 12.0).with_sprite_scale(0.5);
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
            dimensions(true, 1.0),
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
    fn identity_is_source_only_with_djinn_display_name() {
        let mut view = view(ActivityState::Working, false);
        for source in ["djinn", "Djinn", "opencode", "custom-source", "Custom App"] {
            view.following.as_mut().unwrap().source = source.into();
            let expected = if source == "djinn" { "Djinn" } else { source };
            let before = text(&view, None);
            assert_eq!(before.identity, expected);
            view.following.as_mut().unwrap().instance = "backend-private-id".into();
            view.following.as_mut().unwrap().activity = "session-private-id".into();
            assert_eq!(text(&view, None), before);
            view.following.as_mut().unwrap().instance.clear();
            assert_eq!(text(&view, None), before);
        }
    }

    #[test]
    fn missing_or_blank_summary_uses_untitled_session_even_when_unavailable() {
        let mut view = view(ActivityState::Idle, false);
        for available in [true, false] {
            view.following.as_mut().unwrap().available = available;
            for summary in [None, Some(String::new()), Some(" \n\t\0 ".into())] {
                view.following.as_mut().unwrap().summary = summary;
                let model = text(&view, None);
                assert_eq!(model.identity, "opencode");
                assert_eq!(model.summary, "Untitled session");
                assert_eq!(model.status, if available { "Idle" } else { "Unavailable" });
            }
        }
    }

    #[test]
    fn preview_without_focus_is_not_agent_activity() {
        let view = Presentation::default();
        assert_eq!(text(&view, None).identity, "Unattached familiar");
        assert_eq!(text(&view, None).summary, "Click for actions");
        assert_eq!(text(&view, None).status, "Unattached");
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
            let canvas = Canvas::new((384 * density, 352 * density), true, 1.0);
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
        let canvas = Canvas::new(dimensions(true, 1.0), true, 1.0);
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
            let logical = dimensions(show, 1.0);
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
        assert_eq!(Renderer::new(None, f32::NAN).font_size, 12.0);
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
        assert_eq!(dimensions(true, 1.0), (384, 352));
        assert_eq!(dimensions(false, 1.0), (192, 208));
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
            dimensions(false, 1.0),
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
    fn half_size_sprite_compacts_window_without_scaling_bubble_or_font() {
        assert_eq!(dimensions(true, 0.5), (384, 248));
        assert_eq!(dimensions(false, 0.5), (96, 104));
        let sprite: Vec<_> = (0..SPRITE_WIDTH * SPRITE_HEIGHT * 4)
            .map(|i| (i % 251) as u8)
            .collect();
        let selected = view(ActivityState::Working, false);
        let mut full = vec![0; WIDTH * HEIGHT * 4];
        Renderer::new(Some(Typeface::bundled().unwrap()), 12.0).compose(
            &mut full,
            dimensions(true, 1.0),
            &sprite,
            &selected,
            None,
            true,
        );
        let half_size = dimensions(true, 0.5);
        let mut half = vec![0; half_size.0 as usize * half_size.1 as usize * 4];
        Renderer::new(Some(Typeface::bundled().unwrap()), 12.0)
            .with_sprite_scale(0.5)
            .compose(&mut half, half_size, &sprite, &selected, None, true);
        assert_eq!(
            &full[..BUBBLE_HEIGHT * WIDTH * 4],
            &half[..BUBBLE_HEIGHT * WIDTH * 4]
        );
        for y in 0..104 {
            for x in 0..96 {
                let sx = (((x as f64 + 0.5) / 96.0) * 192.0).floor() as usize;
                let sy = (((y as f64 + 0.5) / 104.0) * 208.0).floor() as usize;
                let source = (sy * SPRITE_WIDTH + sx) * 4;
                let target = (((y + BUBBLE_HEIGHT) * WIDTH) + x + 144) * 4;
                assert_eq!(&half[target..target + 4], &sprite[source..source + 4]);
            }
        }
        // A half-size sprite on a 2x display still has one physical pixel per
        // source pixel: text/display density and sprite scale are independent.
        let mut dense_sprite = vec![0; sprite.len()];
        Renderer::new(None, 12.0).with_sprite_scale(0.5).compose(
            &mut dense_sprite,
            (192, 208),
            &sprite,
            &selected,
            None,
            false,
        );
        assert_eq!(dense_sprite, sprite);
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
            dimensions(true, 1.0),
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
            dimensions(false, 1.0),
            &[],
            &Presentation::default(),
            None,
            false,
        );
    }
}
