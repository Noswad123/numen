use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::animation::Animation;

pub const CELL_WIDTH: u32 = 192;
pub const CELL_HEIGHT: u32 = 208;
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
    frames: Vec<Vec<u8>>,
}

pub struct SpriteAtlas {
    clips: HashMap<Animation, Clip>,
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
    row: u32,
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
            manifest.width == 1536 && manifest.height == 2288,
            "invalid atlas dimensions"
        );
        ensure!(
            manifest.cell_width == CELL_WIDTH && manifest.cell_height == CELL_HEIGHT,
            "invalid cell dimensions"
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
            ensure!(
                clip.row < manifest.rows,
                "invalid row for {}",
                animation.label()
            );
            ensure!(
                rows.insert(clip.row),
                "duplicate atlas row for {}",
                animation.label()
            );
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
        Ok(manifest)
    }
}

impl SpriteAtlas {
    pub fn load() -> Result<Self> {
        Self::load_character(DEFAULT_CHARACTER_ID)
    }

    pub fn load_character(id: &str) -> Result<Self> {
        let character = character(id).with_context(|| format!("unknown character: {id}"))?;
        Self::from_bytes(character.manifest, character.png)
            .with_context(|| format!("cannot load character: {id}"))
    }

    fn from_bytes(json: &str, png: &[u8]) -> Result<Self> {
        let manifest = Manifest::parse(json)?;
        let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
            .context("cannot decode embedded spritesheet")?
            .into_rgba8();
        ensure!(
            image.dimensions() == (manifest.width, manifest.height),
            "embedded PNG dimensions do not match manifest"
        );
        let mut clips = HashMap::new();
        for animation in Animation::ALL {
            let metadata = &manifest.clips[&animation];
            let frames = (0..metadata.frame_durations_ms.len())
                .map(|column| {
                    image::imageops::crop_imm(
                        &image,
                        column as u32 * CELL_WIDTH,
                        metadata.row * CELL_HEIGHT,
                        CELL_WIDTH,
                        CELL_HEIGHT,
                    )
                    .to_image()
                    .into_raw()
                })
                .collect();
            clips.insert(
                animation,
                Clip {
                    frame_durations: metadata
                        .frame_durations_ms
                        .iter()
                        .map(|&ms| Duration::from_millis(ms))
                        .collect(),
                    looping: metadata.looping,
                    frames,
                },
            );
        }
        Ok(Self { clips })
    }

    /// `index` must be in `0..clip(animation).frame_durations.len()`.
    pub fn frame(&self, animation: Animation, index: usize) -> &[u8] {
        &self.clips[&animation].frames[index]
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
            assert_eq!(source.dimensions(), (1536, 2288));
            let expected: [(Animation, u32, &[u64]); 9] = [
                (Animation::Idle, 0, &[280, 110, 110, 140, 140, 320]),
                (
                    Animation::RunningRight,
                    1,
                    &[120, 120, 120, 120, 120, 120, 120, 220],
                ),
                (
                    Animation::RunningLeft,
                    2,
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
                assert_eq!(manifest.clips[&animation].row, row);
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
                    assert_eq!(frame.len(), (CELL_WIDTH * CELL_HEIGHT * 4) as usize);
                    let expected = image::imageops::crop_imm(
                        &source,
                        index as u32 * CELL_WIDTH,
                        row * CELL_HEIGHT,
                        CELL_WIDTH,
                        CELL_HEIGHT,
                    )
                    .to_image();
                    assert_eq!(frame, expected.as_raw());
                    assert!(std::ptr::eq(frame, atlas.frame(animation, index)));
                    assert!(frame.chunks_exact(4).any(|pixel| pixel[3] == 0));
                    assert!(frame.chunks_exact(4).any(|pixel| pixel[3] == 255));
                    assert!(
                        frame
                            .chunks_exact(4)
                            .any(|pixel| pixel[3] > 0 && pixel[3] < 255)
                    );
                }
            }
            assert!(
                atlas
                    .frame(Animation::Idle, 0)
                    .chunks_exact(4)
                    .any(|pixel| pixel[3] == 0)
            );
            assert!(
                atlas
                    .frame(Animation::Idle, 0)
                    .chunks_exact(4)
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
}
