//! Native-surface-independent playback and split CPU composition.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    RunOptions,
    activity::Presentation,
    animation::{Animation, Player},
    assets::{Character, SpriteAtlas},
    bubble::{self, Renderer},
    display::LiveDisplay,
    motion::Facing,
    protocol::FamiliarSettings,
    sprite::{initial_facing, selected_facing},
    typography::Typeface,
    visibility::BubbleVisibility,
};

const DEMO_INTERVAL: Duration = Duration::from_secs(3);

enum CornerReturn {
    Live,
    Preview {
        animation: Animation,
        selection_index: usize,
    },
}

pub(crate) struct FamiliarScene {
    pub id: String,
    pub character: &'static Character,
    pub options: RunOptions,
    pub sprite_dirty: bool,
    pub bubble_dirty: bool,
    atlas: Arc<SpriteAtlas>,
    player: Player,
    facing: Facing,
    selection_index: usize,
    display: LiveDisplay,
    latest_view: Presentation,
    renderer: Renderer,
    visibility: BubbleVisibility,
    visible: bool,
    last_tick: Instant,
    next_demo: Instant,
    corner_return: Option<CornerReturn>,
}

impl FamiliarScene {
    pub fn new(
        id: String,
        character: &'static Character,
        atlas: Arc<SpriteAtlas>,
        options: RunOptions,
        typeface: Option<Typeface>,
    ) -> Self {
        let now = Instant::now();
        let mut display = LiveDisplay::default();
        if options.manual || options.demo {
            display.manual(options.animation);
        } else {
            display.on_advance(options.animation);
        }
        let mut visibility = BubbleVisibility::new(
            !options.no_bubble,
            options.always_bubble,
            options.bubble_duration,
        );
        visibility.observe(&Presentation::default(), now);
        Self {
            facing: selected_facing(options.animation, initial_facing(&id)),
            id,
            character,
            player: Player::new(options.animation),
            selection_index: Animation::ALL
                .iter()
                .position(|&a| a == options.animation)
                .unwrap_or(0),
            renderer: Renderer::new(typeface, options.font_size)
                .with_sprite_scale(options.sprite_scale),
            visible: visibility.visible(now),
            options,
            atlas,
            display,
            latest_view: Presentation::default(),
            visibility,
            sprite_dirty: true,
            bubble_dirty: true,
            last_tick: now,
            next_demo: now + DEMO_INTERVAL,
            corner_return: None,
        }
    }

    /// Returns whether any setting changed; invalid settings leave the scene untouched.
    pub fn apply_settings(&mut self, settings: FamiliarSettings) -> anyhow::Result<bool> {
        settings.validate()?;
        let sprite_scale = settings.sprite_scale as f32;
        let font_size = settings.font_size as f32;
        let duration = Duration::from_secs_f64(settings.bubble_duration);
        let sprite_changed = self.options.sprite_scale != sprite_scale;
        let font_changed = self.options.font_size != font_size;
        let changed = sprite_changed || font_changed || self.options.bubble_duration != duration;
        self.options.sprite_scale = sprite_scale;
        self.options.font_size = font_size;
        self.options.bubble_duration = duration;
        self.renderer.set_sprite_scale(sprite_scale);
        self.renderer.set_font_size(font_size);
        self.visibility.set_duration(duration);
        self.sprite_dirty |= sprite_changed;
        self.bubble_dirty |= font_changed;
        Ok(changed)
    }

    pub fn update(&mut self, view: Presentation, now: Instant) {
        if self.latest_view != view {
            let before = bubble::text(&self.latest_view, self.preview());
            self.visibility.observe(&view, now);
            self.latest_view = view.clone();
            if let Some(animation) = self.display.update(view)
                && !self.corner_active()
            {
                self.select(animation, now);
            }
            self.bubble_dirty |= before != bubble::text(&self.latest_view, self.preview());
        }
        self.sync_visibility(now);
    }

    pub fn reveal(&mut self, now: Instant) {
        self.visibility.reveal(now);
        self.sync_visibility(now);
    }

    pub fn begin_corner(&mut self, _now: Instant) {
        if self.corner_active() {
            return;
        }
        self.corner_return = Some(if self.display.is_manual() {
            CornerReturn::Preview {
                animation: self.player.animation(),
                selection_index: self.selection_index,
            }
        } else {
            CornerReturn::Live
        });
    }

    pub fn face_corner(&mut self, facing: crate::motion::Facing, now: Instant) {
        if !self.corner_active() {
            return;
        }
        self.set_facing(facing);
        let animation = match facing {
            crate::motion::Facing::Left => Animation::RunningLeft,
            crate::motion::Facing::Right => Animation::RunningRight,
        };
        if self.player.animation() != animation {
            self.player.select(animation);
            self.last_tick = now;
            self.sprite_dirty = true;
        }
    }

    pub fn finish_corner(&mut self, now: Instant) {
        let before = bubble::text(&self.latest_view, self.preview());
        let Some(saved) = self.corner_return.take() else {
            return;
        };
        match saved {
            // Source updates consumed travel-time cues; resume restores only the latest base.
            CornerReturn::Live => {
                let animation = self.display.resume();
                self.select(animation, now);
            }
            CornerReturn::Preview {
                animation,
                selection_index,
            } => {
                self.select(animation, now);
                self.selection_index = selection_index;
            }
        }
        self.bubble_dirty |= before != bubble::text(&self.latest_view, self.preview());
    }

