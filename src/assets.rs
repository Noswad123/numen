use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::animation::Animation;
use crate::sprite::{LOGICAL_HEIGHT, LOGICAL_WIDTH, SpriteFrame};

pub const CELL_WIDTH: u32 = LOGICAL_WIDTH;
pub const CELL_HEIGHT: u32 = LOGICAL_HEIGHT;
// Deterministic manual/demo default, independent of activity producer source IDs.
pub const DEFAULT_CHARACTER_ID: &str = "amethyst";
pub const DEFAULT_CHARACTER_NAME: &str = "Amethyst";
pub const RANDOM_PRESET: &str = "random";
const MANIFEST: &str = include_str!("../assets/djinn/manifest.json");
const SPRITESHEET: &[u8] = include_bytes!("../assets/djinn/spritesheet.png");

pub struct Character {
    pub id: &'static str,
    pub name: &'static str,
    manifest: &'static str,
    png: &'static [u8],
}

pub const CHARACTERS: &[Character] = &[
    Character {
        id: DEFAULT_CHARACTER_ID,
        name: DEFAULT_CHARACTER_NAME,
        manifest: MANIFEST,
        png: SPRITESHEET,
    },
    Character {
        id: "ruby",
        name: "Ruby",
        manifest: include_str!("../assets/ruby/manifest.json"),
        png: include_bytes!("../assets/ruby/spritesheet.png"),
    },
    Character {
        id: "saphire",
        name: "Saphire",
        manifest: include_str!("../assets/hexa-glasses/manifest.json"),
        png: include_bytes!("../assets/hexa-glasses/spritesheet.png"),
    },
    Character {
        id: "emerald",
        name: "Emerald",
        manifest: include_str!("../assets/emerald/manifest.json"),
        png: include_bytes!("../assets/emerald/spritesheet.png"),
    },
];

pub fn character(id: &str) -> Option<&'static Character> {
    CHARACTERS.iter().find(|character| character.id == id)
}

pub struct Clip {
    pub frame_durations: Vec<Duration>,
    pub looping: bool,
    frames: Arc<[Vec<u8>]>,
}

#[derive(Clone)]
pub struct SpriteAtlas {
    clips: Arc<HashMap<Animation, Clip>>,
    cell_width: u32,
    cell_height: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    width: u32,
    height: u32,
    cell_width: u32,
    cell_height: u32,
    columns: u32,
    rows: u32,
    #[serde(deserialize_with = "deserialize_clips")]
    clips: HashMap<Animation, ClipMetadata>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipMetadata {
    row: Option<u32>,
    source: Option<Animation>,
    frame_durations_ms: Vec<u64>,
    looping: bool,
}

// A normal HashMap deserializer silently overwrites duplicate JSON keys.
fn deserialize_clips<'de, D>(
    deserializer: D,
) -> std::result::Result<HashMap<Animation, ClipMetadata>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct ClipsVisitor;
    impl<'de> serde::de::Visitor<'de> for ClipsVisitor {
        type Value = HashMap<Animation, ClipMetadata>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a map of unique semantic animation names to clips")
        }

        fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
        where
            M: serde::de::MapAccess<'de>,
        {
            let mut clips = HashMap::new();
            while let Some((animation, clip)) = map.next_entry::<Animation, ClipMetadata>()? {
                if clips.insert(animation, clip).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate clip: {}",
                        animation.label()
                    )));
                }
            }
            Ok(clips)
        }
    }
    deserializer.deserialize_map(ClipsVisitor)
}

