//! Pointer-only Linux menu. Tracking and painting never mutate familiar state.
use anyhow::{Context as _, Result};

use crate::{
    activity::{AttachmentBinding, FocusTarget},
    context_menu::{self, Action, AttachmentContext, ChatChoice, Entry},
    ipc::{FamiliarTargetSnapshot, RuntimeBinding},
    typography::Typeface,
};

const PAD: u32 = 4;
const ROW: u32 = 30;
const SIZE: (u32, u32) = (320, 338);
const LEFT: u32 = 272;

pub(crate) struct AttachmentMenu {
    pub context: AttachmentContext,
    pub frozen: Option<FrozenAttachment>,
}

impl AttachmentMenu {
    pub fn new(
        id: &str,
        applied: &AttachmentBinding,
        snapshot: Option<FamiliarTargetSnapshot>,
    ) -> Self {
        let (context, frozen) = attachment(id, applied, snapshot);
        Self { context, frozen }
    }
}

pub(crate) struct FrozenAttachment {
    binding: RuntimeBinding,
    choices: Vec<ChatChoice>,
}

/// Only a snapshot of the applied attachment may supply selectable choices.
pub fn attachment(
    id: &str,
    applied: &AttachmentBinding,
    snapshot: Option<FamiliarTargetSnapshot>,
) -> (AttachmentContext, Option<FrozenAttachment>) {
    let Some(snapshot) = snapshot.filter(|s| &s.binding.attachment == applied) else {
        return (
            AttachmentContext {
                current: applied.target.clone(),
                available: false,
                choices: Vec::new(),
            },
            None,
        );
    };
    let choices: Vec<_> = snapshot
        .targets
        .iter()
        .filter(|target| target.claimed_by.as_deref().is_none_or(|owner| owner == id))
        .map(|target| ChatChoice {
            target: FocusTarget::from(&target.selection),
            incarnation: target.incarnation,
            summary: target.selection.summary.clone(),
        })
        .collect();
    (
        AttachmentContext {
            current: applied.target.clone(),
            available: true,
            choices: choices.clone(),
        },
        Some(FrozenAttachment {
            binding: snapshot.binding,
            choices,
        }),
    )
}