    pub fn corner_active(&self) -> bool {
        self.corner_return.is_some()
    }

    pub fn cycle(&mut self, now: Instant) {
        let before = bubble::text(&self.latest_view, self.preview());
        let animation = Animation::ALL[(self.selection_index + 1) % Animation::ALL.len()];
        self.display.manual(animation);
        self.select(animation, now);
        self.bubble_dirty |= before != bubble::text(&self.latest_view, self.preview());
    }

    pub fn toggle_pause(&mut self, now: Instant) {
        self.set_paused(!self.options.paused, now);
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        if self.options.paused == paused {
            return;
        }
        self.options.paused = paused;
        self.last_tick = now;
        self.next_demo = now + DEMO_INTERVAL;
    }

    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        let expired = self.visibility.expire(now);
        self.bubble_dirty |= expired;
        self.sync_visibility(now);
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let mut deadline = self.visibility.deadline();
        if !self.options.paused {
            let before = bubble::text(&self.latest_view, self.preview());
            self.sprite_dirty |= self.player.advance(elapsed, &self.atlas);
            if !self.corner_active() {
                if let Some(animation) = self.display.on_advance(self.player.animation()) {
                    self.select(animation, now);
                }
                if self.options.demo && now >= self.next_demo {
                    self.cycle(now);
                }
            }
            self.bubble_dirty |= before != bubble::text(&self.latest_view, self.preview());
            let frame_at = now + self.player.until_next_frame(&self.atlas);
            deadline = Some(deadline.map_or(frame_at, |d| d.min(frame_at)));
            if self.options.demo && !self.corner_active() {
                deadline = deadline.map(|d| d.min(self.next_demo));
            }
        }
        deadline
    }

    pub fn bubble_visible(&self) -> bool {
        self.visible
    }

    pub fn facing(&self) -> Facing {
        self.facing
    }

    pub fn set_facing(&mut self, facing: Facing) -> bool {
        let changed = self.facing != facing;
        self.facing = facing;
        self.sprite_dirty |= changed;
        changed
    }

    pub fn sprite_size(&self) -> (u32, u32) {
        self.scaled_size(bubble::dimensions(false, self.options.sprite_scale))
    }

    pub fn bubble_size(&self) -> (u32, u32) {
        self.scaled_size(bubble::bubble_dimensions())
    }

    pub fn minimum_bubble_size(&self) -> (u32, u32) {
        let width = (self.options.font_size * 6.0 + 32.0).clamp(160.0, 384.0);
        let height = (self.options.font_size * 2.5 + 24.0).clamp(80.0, 144.0);
        (
            (f64::from(width) * self.options.scale).ceil() as u32,
            (f64::from(height) * self.options.scale).ceil() as u32,
        )
    }

    #[cfg(any(target_os = "linux", test))]
    pub fn render_sprite_raster(&mut self, raster: crate::raster::RasterSpec) -> Vec<u8> {
        let size = raster.physical_size();
        let mut frame = vec![0; size.0 as usize * size.1 as usize * 4];
        let sprite = self
            .atlas
            .frame(self.player.animation(), self.player.frame_index())
            .with_facing(self.facing());
        // Sample the source atlas directly, retaining straight RGBA and hidden RGB.
        let width = f64::from(crate::sprite::LOGICAL_WIDTH)
            * f64::from(self.options.sprite_scale)
            * self.options.scale;
        let height = f64::from(crate::sprite::LOGICAL_HEIGHT)
            * f64::from(self.options.sprite_scale)
            * self.options.scale;
        for y in 0..size.1 {
            for x in 0..size.0 {
                let (lx, ly) = raster.buffer_to_logical((f64::from(x) + 0.5, f64::from(y) + 0.5));
                if lx >= width || ly >= height {
                    continue;
                }
                let source = sprite.source_offset(lx / width, ly / height);
                let target = (y as usize * size.0 as usize + x as usize) * 4;
                frame[target..target + 4].copy_from_slice(&sprite[source..source + 4]);
            }
        }
        self.sprite_dirty = false;
        frame
    }

    #[cfg(any(target_os = "linux", test))]
    pub fn render_bubble_raster(
        &mut self,
        raster: crate::raster::RasterSpec,
        layout: crate::bubble_layout::BubbleLayout,
    ) -> Vec<u8> {
        let size = raster.physical_size();
        let mut frame = vec![0; size.0 as usize * size.1 as usize * 4];
        let preview = self.preview();
        self.renderer.compose_bubble_raster(
            &mut frame,
            raster,
            layout,
            self.options.scale,
            (&self.latest_view, preview),
        );
        self.bubble_dirty = false;
        frame
    }

    #[cfg(test)]
    pub fn render_sprite(&mut self, physical_size: (u32, u32)) -> Vec<u8> {
        let mut frame = vec![0; physical_size.0 as usize * physical_size.1 as usize * 4];
        self.renderer.compose_sprite(
            &mut frame,
            physical_size,
            self.atlas
                .frame(self.player.animation(), self.player.frame_index())
                .with_facing(self.facing),
        );
        self.sprite_dirty = false;
        frame
    }

    /// Paints the bubble even while hidden; native mapping owns visibility.
    #[cfg(test)]
    pub fn render_bubble(&mut self, physical_size: (u32, u32)) -> Vec<u8> {
        let mut frame = vec![0; physical_size.0 as usize * physical_size.1 as usize * 4];
        let preview = self.preview();
        self.renderer
            .compose_bubble(&mut frame, physical_size, &self.latest_view, preview);
        self.bubble_dirty = false;
        frame
    }

    /// Layout and mapping are native concerns; painting never changes sprite geometry.
    #[cfg(test)]
    pub fn render_bubble_layout(
        &mut self,
        physical_size: (u32, u32),
        layout: crate::bubble_layout::BubbleLayout,
    ) -> Vec<u8> {
        let mut frame = vec![0; physical_size.0 as usize * physical_size.1 as usize * 4];
        let preview = self.preview();
        self.renderer.compose_bubble_layout(
            &mut frame,
            physical_size,
            layout,
            self.options.scale,
            (&self.latest_view, preview),
        );
        self.bubble_dirty = false;
        frame
    }

    fn scaled_size(&self, size: (u32, u32)) -> (u32, u32) {
        let scale = |v| (f64::from(v) * self.options.scale).ceil().max(1.0) as u32;
        (scale(size.0), scale(size.1))
    }

    fn preview(&self) -> Option<Animation> {
        if let Some(CornerReturn::Preview { animation, .. }) = self.corner_return {
            return Some(animation);
        }
        self.display.is_manual().then(|| self.player.animation())
    }

    fn select(&mut self, animation: Animation, now: Instant) {
        self.set_facing(selected_facing(animation, self.facing));
        self.sprite_dirty |= self.player.animation() != animation || self.player.frame_index() != 0;
        self.player.select(animation);
        self.selection_index = Animation::ALL
            .iter()
            .position(|&a| a == animation)
            .unwrap_or(0);
        self.last_tick = now;
        self.next_demo = now + DEMO_INTERVAL;
    }

    fn sync_visibility(&mut self, now: Instant) {
        let visible = self.visibility.visible(now);
        self.bubble_dirty |= visible != self.visible;
        self.visible = visible;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        activity::{Cue, Following, Selection},
        assets::{DEFAULT_CHARACTER_ID, character},
        protocol::ActivityState,
    };
    use clap::Parser;

    fn scene(args: &[&str]) -> FamiliarScene {
        let args = [vec!["numen", "run"], args.to_vec()].concat();
        let Some(crate::Command::Run(options)) = crate::Cli::try_parse_from(args).unwrap().command
        else {
            panic!("expected run options");
        };
        FamiliarScene::new(
            "test".into(),
            character(DEFAULT_CHARACTER_ID).unwrap(),
            Arc::new(SpriteAtlas::load().unwrap()),
            options,
            Some(Typeface::bundled().unwrap()),
        )
    }

    fn view(state: ActivityState) -> Presentation {
        Presentation {
            base: Some(Selection {
                source: "test".into(),
                instance: "one".into(),
                activity: "chat".into(),
                state,
                attention: false,
                reason: None,
                summary: None,
            }),
            following: Some(Following {
                source: "test".into(),
                instance: "one".into(),
                activity: "chat".into(),
                summary: Some("Original title".into()),
                available: true,
            }),
            cue: None,
        }
    }

    fn clean(scene: &mut FamiliarScene) {
        scene.render_sprite(scene.sprite_size());
        scene.render_bubble(scene.bubble_size());
        assert!(!scene.sprite_dirty && !scene.bubble_dirty);
    }

    #[test]
    fn facing_only_dirties_sprite_without_restarting_playback_or_bubble() {
        let mut s = scene(&[]);
        let now = s.last_tick;
        s.update(view(ActivityState::Working), now);
        s.tick(now + Duration::from_millis(151));
        clean(&mut s);
        let state = (
            s.player.animation(),
            s.player.frame_index(),
            s.player.until_next_frame(&s.atlas),
            s.last_tick,
            s.next_demo,
            s.selection_index,
            s.preview(),
        );
        let size = s.sprite_size();
        let bubble = s.render_bubble(s.bubble_size());
        let pixels = s.render_sprite(size);
        let facing = match s.facing() {
            Facing::Left => Facing::Right,
            Facing::Right => Facing::Left,
        };
        assert!(s.set_facing(facing));
        assert!(s.sprite_dirty && !s.bubble_dirty);
        assert!(!s.set_facing(facing));
        assert_eq!(
            state,
            (
                s.player.animation(),
                s.player.frame_index(),
                s.player.until_next_frame(&s.atlas),
                s.last_tick,
                s.next_demo,
                s.selection_index,
                s.preview(),
            )
        );
        assert_eq!(s.sprite_size(), size);
        assert_eq!(s.render_bubble(s.bubble_size()), bubble);
        assert_ne!(s.render_sprite(size), pixels);
        s.update(view(ActivityState::Waiting), s.last_tick);
        assert_eq!(s.facing(), facing);
    }

    #[test]
    fn initial_headings_and_explicit_running_selections() {
        let mut s = scene(&[]);
        let mut headings = Vec::new();
        for id in ["djinn", "emerald", "ruby", "hexa-glasses"] {
            let fresh = FamiliarScene::new(
                id.into(),
                s.character,
                s.atlas.clone(),
                s.options.clone(),
                None,
            );
            assert_eq!(fresh.facing(), initial_facing(id));
            headings.push(fresh.facing());
        }
        assert!(headings.contains(&Facing::Left) && headings.contains(&Facing::Right));
        for (animation, facing) in [
            ("running-left", Facing::Left),
            ("running-right", Facing::Right),
        ] {
            let fresh = scene(&["--animation", animation]);
            assert_eq!(fresh.facing(), facing);
        }
        for animation in Animation::ALL {
            s.set_facing(Facing::Right);
            s.select(animation, s.last_tick);
            assert_eq!(
                s.facing(),
                if animation == Animation::RunningLeft {
                    Facing::Left
                } else {
                    Facing::Right
                }
            );
            s.set_facing(Facing::Left);
            s.select(animation, s.last_tick);
            assert_eq!(
                s.facing(),
                if animation == Animation::RunningRight {
                    Facing::Right
                } else {
                    Facing::Left
                }
            );
        }
    }

    #[test]
    fn corner_keeps_travel_heading_except_explicit_running_preview() {
        for mode in [None, Some("--manual"), Some("--demo")] {
            for animation in ["idle", "working", "running-left", "running-right"] {
                let mut args = vec!["--animation", animation];
                if let Some(mode) = mode {
                    args.push(mode);
                }
                let mut s = scene(&args);
                let now = s.last_tick;
                for travel in [Facing::Left, Facing::Right] {
                    s.begin_corner(now);
                    s.face_corner(travel, now);
                    assert_eq!(s.facing(), travel);
                    s.finish_corner(now);
                    let expected = if mode.is_some() {
                        match animation {
                            "running-left" => Facing::Left,
                            "running-right" => Facing::Right,
                            _ => travel,
                        }
                    } else {
                        travel
                    };
                    assert_eq!(s.facing(), expected);
                    assert_eq!(s.display.is_manual(), mode.is_some());
                }
            }
        }
    }

    #[test]
    fn running_left_samples_canonical_right_once_and_keeps_logical_size() {
        let mut s = scene(&[
            "--manual",
            "--animation",
            "running-left",
            "--sprite-scale",
            "1",
            "--scale",
            "1",
        ]);
        assert_eq!(s.sprite_size(), (192, 208));
        let atlas = s.atlas.clone();
        let left = atlas.frame(Animation::RunningLeft, 0);
        let right = atlas.frame(Animation::RunningRight, 0);
        assert_eq!(left.rgba.as_ptr(), right.rgba.as_ptr());
        assert!(!left.mirror_x && !right.mirror_x);
        let painted = s.render_sprite(s.sprite_size());
        for y in 0..208 {
            for x in 0..192 {
                let sx = right.width as usize
                    - 1
                    - (((x as f64 + 0.5) / 192.0) * f64::from(right.width)).floor() as usize;
                let sy = (((y as f64 + 0.5) / 208.0) * f64::from(right.height)).floor() as usize;
                let source = (sy * right.width as usize + sx) * 4;
                let target = (y * 192 + x) * 4;
                assert_eq!(&painted[target..target + 4], &right[source..source + 4]);
            }
        }
    }

    #[test]
    fn every_animation_renders_both_headings_from_hd_source() {
        let mut s = scene(&["--scale", "1", "--sprite-scale", "1"]);
        let atlas = s.atlas.clone();
        for animation in Animation::ALL {
            s.select(animation, s.last_tick);
            for facing in [Facing::Left, Facing::Right] {
                s.set_facing(facing);
                let source = atlas.frame(animation, s.player.frame_index());
                let frame = s.render_sprite(s.sprite_size());
                assert_eq!(frame.len(), 192 * 208 * 4);
                for y in 0..208 {
                    for x in 0..192 {
                        let sx =
                            (((x as f64 + 0.5) / 192.0) * f64::from(source.width)).floor() as usize;
                        let sx = if facing == Facing::Left {
                            source.width as usize - 1 - sx
                        } else {
                            sx
                        };
                        let sy = (((y as f64 + 0.5) / 208.0) * f64::from(source.height)).floor()
                            as usize;
                        let offset = (sy * source.width as usize + sx) * 4;
                        let target = (y * 192 + x) * 4;
                        assert_eq!(&frame[target..target + 4], &source[offset..offset + 4]);
                    }
                }
            }
        }
    }

    #[test]
    fn fractional_split_densities_preserve_settings_dirty_flags_and_atlas_alpha() {
        use crate::raster::{Density, RasterMode, RasterSpec};
        let mut s = scene(&["--scale", "1.375"]);
        s.apply_settings(FamiliarSettings {
            sprite_scale: 0.713,
            ..FamiliarSettings::default()
        })
        .unwrap();
        let sizes = (s.sprite_size(), s.bubble_size());
        let scale = s.options.scale;
        let layout = crate::bubble_layout::BubbleLayout {
            origin: (0, 0),
            size: (301, 149),
            tail: None,
        };
        for (units, facing) in [120, 150, 180, 210, 240]
            .into_iter()
            .flat_map(|units| [Facing::Left, Facing::Right].map(|facing| (units, facing)))
        {
            s.set_facing(facing);
            let raster = RasterSpec::fit(
                sizes.0,
                Density::from_preferred_scale(units).unwrap(),
                RasterMode::ViewportDestination,
            )
            .unwrap();
            s.sprite_dirty = true;
            s.bubble_dirty = true;
            let sprite = s.render_sprite_raster(raster);
            assert!(!s.sprite_dirty && s.bubble_dirty);
            let size = raster.physical_size();
            let atlas = s.atlas.frame(s.player.animation(), s.player.frame_index());
            let width = 192.0 * f64::from(s.options.sprite_scale) * scale;
            let height = 208.0 * f64::from(s.options.sprite_scale) * scale;
            for y in 0..size.1 {
                for x in 0..size.0 {
                    let (lx, ly) =
                        raster.buffer_to_logical((f64::from(x) + 0.5, f64::from(y) + 0.5));
                    let offset = ((y * size.0 + x) * 4) as usize;
                    if lx < width && ly < height {
                        let sx = (lx / width * f64::from(atlas.width)).floor() as usize;
                        let sx = if s.facing() == Facing::Left {
                            atlas.width as usize - 1 - sx
                        } else {
                            sx
                        };
                        let sy = (ly / height * f64::from(atlas.height)).floor() as usize;
                        let source = (sy * atlas.width as usize + sx) * 4;
                        assert_eq!(&sprite[offset..offset + 4], &atlas[source..source + 4]);
                    } else {
                        assert_eq!(&sprite[offset..offset + 4], &[0; 4]);
                    }
                }
            }
            let bubble = RasterSpec::fit(
                layout.size,
                Density::from_preferred_scale(360 - units).unwrap(),
                RasterMode::ViewportDestination,
            )
            .unwrap();
            let frame = s.render_bubble_raster(bubble, layout);
            assert!(!s.bubble_dirty && !s.sprite_dirty);
            assert!(frame.chunks_exact(4).any(|p| p[3] == 255));
            assert_eq!((s.sprite_size(), s.bubble_size()), sizes);
            assert_eq!(s.options.scale, scale);
        }
    }

    #[test]
    fn responsive_layout_reflow_and_reveal_are_independent_of_sprite_size() {
        use crate::bubble_layout::{BubbleLayout, Tail, TailEdge};
        let mut s = scene(&["--scale", "1.375"]);
        let now = s.last_tick;
        let layout = BubbleLayout {
            origin: (100, 200),
            size: (275, 198),
            tail: Some(Tail {
                edge: TailEdge::Bottom,
                position: 100,
            }),
        };
        assert_eq!(s.minimum_bubble_size(), (220, 110));
        let size = (layout.size.0 * 2, layout.size.1 * 2);
        let first = s.render_bubble_layout(size, layout);
        assert!(!s.bubble_dirty);
        let sprite_size = s.sprite_size();
        s.reveal(now);
        assert_eq!(s.sprite_size(), sprite_size);
        assert_eq!(first, s.render_bubble_layout(size, layout));
        s.apply_settings(FamiliarSettings {
            sprite_scale: 2.0,
            ..FamiliarSettings::default()
        })
        .unwrap();
        assert_eq!(first, s.render_bubble_layout(size, layout));
        assert_ne!(s.sprite_size(), sprite_size);
        let new_sprite_size = s.sprite_size();
        let shifted = BubbleLayout {
            tail: Some(Tail {
                edge: TailEdge::Bottom,
                position: 150,
            }),
            ..layout
        };
        assert_ne!(first, s.render_bubble_layout(size, shifted));
        assert_eq!(s.sprite_size(), new_sprite_size);
        s.apply_settings(FamiliarSettings {
            font_size: 32.0,
            ..FamiliarSettings::default()
        })
        .unwrap();
        assert_eq!(s.minimum_bubble_size(), (308, 143));
    }

    #[test]
    fn minimum_height_bubble_keeps_attention_and_failure_visible() {
        let mut s = scene(&["--scale", "1"]);
        let layout = crate::bubble_layout::layout(
            (144, 80),
            (96, 104),
            s.bubble_size(),
            s.minimum_bubble_size(),
            Some((384, 264)),
        )
        .unwrap();
        assert_eq!(layout.size, (384, 80));
        assert_eq!(
            layout.tail.unwrap().edge,
            crate::bubble_layout::TailEdge::Bottom
        );
        let mut waiting = view(ActivityState::Waiting);
        waiting.base.as_mut().unwrap().attention = true;
        s.update(waiting.clone(), s.last_tick);
        let attention = s.render_bubble_layout(layout.size, layout);
        waiting.base.as_mut().unwrap().attention = false;
        s.update(waiting, s.last_tick);
        let dependency = s.render_bubble_layout(layout.size, layout);
        s.update(view(ActivityState::Failed), s.last_tick);
        let failed = s.render_bubble_layout(layout.size, layout);
        assert_ne!(attention, dependency);
        assert_ne!(attention, failed);
        assert_ne!(dependency, failed);
        assert!(!s.bubble_dirty);
    }

    #[test]
    fn independent_sizes_settings_and_reveal_never_resize_sprite() {
        let mut s = scene(&["--scale", "1.375"]);
        let now = s.last_tick;
        assert_eq!(s.sprite_size(), (132, 143));
        assert_eq!(s.bubble_size(), (528, 198));
        clean(&mut s);
        s.reveal(now);
        assert!(s.bubble_visible() && s.bubble_dirty && !s.sprite_dirty);
        assert_eq!(s.sprite_size(), (132, 143));
        clean(&mut s);
        let mut settings = FamiliarSettings::default();
        assert!(!s.apply_settings(settings).unwrap());
        settings.sprite_scale = 1.0;
        assert!(s.apply_settings(settings).unwrap());
        assert!(s.sprite_dirty && !s.bubble_dirty);
        assert_eq!(s.sprite_size(), (264, 286));
        assert_eq!(s.bubble_size(), (528, 198));
        clean(&mut s);
        settings.font_size = 24.0;
        s.apply_settings(settings).unwrap();
        assert!(!s.sprite_dirty && s.bubble_dirty);
        clean(&mut s);
        settings.bubble_duration = 0.25;
        s.apply_settings(settings).unwrap();
        assert!(!s.sprite_dirty && !s.bubble_dirty);
        assert_eq!(s.visibility.deadline(), Some(now + Duration::from_secs(5)));
        settings.font_size = f64::NAN;
        assert!(s.apply_settings(settings).is_err());
        assert_eq!(s.options.font_size, 24.0);
    }

    #[test]
    fn source_metadata_and_cues_use_existing_playback_rules() {
        let mut s = scene(&[]);
        let now = s.last_tick;
        let mut v = view(ActivityState::Working);
        s.update(v.clone(), now);
        assert_eq!(s.player.animation(), Animation::Working);
        s.tick(now + Duration::from_millis(150));
        assert_eq!(s.player.frame_index(), 1);
        clean(&mut s);
        let remaining = s.player.until_next_frame(&s.atlas);
        let last_tick = s.last_tick;
        v.following.as_mut().unwrap().summary = Some("Updated title".into());
        s.update(v.clone(), last_tick);
        assert_eq!(s.player.frame_index(), 1);
        assert_eq!(s.player.until_next_frame(&s.atlas), remaining);
        assert_eq!(s.last_tick, last_tick);
        assert!(!s.sprite_dirty && s.bubble_dirty);
        clean(&mut s);
        s.update(v.clone(), last_tick);
        assert!(!s.sprite_dirty && !s.bubble_dirty);
        let mut outcome = v.base.clone().unwrap();
        outcome.state = ActivityState::Succeeded;
        v.cue = Some(Cue {
            id: 1,
            selection: outcome,
        });
        s.update(v.clone(), last_tick);
        assert_eq!(s.player.animation(), Animation::Succeeded);
        let total: Duration = s
            .atlas
            .clip(Animation::Succeeded)
            .frame_durations
            .iter()
            .copied()
            .sum();
        let end = last_tick + total;
        s.tick(end);
        assert_eq!(s.player.animation(), Animation::Working);
        s.update(v, end);
        assert_eq!(s.player.animation(), Animation::Working);
        s.update(view(ActivityState::Failed), end);
        assert_eq!(
            s.player.animation(),
            Animation::Idle,
            "historical outcomes are not fresh cues"
        );
    }

    #[test]
    fn paused_time_is_discarded_but_bubble_expiry_still_dirties() {
        let mut s = scene(&["--paused"]);
        let now = s.last_tick;
        s.reveal(now);
        clean(&mut s);
        assert_eq!(s.tick(now), Some(now + Duration::from_secs(5)));
        assert!(!s.sprite_dirty && !s.bubble_dirty);
        assert_eq!(s.tick(now + Duration::from_secs(5)), None);
        assert!(!s.bubble_visible() && s.bubble_dirty && !s.sprite_dirty);
        clean(&mut s);
        assert_eq!(s.tick(now + Duration::from_secs(6)), None);
        assert!(!s.bubble_dirty);
        s.toggle_pause(now + Duration::from_secs(10));
        let next = s.tick(now + Duration::from_secs(10)).unwrap();
        assert_eq!(s.player.frame_index(), 0);
        assert_eq!(
            next,
            now + Duration::from_secs(10) + Duration::from_millis(280)
        );
        s.tick(next);
        assert_eq!(s.player.frame_index(), 1);
        assert!(s.sprite_dirty);
    }

    #[test]
    fn subframe_ticks_and_visibility_policies_do_not_dirty_unrelated_buffers() {
        for args in [vec![], vec!["--no-bubble"], vec!["--always-bubble"]] {
            let mut s = scene(&args);
            let now = s.last_tick;
            clean(&mut s);
            let next = s.tick(now + Duration::from_millis(1));
            assert_eq!(next, Some(now + Duration::from_millis(280)));
            assert!(!s.sprite_dirty && !s.bubble_dirty);
            s.reveal(now);
            assert_eq!(s.bubble_visible(), !s.options.no_bubble);
            assert!(!s.sprite_dirty);
            let bubble = s.render_bubble(s.bubble_size());
            assert_eq!(&bubble[..4], &[0; 4]);
            assert!(bubble.chunks_exact(4).any(|p| p[3] != 0));
            let end = now + s.options.bubble_duration;
            s.tick(end);
            assert_eq!(s.bubble_visible(), s.options.always_bubble);
            assert_eq!(
                s.bubble_dirty,
                !s.options.no_bubble && !s.options.always_bubble
            );
        }
    }

    #[test]
    fn menu_pause_is_idempotent_and_resume_discards_paused_elapsed_time() {
        let mut s = scene(&[]);
        let now = s.last_tick;
        s.set_paused(false, now + Duration::from_secs(1));
        assert_eq!(s.last_tick, now);
        let paused = now + Duration::from_millis(100);
        s.set_paused(true, paused);
        s.set_paused(true, paused + Duration::from_secs(1));
        assert_eq!(s.last_tick, paused);
        let resumed = paused + Duration::from_secs(30);
        s.set_paused(false, resumed);
        assert!(!s.options.paused);
        assert_eq!(s.last_tick, resumed);
        s.tick(resumed);
        assert_eq!(s.player.frame_index(), 0);
    }

    #[test]
    fn manual_and_demo_keep_preview_and_separate_selection_cursor() {
        for args in [vec!["--manual"], vec!["--demo"]] {
            let mut s = scene(&args);
            let now = s.last_tick;
            s.update(view(ActivityState::Working), now);
            assert_eq!(s.player.animation(), Animation::Idle);
            assert!(s.preview().is_some());
            s.cycle(now);
            assert_eq!(s.player.animation(), Animation::Working);
            if s.options.demo {
                for index in 2..=3 {
                    s.tick(now + DEMO_INTERVAL * (index - 1));
                    assert_eq!(s.player.animation(), Animation::ALL[index as usize]);
                }
                // Succeeded finishes before the demo interval; the cursor must still advance to Failed.
                let end: Duration = s
                    .atlas
                    .clip(Animation::Succeeded)
                    .frame_durations
                    .iter()
                    .copied()
                    .sum();
                s.tick(now + DEMO_INTERVAL * 2 + end);
                assert_eq!(s.player.animation(), Animation::Idle);
                s.tick(now + DEMO_INTERVAL * 3);
                assert_eq!(s.player.animation(), Animation::Failed);
            }
        }
    }

    #[test]
    fn corner_source_updates_restore_latest_base_without_replacing_running_pose() {
        for attention in [false, true] {
            let mut s = scene(&[]);
            let now = s.last_tick;
            s.update(view(ActivityState::Working), now);
            clean(&mut s);
            let deadline = s.next_demo;
            s.begin_corner(now + Duration::from_millis(10));
            assert!(s.corner_active());
            assert_eq!(s.player.animation(), Animation::Working);
            assert_eq!(s.last_tick, now);
            assert_eq!(s.next_demo, deadline);
            assert!(!s.sprite_dirty && !s.bubble_dirty);
            s.face_corner(crate::motion::Facing::Left, now);
            assert_eq!(s.player.animation(), Animation::RunningLeft);
            clean(&mut s);
            let mut waiting = view(ActivityState::Waiting);
            waiting.base.as_mut().unwrap().attention = attention;
            s.update(waiting.clone(), now + Duration::from_millis(20));
            assert_eq!(s.player.animation(), Animation::RunningLeft);
            assert_eq!(s.latest_view, waiting);
            assert!(!s.sprite_dirty && s.bubble_dirty);
            assert!(!s.display.is_manual());
            assert_eq!(s.last_tick, now);
            s.finish_corner(now + Duration::from_millis(40));
            assert!(!s.corner_active());
            assert_eq!(
                s.player.animation(),
                if attention {
                    Animation::Waiting
                } else {
                    Animation::Review
                }
            );
            let last_tick = s.last_tick;
            clean(&mut s);
            s.finish_corner(now + Duration::from_secs(1));
            s.face_corner(crate::motion::Facing::Right, now + Duration::from_secs(1));
            assert_eq!(s.last_tick, last_tick);
            assert!(!s.sprite_dirty && !s.bubble_dirty);
        }
    }

    #[test]
    fn corner_consumes_terminal_cues_without_replay_or_arrival_celebration() {
        for state in [ActivityState::Succeeded, ActivityState::Failed] {
            let mut s = scene(&[]);
            let now = s.last_tick;
            s.update(view(ActivityState::Working), now);
            s.begin_corner(now);
            s.face_corner(crate::motion::Facing::Left, now);
            let mut terminal = view(state);
            terminal.cue = Some(Cue {
                id: 7,
                selection: terminal.base.clone().unwrap(),
            });
            s.update(terminal.clone(), now);
            s.tick(now + Duration::from_secs(1));
            assert_eq!(s.player.animation(), Animation::RunningLeft);
            s.finish_corner(now + Duration::from_secs(1));
            assert_eq!(s.player.animation(), Animation::Idle);
            // Force a new presentation with the same consumed cue, not the scene's no-op path.
            terminal.following.as_mut().unwrap().summary = Some("Updated outcome".into());
            s.update(terminal, now + Duration::from_secs(1));
            assert_eq!(s.player.animation(), Animation::Idle);
            s.tick(now + Duration::from_secs(2));
            assert_eq!(s.player.animation(), Animation::Idle);
        }
    }

    #[test]
    fn repeated_corner_facing_and_requests_preserve_frame_time_and_return_pose() {
        let mut s = scene(&["--manual", "--animation", "working"]);
        let now = s.last_tick;
        let cursor = s.selection_index;
        let demo = s.next_demo;
        s.begin_corner(now);
        s.face_corner(crate::motion::Facing::Left, now);
        let mut expected = Player::new(Animation::RunningLeft);
        let step = Duration::from_millis(17);
        for index in 1..=30 {
            let at = now + step * index;
            s.begin_corner(at);
            s.face_corner(crate::motion::Facing::Left, at);
            expected.advance(step, &s.atlas);
            let deadline = s.tick(at);
            assert_eq!(s.player.frame_index(), expected.frame_index());
            assert_eq!(
                s.player.until_next_frame(&s.atlas),
                expected.until_next_frame(&s.atlas)
            );
            assert_eq!(deadline, Some(at + expected.until_next_frame(&s.atlas)));
            // A second event at the same instant must not advance the clip twice.
            assert_eq!(s.tick(at), deadline);
            assert_eq!(s.player.frame_index(), expected.frame_index());
            assert_eq!(s.next_demo, demo);
            assert_eq!(s.selection_index, cursor);
        }
        clean(&mut s);
        let at = now + step * 30;
        s.face_corner(crate::motion::Facing::Right, at);
        assert_eq!(s.player.animation(), Animation::RunningRight);
        assert_eq!(s.player.frame_index(), 0);
        assert!(s.sprite_dirty && !s.bubble_dirty);
        s.begin_corner(at);
        s.finish_corner(at);
        assert_eq!(s.player.animation(), Animation::Working);
        assert_eq!(s.selection_index, cursor);
        assert_eq!(s.preview(), Some(Animation::Working));
        assert!(s.display.is_manual());
        assert_eq!(s.options.animation, Animation::Working);
        assert!(!s.bubble_dirty);
    }

    #[test]
    fn corner_preserves_paused_preview_options_and_independent_bubble_expiry() {
        for mode in ["--manual", "--demo"] {
            let mut s = scene(&[
                mode,
                "--animation",
                "review",
                "--scale",
                "1.375",
                "--opaque",
                "--normal-window-level",
            ]);
            let now = s.last_tick;
            s.set_paused(true, now);
            s.cycle(now);
            let animation = s.player.animation();
            let cursor = s.selection_index;
            let sizes = (s.sprite_size(), s.bubble_size(), s.minimum_bubble_size());
            let options = s.options.clone();
            s.reveal(now);
            let bubble = s.render_bubble(s.bubble_size());
            s.begin_corner(now);
            s.face_corner(crate::motion::Facing::Left, now);
            assert_eq!(s.preview(), Some(animation));
            assert_eq!(s.render_bubble(s.bubble_size()), bubble);
            clean(&mut s);
            assert_eq!(s.tick(now), Some(now + Duration::from_secs(5)));
            assert_eq!(s.tick(now + Duration::from_secs(5)), None);
            assert_eq!(s.player.animation(), Animation::RunningLeft);
            assert_eq!(s.player.frame_index(), 0);
            assert!(!s.sprite_dirty && s.bubble_dirty && !s.bubble_visible());
            s.update(view(ActivityState::Working), now + Duration::from_secs(5));
            s.finish_corner(now + Duration::from_secs(5));
            assert_eq!(s.player.animation(), animation);
            assert_eq!(s.preview(), Some(animation));
            assert_eq!(s.selection_index, cursor);
            assert!(s.display.is_manual());
            assert_eq!(
                (
                    s.options.manual,
                    s.options.demo,
                    s.options.paused,
                    s.options.animation
                ),
                (
                    options.manual,
                    options.demo,
                    options.paused,
                    options.animation
                )
            );
            assert_eq!(
                (
                    s.options.scale,
                    s.options.sprite_scale,
                    s.options.font_size,
                    s.options.bubble_duration
                ),
                (
                    options.scale,
                    options.sprite_scale,
                    options.font_size,
                    options.bubble_duration
                )
            );
            assert_eq!(
                (
                    s.options.opaque,
                    s.options.normal_window_level,
                    s.options.no_bubble,
                    s.options.always_bubble
                ),
                (
                    options.opaque,
                    options.normal_window_level,
                    options.no_bubble,
                    options.always_bubble
                )
            );
            assert_eq!(
                (s.sprite_size(), s.bubble_size(), s.minimum_bubble_size()),
                sizes
            );
        }
    }

    #[test]
    fn corner_suppresses_demo_and_settings_do_not_cancel_ownership() {
        let mut s = scene(&["--demo", "--animation", "succeeded"]);
        let now = s.last_tick;
        let cursor = s.selection_index;
        s.tick(now + Duration::from_secs(1));
        assert_eq!(s.player.animation(), Animation::Idle);
        s.begin_corner(now + Duration::from_secs(1));
        s.face_corner(crate::motion::Facing::Right, now + Duration::from_secs(1));
        s.apply_settings(FamiliarSettings {
            sprite_scale: 2.0,
            font_size: 24.0,
            bubble_duration: 0.25,
        })
        .unwrap();
        assert!(s.corner_active());
        let at = now + Duration::from_secs(10);
        let deadline = s.tick(at).unwrap();
        assert!(deadline > at, "overdue demo must not cause a busy loop");
        assert_eq!(s.player.animation(), Animation::RunningRight);
        assert_eq!(s.selection_index, cursor);
        s.finish_corner(at);
        assert_eq!(s.player.animation(), Animation::Idle);
        assert_eq!(s.selection_index, cursor);
        assert_eq!(s.options.animation, Animation::Succeeded);
        assert!(s.options.demo && s.display.is_manual());
        assert_eq!(s.options.sprite_scale, 2.0);
        assert_eq!(s.options.font_size, 24.0);
        assert_eq!(s.options.bubble_duration, Duration::from_secs_f64(0.25));
        s.tick(at + DEMO_INTERVAL);
        assert_eq!(s.player.animation(), Animation::Failed);
    }
}