impl Manifest {
    fn parse(json: &str) -> Result<Self> {
        let manifest: Self = serde_json::from_str(json).context("invalid sprite manifest JSON")?;
        ensure!(
            (
                manifest.width,
                manifest.height,
                manifest.cell_width,
                manifest.cell_height
            ) == (1536, 2288, CELL_WIDTH, CELL_HEIGHT)
                || (
                    manifest.width,
                    manifest.height,
                    manifest.cell_width,
                    manifest.cell_height
                ) == (3072, 4576, CELL_WIDTH * 2, CELL_HEIGHT * 2),
            "invalid atlas dimensions"
        );
        ensure!(
            manifest.columns == 8 && manifest.rows == 11,
            "invalid atlas grid"
        );
        let mut rows = HashSet::new();
        for animation in Animation::ALL {
            let clip = manifest
                .clips
                .get(&animation)
                .with_context(|| format!("missing clip: {}", animation.label()))?;
            match (clip.row, clip.source) {
                (Some(row), None) => {
                    ensure!(row < manifest.rows, "invalid row for {}", animation.label());
                    ensure!(
                        rows.insert(row),
                        "duplicate atlas row for {}",
                        animation.label()
                    );
                }
                (None, Some(Animation::RunningRight)) if animation == Animation::RunningLeft => {
                    let source = manifest
                        .clips
                        .get(&Animation::RunningRight)
                        .context("missing running-right source")?;
                    ensure!(
                        source.row.is_some() && source.source.is_none(),
                        "alias must target a concrete clip"
                    );
                }
                _ => anyhow::bail!("invalid row/source for {}", animation.label()),
            }
            ensure!(
                !clip.frame_durations_ms.is_empty()
                    && clip.frame_durations_ms.len() <= manifest.columns as usize,
                "invalid frame count for {}",
                animation.label()
            );
            ensure!(
                clip.frame_durations_ms.iter().all(|&ms| ms > 0),
                "nonpositive timing for {}",
                animation.label()
            );
            let should_loop = !matches!(
                animation,
                Animation::Waving | Animation::Succeeded | Animation::Failed
            );
            ensure!(
                clip.looping == should_loop,
                "invalid looping policy for {}",
                animation.label()
            );
        }
        let left = &manifest.clips[&Animation::RunningLeft];
        let right = &manifest.clips[&Animation::RunningRight];
        ensure!(
            left.frame_durations_ms == right.frame_durations_ms && left.looping == right.looping,
            "running-left timing must match canonical running-right exactly"
        );
        Ok(manifest)
    }
}

impl SpriteAtlas {
    pub fn load() -> Result<Self> {
        Self::load_character(DEFAULT_CHARACTER_ID)
    }