impl FrozenAttachment {
    /// A busy fresh read is safe: the submitted runtime binding is still frozen,
    /// and the authoritative queue must validate it when applying the command.
    pub fn queue(
        &self,
        id: &str,
        index: usize,
        applied: &AttachmentBinding,
        fresh: Option<&FamiliarTargetSnapshot>,
        submit: impl FnOnce(&RuntimeBinding, &ChatChoice) -> Result<()>,
    ) -> Result<()> {
        anyhow::ensure!(
            &self.binding.attachment == applied,
            "chat attachment changed"
        );
        let choice = self.choices.get(index).context("chat attachment changed")?;
        anyhow::ensure!(
            fresh.is_none_or(|fresh| {
                fresh.binding == self.binding
                    && fresh.targets.iter().any(|target| {
                        target.incarnation == choice.incarnation
                            && FocusTarget::from(&target.selection) == choice.target
                            && target.claimed_by.as_deref().is_none_or(|owner| owner == id)
                    })
            }),
            "selected chat is no longer available"
        );
        submit(&self.binding, choice)
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum MenuResult {
    None,
    Changed,
    Dismiss,
    Action(Action),
}

struct Page {
    entries: Vec<Entry>,
    offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum RowTarget {
    Entry(usize),
    Back,
    Previous,
    Next,
    Dismiss,
}

pub(crate) struct Model {
    pages: Vec<Page>,
    viewport: (u32, u32),
    hover: Option<usize>,
    pressed: Option<usize>,
}

impl Model {
    pub fn new(context: crate::context_menu::Context) -> Self {
        Self {
            pages: vec![Page {
                entries: context_menu::entries(context),
                offset: 0,
            }],
            viewport: SIZE,
            hover: None,
            pressed: None,
        }
    }

    /// Stable requested size across all pages, in logical pixels.
    pub fn size(&self) -> (u32, u32) {
        SIZE
    }

    #[cfg(test)]
    fn visible_position(&self, matches: impl Fn(RowTarget) -> bool) -> Option<(f64, f64)> {
        self.visible()
            .into_iter()
            .enumerate()
            .find(|(_, target)| self.row(*target).1 && matches(*target))
            .map(|(row, _)| (40.0, f64::from(PAD + ROW * row as u32 + ROW / 2)))
    }

    #[cfg(test)]
    pub fn visible_position_for_action(&self, action: Action) -> Option<(f64, f64)> {
        self.visible_position(|target| match target {
            RowTarget::Entry(index) => matches!(
                &self.pages.last().unwrap().entries[index],
                Entry::Item { action: Some(candidate), .. } if *candidate == action
            ),
            _ => false,
        })
    }

    #[cfg(test)]
    pub fn visible_position_for_submenu(&self, label: &str) -> Option<(f64, f64)> {
        self.visible_position(|target| match target {
            RowTarget::Entry(index) => matches!(
                &self.pages.last().unwrap().entries[index],
                Entry::Submenu { label: candidate, .. } if candidate == label
            ),
            _ => false,
        })
    }

    #[cfg(test)]
    pub fn visible_position_for_navigation(&self, label: &str) -> Option<(f64, f64)> {
        self.visible_position(|target| {
            !matches!(target, RowTarget::Entry(_)) && self.row(target).0 == label
        })
    }

    /// Call on compositor configure before dispatching input or painting.
    pub fn configure(&mut self, size: (u32, u32)) -> bool {
        let size = (size.0.min(SIZE.0), size.1.min(SIZE.1));
        if size == self.viewport {
            return false;
        }
        self.viewport = size;
        self.hover = None;
        self.pressed = None;
        // Reflow from the beginning instead of retaining an inaccessible tail.
        for page in &mut self.pages {
            page.offset = 0;
        }
        true
    }

    pub fn input_rect(&self) -> (i32, i32, i32, i32) {
        (
            PAD as i32,
            PAD as i32,
            self.viewport.0.saturating_sub(2 * PAD) as i32,
            self.viewport.1.saturating_sub(2 * PAD) as i32,
        )
    }

    fn capacity(&self) -> usize {
        if self.viewport.0 < 96 || self.viewport.1 < 2 * PAD + 4 * ROW {
            return 0;
        }
        ((self.viewport.1 - 2 * PAD) / ROW).min(11) as usize
    }

    fn visible(&self) -> Vec<RowTarget> {
        let capacity = self.capacity();
        if capacity == 0 {
            return Vec::new();
        }
        let page = self.pages.last().unwrap();
        let paged = page.entries.len() > 8 || page.entries.len() + 1 > capacity;
        let count = if paged { capacity - 3 } else { capacity - 1 }.min(8);
        let end = (page.offset + count).min(page.entries.len());
        let mut rows: Vec<_> = (page.offset..end).map(RowTarget::Entry).collect();
        rows.push(if self.pages.len() == 1 {
            RowTarget::Dismiss
        } else {
            RowTarget::Back
        });
        if paged {
            rows.extend([RowTarget::Previous, RowTarget::Next]);
        }
        rows
    }

    fn row(&self, target: RowTarget) -> (&str, bool, bool, bool) {
        let page = self.pages.last().unwrap();
        match target {
            RowTarget::Entry(index) => match &page.entries[index] {
                Entry::Item {
                    label,
                    action,
                    enabled,
                    checked,
                    ..
                } => (label, *enabled && action.is_some(), *checked, false),
                Entry::Submenu { label, entries } => (label, !entries.is_empty(), false, true),
                Entry::Separator => ("", false, false, false),
            },
            RowTarget::Back => ("Back", true, false, false),
            RowTarget::Dismiss => ("Dismiss", true, false, false),
            RowTarget::Previous => ("Previous", page.offset > 0, false, false),
            RowTarget::Next => (
                "Next",
                page.offset + self.capacity().saturating_sub(3).min(8) < page.entries.len(),
                false,
                false,
            ),
        }
    }

    fn hit(&self, position: (f64, f64)) -> Option<usize> {
        let (x, y) = position;
        if !x.is_finite()
            || !y.is_finite()
            || x < f64::from(PAD)
            || y < f64::from(PAD)
            || x >= f64::from(self.viewport.0.saturating_sub(PAD))
            || y >= f64::from(self.viewport.1.saturating_sub(PAD))
        {
            return None;
        }
        let index = ((y - f64::from(PAD)) / f64::from(ROW)) as usize;
        self.visible()
            .get(index)
            .filter(|&&target| self.row(target).1)
            .map(|_| index)
    }

    pub fn pointer_motion(&mut self, position: (f64, f64)) -> bool {
        let hover = self.hit(position);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    pub fn pointer_press(&mut self, button: u32, position: (f64, f64)) {
        if button == LEFT {
            self.pressed = self.hit(position);
        }
    }

    pub fn pointer_release(&mut self, button: u32, position: (f64, f64)) -> MenuResult {
        if button != LEFT {
            return MenuResult::None;
        }
        let pressed = self.pressed.take();
        let Some(index) = pressed.filter(|pressed| Some(*pressed) == self.hit(position)) else {
            return MenuResult::None;
        };
        let target = self.visible()[index];
        let step = self.capacity().saturating_sub(3).min(8);
        match target {
            RowTarget::Dismiss => return MenuResult::Dismiss,
            RowTarget::Back => {
                self.pages.pop();
            }
            RowTarget::Previous => self.pages.last_mut().unwrap().offset -= step,
            RowTarget::Next => self.pages.last_mut().unwrap().offset += step,
            RowTarget::Entry(index) => match &self.pages.last().unwrap().entries[index] {
                Entry::Item {
                    action: Some(action),
                    enabled: true,
                    ..
                } => return MenuResult::Action(*action),
                Entry::Submenu { entries, .. } => self.pages.push(Page {
                    entries: entries.clone(),
                    offset: 0,
                }),
                _ => return MenuResult::None,
            },
        }
        self.hover = None;
        MenuResult::Changed
    }

    /// Straight RGBA, top-left origin. Native owns font loading (including a lazy
    /// Typeface::bundled fallback when bubbles have not loaded a face).
    pub fn paint(
        &self,
        typeface: &mut Typeface,
        physical_size: (u32, u32),
        logical_size: (u32, u32),
    ) -> Vec<u8> {
        let (pw, ph) = physical_size;
        // Bound allocations and reject malformed scales, without shrinking fonts.
        if pw > 8192 || ph > 8192 {
            return Vec::new();
        }
        let mut pixels = vec![0; pw as usize * ph as usize * 4];
        if logical_size.0 == 0 || logical_size.1 == 0 {
            return pixels;
        }
        let scale = pw / logical_size.0;
        if scale == 0
            || scale > 8
            || pw % logical_size.0 != 0
            || logical_size.1.checked_mul(scale) != Some(ph)
        {
            return pixels;
        }
        let width = self.viewport.0.min(logical_size.0);
        let height = self.viewport.1.min(logical_size.1);
        if width <= 2 * PAD || height <= 2 * PAD {
            return pixels;
        }
        let panel = (
            PAD * scale,
            PAD * scale,
            (width - PAD) * scale,
            (height - PAD) * scale,
        );
        fill(&mut pixels, pw, panel, [29, 32, 39, 255]);
        for (index, target) in self.visible().into_iter().enumerate() {
            let top = (PAD + index as u32 * ROW) * scale;
            let bottom = top + ROW * scale;
            if bottom > panel.3 {
                break;
            }
            let (label, enabled, checked, submenu) = self.row(target);
            if self.hover == Some(index) && enabled {
                fill(
                    &mut pixels,
                    pw,
                    (panel.0, top, panel.2, bottom),
                    [55, 68, 89, 255],
                );
            }
            let color = if enabled {
                [236, 239, 245]
            } else {
                [133, 139, 151]
            };
            let clip = (panel.0, top, panel.2, bottom);
            if checked {
                // Geometric marker works even with a face lacking UI symbols.
                fill(
                    &mut pixels,
                    pw,
                    (11 * scale, top + 12 * scale, 17 * scale, top + 18 * scale),
                    [140, 195, 236, 255],
                );
            }
            let font = if matches!(target, RowTarget::Entry(_)) {
                14.0
            } else {
                12.0
            };
            let px = font * scale as f32;
            let right = panel.2.saturating_sub(22 * scale);
            let x = 25 * scale;
            let available = right.saturating_sub(x) as f32;
            let mut text = label.to_owned();
            if typeface.measure(&text, px) > available {
                while !text.is_empty() && typeface.measure(&format!("{text}..."), px) > available {
                    text.pop();
                }
                text.push_str("...");
            }
            let metrics = typeface.line_metrics(px);
            let baseline = top as f32
                + (ROW as f32 * scale as f32 - (metrics.ascent - metrics.descent)) / 2.0
                + metrics.ascent;
            draw_text(
                &mut pixels,
                pw,
                typeface,
                &text,
                px,
                (x as f32, baseline),
                color,
                (clip.0, clip.1, right, clip.3),
            );
            if submenu {
                draw_text(
                    &mut pixels,
                    pw,
                    typeface,
                    ">",
                    px,
                    ((panel.2 - 16 * scale) as f32, baseline),
                    color,
                    clip,
                );
            }
        }
        pixels
    }
}

fn fill(pixels: &mut [u8], width: u32, rect: (u32, u32, u32, u32), color: [u8; 4]) {
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let offset = (y as usize * width as usize + x as usize) * 4;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    pixels: &mut [u8],
    width: u32,
    face: &mut Typeface,
    text: &str,
    px: f32,
    origin: (f32, f32),
    color: [u8; 3],
    clip: (u32, u32, u32, u32),
) {
    let mut x = origin.0;
    let mut previous = None;
    for c in text.chars() {
        if let Some(previous) = previous {
            x += face.kern(previous, c, px);
        }
        let glyph = face.glyph(c, px);
        let left = x.round() as i32 + glyph.metrics.xmin;
        // fontdue bitmap rows run downward; ymin is measured upward from baseline.
        let top = origin.1.round() as i32 - glyph.metrics.ymin - glyph.metrics.height as i32;
        for gy in 0..glyph.metrics.height {
            let y = top + gy as i32;
            if y < clip.1 as i32 || y >= clip.3 as i32 {
                continue;
            }
            for gx in 0..glyph.metrics.width {
                let dx = left + gx as i32;
                if dx < clip.0 as i32 || dx >= clip.2 as i32 {
                    continue;
                }
                let coverage = u32::from(glyph.coverage[gy * glyph.metrics.width + gx]);
                let offset = (y as usize * width as usize + dx as usize) * 4;
                for channel in 0..3 {
                    pixels[offset + channel] = ((u32::from(color[channel]) * coverage
                        + u32::from(pixels[offset + channel]) * (255 - coverage)
                        + 127)
                        / 255) as u8;
                }
            }
        }
        x += glyph.metrics.advance_width;
        previous = Some(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{context_menu::Context, protocol::FamiliarSettings};

    fn context() -> Context {
        Context {
            paused: false,
            bubbles: true,
            copy_session_id: false,
            open_chat: false,
            navigation: false,
            settings: Some(FamiliarSettings::default()),
            preferences: false,
            attachments: None,
            open_familiar: None,
        }
    }

    fn model() -> Model {
        Model::new(context())
    }

    fn click_position(model: &mut Model, position: (f64, f64)) -> MenuResult {
        model.pointer_press(LEFT, position);
        model.pointer_release(LEFT, position)
    }

    fn find(model: &mut Model, lookup: impl Fn(&Model) -> Option<(f64, f64)>) -> (f64, f64) {
        for _ in 0..1024 {
            if let Some(position) = lookup(model) {
                return position;
            }
            let next = model
                .visible_position_for_navigation("Next")
                .expect("target not reachable");
            assert_eq!(click_position(model, next), MenuResult::Changed);
        }
        panic!("paging did not terminate")
    }

    fn open_submenu(model: &mut Model, label: &str) {
        let position = find(model, |model| model.visible_position_for_submenu(label));
        assert_eq!(click_position(model, position), MenuResult::Changed);
    }

    fn click(model: &mut Model, row: usize) -> MenuResult {
        let position = (40.0, f64::from(PAD + ROW * row as u32 + ROW / 2));
        model.pointer_press(LEFT, position);
        model.pointer_release(LEFT, position)
    }

    fn choice(index: usize) -> ChatChoice {
        ChatChoice {
            target: FocusTarget {
                source: "djinn".into(),
                instance: format!("instance-{index}"),
                activity: "chat".into(),
            },
            incarnation: index as u64 + 1,
            summary: Some("Same title".into()),
        }
    }

    #[test]
    fn root_matches_shared_catalog_exactly_across_contexts() {
        for settings in [
            None,
            Some(FamiliarSettings::default()),
            Some(FamiliarSettings {
                sprite_scale: 0.5 + f64::EPSILON,
                font_size: 13.0,
                bubble_duration: 7.0,
            }),
        ] {
            for attachment_state in 0..5 {
                for features in [false, true] {
                    for capacity in [None, Some(false), Some(true)] {
                        let make_context = || Context {
                            paused: features,
                            bubbles: features,
                            copy_session_id: features,
                            open_chat: features,
                            navigation: features,
                            settings,
                            preferences: features,
                            attachments: (attachment_state > 0).then(|| AttachmentContext {
                                current: (attachment_state >= 3).then(|| choice(0).target),
                                available: attachment_state % 2 == 1,
                                choices: vec![choice(0)],
                            }),
                            open_familiar: capacity,
                        };
                        let model = Model::new(make_context());
                        assert_eq!(
                            model.pages[0].entries,
                            context_menu::entries(make_context())
                        );
                        assert!(!model.pages[0].entries.iter().any(|entry| match entry {
                            Entry::Item { label, .. } | Entry::Submenu { label, .. } =>
                                label == "Appearance" || label == "Reveal status",
                            Entry::Separator => false,
                        }));
                    }
                }
            }
        }
    }

    #[test]
    fn paginated_root_reaches_every_enabled_entry_and_preserves_separators() {
        for size in [SIZE, (180, 158)] {
            for enabled in [false, true] {
                for capacity in [None, Some(false), Some(true)] {
                    let mut model = Model::new(Context {
                        copy_session_id: enabled,
                        open_chat: enabled,
                        navigation: enabled,
                        open_familiar: capacity,
                        attachments: Some(AttachmentContext {
                            current: Some(choice(0).target),
                            available: true,
                            choices: vec![choice(0)],
                        }),
                        ..context()
                    });
                    model.configure(size);
                    let expected = model.pages[0].entries.clone();
                    let mut visited = Vec::new();
                    loop {
                        for (row, target) in model.visible().into_iter().enumerate() {
                            let RowTarget::Entry(index) = target else {
                                continue;
                            };
                            visited.push(index);
                            match &expected[index] {
                                Entry::Item {
                                    action: Some(action),
                                    enabled,
                                    ..
                                } => {
                                    let action = *action;
                                    let enabled = *enabled;
                                    assert_eq!(
                                        model.visible_position_for_action(action).is_some(),
                                        enabled
                                    );
                                    assert_eq!(
                                        click(&mut model, row),
                                        if enabled {
                                            MenuResult::Action(action)
                                        } else {
                                            MenuResult::None
                                        }
                                    );
                                }
                                Entry::Separator => {
                                    assert_eq!(click(&mut model, row), MenuResult::None)
                                }
                                Entry::Submenu { label, .. } => {
                                    let position =
                                        model.visible_position_for_submenu(label).unwrap();
                                    assert_eq!(
                                        click_position(&mut model, position),
                                        MenuResult::Changed
                                    );
                                    let back =
                                        model.visible_position_for_navigation("Back").unwrap();
                                    assert_eq!(
                                        click_position(&mut model, back),
                                        MenuResult::Changed
                                    );
                                }
                                _ => panic!("unexpected root entry"),
                            }
                        }
                        let Some(next) = model.visible_position_for_navigation("Next") else {
                            break;
                        };
                        assert_eq!(click_position(&mut model, next), MenuResult::Changed);
                    }
                    assert_eq!(visited, (0..expected.len()).collect::<Vec<_>>());
                    assert_eq!(model.pages[0].entries, expected);
                    let dismiss = model.visible_position_for_navigation("Dismiss").unwrap();
                    assert_eq!(click_position(&mut model, dismiss), MenuResult::Dismiss);
                }
            }
        }
    }

    #[test]
    fn settings_and_preferences_follow_context_gates() {
        for settings in [None, Some(FamiliarSettings::default())] {
            for preferences in [false, true] {
                let mut model = Model::new(Context {
                    settings,
                    preferences,
                    ..context()
                });
                if settings.is_none() && !preferences {
                    assert!(!model.pages[0].entries.iter().any(
                        |entry| matches!(entry, Entry::Submenu { label, .. } if label == "Settings")
                    ));
                    continue;
                }
                open_submenu(&mut model, "Settings");
                assert_eq!(
                    model
                        .visible_position_for_action(Action::ForgetPreferences)
                        .is_some(),
                    preferences
                );
                if preferences {
                    let position = model
                        .visible_position_for_action(Action::ForgetPreferences)
                        .unwrap();
                    assert_eq!(
                        click_position(&mut model, position),
                        MenuResult::Action(Action::ForgetPreferences)
                    );
                }
                let fields = &model.pages.last().unwrap().entries;
                assert_eq!(
                    fields
                        .iter()
                        .filter(|entry| matches!(entry, Entry::Submenu { .. }))
                        .count(),
                    if settings.is_some() {
                        crate::menu::SETTINGS.len()
                    } else {
                        0
                    }
                );
            }
        }
    }

    fn visit(model: &mut Model, indices: &mut Vec<usize>) {
        loop {
            let rows = model.visible();
            assert!(rows.len() <= 11);
            assert!(
                rows.iter()
                    .filter(|row| matches!(row, RowTarget::Entry(_)))
                    .count()
                    <= 8
            );
            for (row, target) in rows.iter().enumerate() {
                if let RowTarget::Entry(index) = *target {
                    match &model.pages.last().unwrap().entries[index] {
                        Entry::Submenu { .. } => {
                            assert_eq!(click(model, row), MenuResult::Changed);
                            visit(model, indices);
                        }
                        Entry::Item {
                            action: Some(Action::Attach(index)),
                            ..
                        } => {
                            let index = *index;
                            assert_eq!(
                                click(model, row),
                                MenuResult::Action(Action::Attach(index))
                            );
                            indices.push(index);
                        }
                        _ => {}
                    }
                }
            }
            if let Some(row) = rows
                .iter()
                .position(|target| *target == RowTarget::Next && model.row(*target).1)
            {
                assert_eq!(click(model, row), MenuResult::Changed);
            } else {
                let back = rows
                    .iter()
                    .position(|target| *target == RowTarget::Back)
                    .unwrap();
                assert_eq!(click(model, back), MenuResult::Changed);
                break;
            }
        }
    }

    #[test]
    fn all_512_original_indices_are_reachable_with_bounded_pages() {
        for size in [SIZE, (180, 158)] {
            let mut model = Model::new(Context {
                attachments: Some(AttachmentContext {
                    current: None,
                    available: true,
                    choices: (0..512).map(choice).collect(),
                }),
                ..context()
            });
            model.configure(size);
            open_submenu(&mut model, "Attach to app / chat");
            let mut indices = Vec::new();
            visit(&mut model, &mut indices);
            assert_eq!(indices, (0..512).collect::<Vec<_>>());
        }
    }

    #[test]
    fn paging_boundaries_never_hide_a_tail_and_size_stays_stable() {
        for count in [1, 8, 9, 10, 11, 16, 17] {
            let mut model = Model::new(Context {
                paused: true,
                bubbles: false,
                attachments: Some(AttachmentContext {
                    current: None,
                    available: true,
                    choices: (0..count).map(choice).collect(),
                }),
                ..context()
            });
            assert_eq!(
                model.visible_position_for_action(Action::RevealStatus),
                None
            );
            let pause = find(&mut model, |model| {
                model.visible_position_for_action(Action::SetPaused(false))
            });
            assert_eq!(
                click_position(&mut model, pause),
                MenuResult::Action(Action::SetPaused(false))
            );
            // Start at the root's beginning before searching for an earlier submenu.
            model.pages[0].offset = 0;
            open_submenu(&mut model, "Attach to app / chat");
            let mut indices = Vec::new();
            visit(&mut model, &mut indices);
            assert_eq!(indices, (0..count).collect::<Vec<_>>());
            assert_eq!(model.size(), SIZE);
        }
    }

    #[test]
    fn exact_f64_custom_values_are_not_rounded_to_presets() {
        let mut model = Model::new(Context {
            settings: Some(FamiliarSettings {
                sprite_scale: 0.5 + f64::EPSILON,
                font_size: 13.0,
                bubble_duration: 7.0,
            }),
            ..context()
        });
        open_submenu(&mut model, "Settings");
        for entry in &model.pages.last().unwrap().entries {
            let Entry::Submenu { entries, .. } = entry else {
                panic!()
            };
            assert_eq!(entries.len(), 5);
            assert!(
                entries[..4]
                    .iter()
                    .all(|entry| matches!(entry, Entry::Item { checked: false, .. }))
            );
            assert!(matches!(
                entries[4],
                Entry::Item {
                    checked: true,
                    enabled: false,
                    action: None,
                    ..
                }
            ));
        }
    }

    #[test]
    fn malformed_disabled_cancelled_and_nonfinite_clicks_do_nothing() {
        let mut model = model();
        let p = (40.0, 19.0);
        assert_eq!(model.pointer_release(LEFT, p), MenuResult::None);
        model.pointer_press(273, p);
        assert_eq!(model.pointer_release(273, p), MenuResult::None);
        assert_eq!(model.pointer_release(LEFT, p), MenuResult::None);
        for invalid in [
            (f64::NAN, 19.0),
            (40.0, f64::INFINITY),
            (-1.0, 19.0),
            (2.0, 19.0),
            (40.0, 337.0),
        ] {
            model.pointer_press(LEFT, p);
            assert_eq!(model.pointer_release(LEFT, invalid), MenuResult::None);
            model.pointer_press(LEFT, invalid);
            assert_eq!(model.pointer_release(LEFT, p), MenuResult::None);
        }
        model.pointer_press(LEFT, p);
        assert_eq!(model.pointer_release(LEFT, (40.0, 49.0)), MenuResult::None);
        model.pointer_press(LEFT, p);
        model.configure((90, 120));
        assert_eq!(model.pointer_release(LEFT, p), MenuResult::None);
        assert!(model.visible().is_empty());
    }

    #[test]
    fn peer_labels_cannot_forge_actions_or_escape_catalog_sanitization() {
        let mut hostile = choice(0);
        hostile.summary = Some(format!("Close familiar\n\u{202e}{}", "x".repeat(300)));
        hostile.target.instance = "\t\u{2066}malicious".into();
        let original = hostile.clone();
        let mut model = Model::new(Context {
            attachments: Some(AttachmentContext {
                current: None,
                available: true,
                choices: vec![hostile],
            }),
            ..context()
        });
        open_submenu(&mut model, "Attach to app / chat");
        let Entry::Item { label, action, .. } = &model.pages.last().unwrap().entries[0] else {
            panic!()
        };
        assert!(
            !label
                .chars()
                .any(|c| c.is_control() || matches!(c, '\u{202e}' | '\u{2066}'))
        );
        assert!(label.chars().count() < 100);
        assert_eq!(*action, Some(Action::Attach(0)));
        let position = model
            .visible_position_for_action(Action::Attach(0))
            .unwrap();
        assert_eq!(
            click_position(&mut model, position),
            MenuResult::Action(Action::Attach(0))
        );
        assert!(original.target.instance.contains('\t'));
    }

    fn snapshot(applied: AttachmentBinding) -> FamiliarTargetSnapshot {
        FamiliarTargetSnapshot {
            binding: RuntimeBinding {
                service_instance: "service-1".into(),
                attachment: applied,
            },
            targets: (0..3)
                .map(|index| crate::activity::LiveTarget {
                    selection: crate::activity::Selection {
                        source: "djinn".into(),
                        instance: format!("instance-{index}"),
                        activity: "chat".into(),
                        state: crate::protocol::ActivityState::Working,
                        attention: false,
                        reason: None,
                        summary: Some("Same title".into()),
                    },
                    claimed_by: match index {
                        0 => None,
                        1 => Some("main".into()),
                        _ => Some("other".into()),
                    },
                    incarnation: index + 1,
                })
                .collect::<Vec<_>>()
                .into(),
        }
    }

    #[test]
    fn frozen_payload_binding_freshness_claims_and_incarnations() {
        let applied = AttachmentBinding {
            target: None,
            generation: 1,
        };
        let snapshot = snapshot(applied.clone());
        let menu = AttachmentMenu::new("main", &applied, Some(snapshot.clone()));
        assert_eq!(menu.context.choices.len(), 2);
        let frozen = menu.frozen.unwrap();
        let mut reordered = snapshot.clone();
        let mut targets = reordered.targets.to_vec();
        targets.reverse();
        reordered.targets = targets.into();
        for fresh in [None, Some(&snapshot), Some(&reordered)] {
            frozen
                .queue("main", 1, &applied, fresh, |binding, selected| {
                    assert_eq!(binding, &snapshot.binding);
                    assert_eq!(selected, &choice(1));
                    Ok(())
                })
                .unwrap();
        }
        for change in 0..5 {
            let mut fresh = snapshot.clone();
            let mut targets = fresh.targets.to_vec();
            match change {
                0 => targets[0].incarnation += 1,
                1 => targets[0].claimed_by = Some("other".into()),
                2 => {
                    targets.remove(0);
                }
                3 => fresh.binding.service_instance = "new-service".into(),
                _ => fresh.binding.attachment.generation += 1,
            }
            fresh.targets = targets.into();
            assert!(
                frozen
                    .queue("main", 0, &applied, Some(&fresh), |_, _| panic!(
                        "stale submission"
                    ))
                    .is_err()
            );
        }
        for index in [2, usize::MAX] {
            assert!(
                frozen
                    .queue("main", index, &applied, None, |_, _| panic!())
                    .is_err()
            );
        }
        let changed = AttachmentBinding {
            generation: 2,
            ..applied.clone()
        };
        assert!(
            frozen
                .queue("main", 0, &changed, None, |_, _| panic!())
                .is_err()
        );
        assert!(
            frozen
                .queue("main", 0, &applied, None, |_, _| anyhow::bail!(
                    "queue full"
                ))
                .is_err()
        );
        for snapshot in [None, Some(snapshot)] {
            let (context, frozen) = attachment("main", &changed, snapshot);
            assert!(!context.available);
            assert!(context.choices.is_empty());
            assert!(frozen.is_none());
        }
    }

    #[test]
    fn renderer_rgba_alpha_hover_integer_scale_and_clipping() {
        let mut model = model();
        let mut face = Typeface::bundled().unwrap();
        for size in [SIZE, (140, 158), (30, 30)] {
            model.configure(size);
            for scale in [1, 2] {
                let physical = (size.0 * scale, size.1 * scale);
                let normal = model.paint(&mut face, physical, size);
                assert_eq!(normal.len(), physical.0 as usize * physical.1 as usize * 4);
                for y in 0..physical.1 {
                    for x in 0..physical.0 {
                        let alpha = normal[((y * physical.0 + x) * 4 + 3) as usize];
                        assert_eq!(
                            alpha,
                            if x >= PAD * scale
                                && y >= PAD * scale
                                && x < (size.0 - PAD) * scale
                                && y < (size.1 - PAD) * scale
                            {
                                255
                            } else {
                                0
                            }
                        );
                    }
                }
                if size.0 >= 96 {
                    model.pointer_motion((40.0, 19.0));
                    assert_ne!(normal, model.paint(&mut face, physical, size));
                    model.pointer_motion((-1.0, -1.0));
                }
            }
        }
    }
}