    pub fn load_character(id: &str) -> Result<Self> {
        let character = character(id).with_context(|| format!("unknown character: {id}"))?;
        // Only the four immutable bundled characters enter this bounded cache.
        // Serialize first decodes too, avoiding concurrent full-sheet allocations.
        static CACHE: OnceLock<Mutex<HashMap<&'static str, SpriteAtlas>>> = OnceLock::new();
        let mut cache = CACHE
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| anyhow::anyhow!("sprite cache poisoned"))?;
        if let Some(atlas) = cache.get(character.id) {
            return Ok(atlas.clone());
        }
        let atlas = Self::from_bytes(character.manifest, character.png)
            .with_context(|| format!("cannot load character: {id}"))?;
        cache.insert(character.id, atlas.clone());
        Ok(atlas)
    }

    fn from_bytes(json: &str, png: &[u8]) -> Result<Self> {
        let manifest = Manifest::parse(json)?;
        let mut reader = image::ImageReader::with_format(Cursor::new(png), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(manifest.width);
        limits.max_image_height = Some(manifest.height);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .context("cannot decode embedded spritesheet")?
            .into_rgba8();
        ensure!(
            image.dimensions() == (manifest.width, manifest.height),
            "embedded PNG dimensions do not match manifest"
        );
        let mut clips = HashMap::new();
        let mut canonical = HashMap::<Animation, Arc<[Vec<u8>]>>::new();
        for animation in Animation::ALL {
            // Legacy row 2 is already left-facing. Validate its metadata, but
            // never store pre-oriented pixels: every caller applies facing once.
            if animation == Animation::RunningLeft {
                continue;
            }
            let metadata = &manifest.clips[&animation];
            let Some(row) = metadata.row else { continue };
            let frames = (0..metadata.frame_durations_ms.len())
                .map(|column| {
                    image::imageops::crop_imm(
                        &image,
                        column as u32 * manifest.cell_width,
                        row * manifest.cell_height,
                        manifest.cell_width,
                        manifest.cell_height,
                    )
                    .to_image()
                    .into_raw()
                })
                .collect::<Vec<_>>();
            canonical.insert(animation, frames.into());
        }
        for animation in Animation::ALL {
            let metadata = &manifest.clips[&animation];
            let source = if animation == Animation::RunningLeft {
                Animation::RunningRight
            } else {
                animation
            };
            clips.insert(
                animation,
                Clip {
                    frame_durations: metadata
                        .frame_durations_ms
                        .iter()
                        .map(|&ms| Duration::from_millis(ms))
                        .collect(),
                    looping: metadata.looping,
                    frames: Arc::clone(&canonical[&source]),
                },
            );
        }
        Ok(Self {
            clips: Arc::new(clips),
            cell_width: manifest.cell_width,
            cell_height: manifest.cell_height,
        })
    }

    /// `index` must be in `0..clip(animation).frame_durations.len()`.
    pub fn frame(&self, animation: Animation, index: usize) -> SpriteFrame<'_> {
        SpriteFrame::new(
            &self.clips[&animation].frames[index],
            self.cell_width,
            self.cell_height,
        )
    }

    pub fn source_cell_size(&self) -> (u32, u32) {
        (self.cell_width, self.cell_height)
    }

    pub fn clip(&self, animation: Animation) -> &Clip {
        &self.clips[&animation]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_character_identity() {
        assert_eq!(DEFAULT_CHARACTER_ID, "amethyst");
        assert_eq!(DEFAULT_CHARACTER_NAME, "Amethyst");
        let default = character(DEFAULT_CHARACTER_ID).unwrap();
        assert_eq!(default.name, DEFAULT_CHARACTER_NAME);
        let atlas = SpriteAtlas::load().unwrap();
        let explicit = SpriteAtlas::load_character(DEFAULT_CHARACTER_ID).unwrap();
        for animation in Animation::ALL {
            assert_eq!(atlas.frame(animation, 0), explicit.frame(animation, 0));
        }
    }

    #[test]
    fn catalog_identity_and_lookup() {
        assert_eq!(RANDOM_PRESET, "random");
        assert_eq!(
            CHARACTERS
                .iter()
                .map(|c| (c.id, c.name))
                .collect::<Vec<_>>(),
            [
                ("amethyst", "Amethyst"),
                ("ruby", "Ruby"),
                ("saphire", "Saphire"),
                ("emerald", "Emerald")
            ]
        );
        let mut ids = HashSet::new();
        for entry in CHARACTERS {
            assert!(ids.insert(entry.id));
            assert_eq!(character(entry.id).unwrap().name, entry.name);
        }
        for id in [
            "",
            "unknown",
            "Ruby",
            "djinn",
            "hexa-glasses",
            "sapphire",
            RANDOM_PRESET,
        ] {
            assert!(character(id).is_none());
            let error = SpriteAtlas::load_character(id).err().unwrap();
            assert!(error.to_string().contains("unknown character:"));
        }
    }

    #[test]
    fn exact_manifest_rows_timings_and_cached_rgba() {
        for character in CHARACTERS {
            let manifest = Manifest::parse(character.manifest).unwrap();
            let atlas = SpriteAtlas::load_character(character.id).unwrap();
            let decoded = image::load_from_memory(character.png).unwrap();
            assert_eq!(decoded.color(), image::ColorType::Rgba8);
            let source = decoded.into_rgba8();
            assert_eq!(source.dimensions(), (3072, 4576));
            assert_eq!(atlas.source_cell_size(), (384, 416));
            assert_eq!((CELL_WIDTH, CELL_HEIGHT), (192, 208));
            let mut unique_frames = HashSet::new();
            let mut semantic_frames = 0;
            let expected: [(Animation, u32, &[u64]); 9] = [
                (Animation::Idle, 0, &[280, 110, 110, 140, 140, 320]),
                (
                    Animation::RunningRight,
                    1,
                    &[120, 120, 120, 120, 120, 120, 120, 220],
                ),
                (
                    Animation::RunningLeft,
                    1,
                    &[120, 120, 120, 120, 120, 120, 120, 220],
                ),
                (Animation::Waving, 3, &[140, 140, 140, 280]),
                (Animation::Succeeded, 4, &[140, 140, 140, 140, 280]),
                (
                    Animation::Failed,
                    5,
                    &[140, 140, 140, 140, 140, 140, 140, 240],
                ),
                (Animation::Waiting, 6, &[150, 150, 150, 150, 150, 260]),
                (Animation::Working, 7, &[120, 120, 120, 120, 120, 220]),
                (Animation::Review, 8, &[150, 150, 150, 150, 150, 280]),
            ];
            for (animation, row, timings) in expected {
                let metadata = &manifest.clips[&animation];
                if animation == Animation::RunningLeft {
                    assert_eq!(metadata.row, None);
                    assert_eq!(metadata.source, Some(Animation::RunningRight));
                } else {
                    assert_eq!(metadata.row, Some(row));
                    assert_eq!(metadata.source, None);
                }
                let clip = atlas.clip(animation);
                assert_eq!(
                    clip.looping,
                    !matches!(
                        animation,
                        Animation::Waving | Animation::Succeeded | Animation::Failed
                    )
                );
                assert_eq!(
                    clip.frame_durations,
                    timings
                        .iter()
                        .map(|&ms| Duration::from_millis(ms))
                        .collect::<Vec<_>>()
                );
                assert_eq!(clip.frames.len(), timings.len());
                for index in 0..timings.len() {
                    let frame = atlas.frame(animation, index);
                    assert_eq!((frame.width, frame.height), (384, 416));
                    assert!(!frame.mirror_x);
                    assert_eq!(frame.len(), 638976);
                    unique_frames.insert(frame.rgba.as_ptr());
                    semantic_frames += 1;
                    let expected =
                        image::imageops::crop_imm(&source, index as u32 * 384, row * 416, 384, 416)
                            .to_image();
                    assert_eq!(frame.rgba, expected.as_raw());
                    assert_eq!(
                        frame.rgba.as_ptr(),
                        atlas.frame(animation, index).rgba.as_ptr()
                    );
                    if animation == Animation::RunningLeft {
                        assert_eq!(
                            frame.rgba.as_ptr(),
                            atlas.frame(Animation::RunningRight, index).rgba.as_ptr()
                        );
                    }
                    assert!(frame.as_chunks::<4>().0.iter().any(|pixel| pixel[3] == 0));
                    assert!(frame.as_chunks::<4>().0.iter().any(|pixel| pixel[3] == 255));
                    assert!(
                        frame
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .any(|pixel| pixel[3] > 0 && pixel[3] < 255)
                    );
                }
            }
            assert_eq!(semantic_frames, 57);
            assert_eq!(unique_frames.len(), 49);
            let retired = image::imageops::crop_imm(&source, 0, 832, 3072, 416).to_image();
            assert!(retired.as_raw().iter().all(|&byte| byte == 0));
            assert!(
                atlas
                    .frame(Animation::Idle, 0)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] == 0)
            );
            assert!(
                atlas
                    .frame(Animation::Idle, 0)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] > 0)
            );
        }
    }

    #[test]
    fn rejects_invalid_metadata() {
        let valid: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        for (path, replacement) in [
            (vec!["width"], serde_json::json!(1)),
            (vec!["height"], serde_json::json!(1)),
            (vec!["cell_width"], serde_json::json!(0)),
            (vec!["cell_height"], serde_json::json!(0)),
            (vec!["columns"], serde_json::json!(9)),
            (vec!["rows"], serde_json::json!(12)),
            (vec!["clips", "idle", "row"], serde_json::json!(11)),
            (vec!["clips", "idle", "row"], serde_json::json!(-1)),
            (vec!["clips", "idle", "row"], serde_json::json!(1)),
            (
                vec!["clips", "idle", "frame_durations_ms"],
                serde_json::json!([]),
            ),
            (
                vec!["clips", "idle", "frame_durations_ms"],
                serde_json::json!([1, 1, 1, 1, 1, 1, 1, 1, 1]),
            ),
            (
                vec!["clips", "idle", "frame_durations_ms"],
                serde_json::json!([0]),
            ),
            (
                vec!["clips", "idle", "frame_durations_ms"],
                serde_json::json!([-1]),
            ),
            (vec!["clips", "idle", "looping"], serde_json::json!(false)),
            (vec!["clips", "waving", "looping"], serde_json::json!(true)),
        ] {
            let mut invalid = valid.clone();
            let mut target = &mut invalid;
            for key in &path {
                target = &mut target[*key];
            }
            *target = replacement;
            assert!(
                Manifest::parse(&invalid.to_string()).is_err(),
                "accepted {path:?}"
            );
        }
        let mut missing = valid.clone();
        missing["clips"].as_object_mut().unwrap().remove("idle");
        assert!(Manifest::parse(&missing.to_string()).is_err());
        let mut unknown = valid.clone();
        unknown["clips"]["jumping"] = unknown["clips"]["succeeded"].clone();
        assert!(Manifest::parse(&unknown.to_string()).is_err());
        let duplicate = MANIFEST.replacen(
            "\"idle\":",
            "\"idle\": {\"row\": 0, \"frame_durations_ms\": [1], \"looping\": true}, \"idle\":",
            1,
        );
        assert!(Manifest::parse(&duplicate).is_err());
        assert!(Manifest::parse("not json").is_err());
    }

    #[test]
    fn rejects_bad_png_and_dimension_mismatch() {
        assert!(SpriteAtlas::from_bytes(MANIFEST, b"not a PNG").is_err());
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(1, 1)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        assert!(SpriteAtlas::from_bytes(MANIFEST, encoded.get_ref()).is_err());
    }

    #[test]
    fn rejects_invalid_aliases_without_relaxing_metadata_policies() {
        let valid: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        for replacement in [
            serde_json::json!({"row": 2, "source": "running-right", "frame_durations_ms": [120,120,120,120,120,120,120,220], "looping": true}),
            serde_json::json!({"frame_durations_ms": [120], "looping": true}),
            serde_json::json!({"source": "idle", "frame_durations_ms": [120], "looping": true}),
            serde_json::json!({"source": "running-left", "frame_durations_ms": [120], "looping": true}),
            serde_json::json!({"source": "running-right", "frame_durations_ms": [120], "looping": true}),
            serde_json::json!({"source": "running-right", "frame_durations_ms": [120,120,120,120,120,120,120,221], "looping": true}),
            serde_json::json!({"source": "running-right", "frame_durations_ms": [120,120,120,120,120,120,120,220], "looping": false}),
            serde_json::json!({"row": 11, "frame_durations_ms": [120], "looping": true}),
        ] {
            let mut invalid = valid.clone();
            invalid["clips"]["running-left"] = replacement;
            assert!(Manifest::parse(&invalid.to_string()).is_err());
        }
        let mut cycle = valid.clone();
        cycle["clips"]["running-right"]
            .as_object_mut()
            .unwrap()
            .remove("row");
        cycle["clips"]["running-right"]["source"] = serde_json::json!("running-left");
        assert!(Manifest::parse(&cycle.to_string()).is_err());
        let mut unsupported = valid;
        unsupported["clips"]["idle"]
            .as_object_mut()
            .unwrap()
            .remove("row");
        unsupported["clips"]["idle"]["source"] = serde_json::json!("running-right");
        assert!(Manifest::parse(&unsupported.to_string()).is_err());
    }

    #[test]
    fn synthetic_1x_concrete_rows_normalize_without_double_mirroring() {
        use crate::{
            bubble::{Renderer, SpriteProjection},
            motion::Facing,
        };
        use clap::Parser;

        // Format compatibility must not require archived artwork to remain bundled.
        let mut legacy: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        for key in ["width", "height", "cell_width", "cell_height"] {
            legacy[key] = serde_json::json!(legacy[key].as_u64().unwrap() / 2);
        }
        legacy["clips"]["running-left"]
            .as_object_mut()
            .unwrap()
            .remove("source");
        legacy["clips"]["running-left"]["row"] = serde_json::json!(2);
        let json = legacy.to_string();
        let source = image::RgbaImage::from_fn(1536, 2288, |x, y| {
            let (sx, sy, row, column) = (x % 192, y % 208, y / 208, x / 192);
            image::Rgba([
                sx as u8,
                sy as u8,
                (row * 31 + column * 9) as u8,
                ((sx * 13 + sy * 7 + row * 11 + column * 5) % 256) as u8,
            ])
        });
        let mut png = std::io::Cursor::new(Vec::new());
        source.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let atlas = Arc::new(SpriteAtlas::from_bytes(&json, png.get_ref()).unwrap());
        assert_eq!(atlas.source_cell_size(), (192, 208));
        let metadata = Manifest::parse(&json).unwrap();
        assert_eq!(metadata.clips[&Animation::RunningLeft].row, Some(2));
        let mut unique_frames = HashSet::new();
        let mut semantic_frames = 0;
        for animation in Animation::ALL {
            for i in 0..atlas.clip(animation).frame_durations.len() {
                let canonical = if animation == Animation::RunningLeft {
                    Animation::RunningRight
                } else {
                    animation
                };
                let expected = image::imageops::crop_imm(
                    &source,
                    i as u32 * 192,
                    metadata.clips[&canonical].row.unwrap() * 208,
                    192,
                    208,
                )
                .to_image();
                assert_eq!(atlas.frame(animation, i).rgba, expected.as_raw());
                assert!(!atlas.frame(animation, i).mirror_x);
                unique_frames.insert(atlas.frame(animation, i).rgba.as_ptr());
                semantic_frames += 1;
                if animation == Animation::RunningLeft {
                    assert_eq!(
                        atlas.frame(animation, i).rgba.as_ptr(),
                        atlas.frame(Animation::RunningRight, i).rgba.as_ptr()
                    );
                }
            }
        }
        assert_eq!(semantic_frames, 57);
        assert_eq!(unique_frames.len(), 49);

        let mut renderer = Renderer::new(None, 12.0);
        let size = (192, 208);
        let projection = SpriteProjection::new(size, false, 1.0);
        for (animation, facing) in [
            (Animation::RunningLeft, Facing::Left),
            (Animation::RunningRight, Facing::Right),
        ] {
            let Some(crate::Command::Run(options)) = crate::Cli::try_parse_from([
                "numen",
                "run",
                "--manual",
                "--animation",
                animation.label(),
                "--scale",
                "1",
                "--sprite-scale",
                "1",
                "--no-bubble",
            ])
            .unwrap()
            .command
            else {
                panic!("expected run options")
            };
            let mut scene = crate::desktop_scene::FamiliarScene::new(
                "legacy-fixture".into(),
                character(DEFAULT_CHARACTER_ID).unwrap(),
                Arc::clone(&atlas),
                options,
                None,
            );
            assert_eq!(scene.sprite_size(), size);
            assert_eq!(scene.facing(), facing);
            for i in 0..atlas.clip(animation).frame_durations.len() {
                // Independent reference from synthetic row 1, not SpriteFrame sampling.
                let mut expected = vec![0; 192 * 208 * 4];
                for y in 0..208 {
                    for x in 0..192 {
                        let sx = if facing == Facing::Left { 191 - x } else { x };
                        let pixel = source.get_pixel(i as u32 * 192 + sx, 208 + y);
                        let target = ((y * 192 + x) * 4) as usize;
                        expected[target..target + 4].copy_from_slice(&pixel.0);
                    }
                }
                let frame = atlas.frame(animation, i).with_facing(facing);
                let mut rendered = vec![0; expected.len()];
                renderer.compose_sprite(&mut rendered, size, frame);
                assert_eq!(rendered, expected);
                renderer.compose(
                    &mut rendered,
                    size,
                    frame,
                    &crate::activity::Presentation::default(),
                    None,
                    false,
                );
                assert_eq!(rendered, expected);
                assert_eq!(
                    projection.alpha(frame),
                    expected
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|p| p[3])
                        .collect::<Vec<_>>()
                );
                if i == 0 {
                    assert_eq!(scene.render_sprite(size), expected);
                    let raster = crate::raster::RasterSpec::from_sizes(size, size).unwrap();
                    assert_eq!(scene.render_sprite_raster(raster), expected);
                }
            }
        }
    }

    #[test]
    fn rejects_incompatible_legacy_concrete_left_metadata() {
        let mut legacy: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        for key in ["width", "height", "cell_width", "cell_height"] {
            legacy[key] = serde_json::json!(legacy[key].as_u64().unwrap() / 2);
        }
        legacy["clips"]["running-left"]
            .as_object_mut()
            .unwrap()
            .remove("source");
        legacy["clips"]["running-left"]["row"] = serde_json::json!(2);
        assert!(Manifest::parse(&legacy.to_string()).is_ok());
        for (key, value) in [
            ("frame_durations_ms", serde_json::json!([120])),
            (
                "frame_durations_ms",
                serde_json::json!([120, 120, 120, 120, 120, 120, 120, 221]),
            ),
            ("looping", serde_json::json!(false)),
            ("row", serde_json::json!(1)),
            ("row", serde_json::json!(11)),
        ] {
            let mut invalid = legacy.clone();
            invalid["clips"]["running-left"][key] = value;
            assert!(
                Manifest::parse(&invalid.to_string()).is_err(),
                "accepted legacy {key}"
            );
        }
    }

    #[test]
    fn concurrent_loads_share_immutable_storage() {
        for character in CHARACTERS {
            let atlas = SpriteAtlas::load_character(character.id).unwrap();
            let pointer = atlas.frame(Animation::Idle, 0).rgba.as_ptr() as usize;
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..16)
                    .map(|_| {
                        scope.spawn(|| {
                            let loaded = SpriteAtlas::load_character(character.id).unwrap();
                            assert_eq!(
                                loaded.frame(Animation::Idle, 0).rgba.as_ptr() as usize,
                                pointer
                            );
                        })
                    })
                    .collect();
                for handle in handles {
                    handle.join().unwrap();
                }
            });
        }
    }
}
