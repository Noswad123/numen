//! Single-face text metrics and grayscale rasterization, without complex-script shaping.
use std::{collections::HashMap, fs::File, io::Read, path::Path};

use anyhow::{Context, Result, anyhow, bail, ensure};
use fontdb::{Database, Family, Query, Source, Stretch, Style, Weight};
use fontdue::{Font, FontSettings, LineMetrics, Metrics};

pub const PREFERRED_FAMILY: &str = "SauceCodePro Nerd Font";
pub const FALLBACK_FONT_LICENSE: &str = include_str!("../assets/fonts/OFL.txt");
const BUNDLED_FONT: &[u8] = include_bytes!("../assets/fonts/SourceCodePro-Regular.ttf");
const MAX_FONT_BYTES: usize = 32 * 1024 * 1024;
const MAX_CACHED_GLYPHS: usize = 512;

pub struct RasterGlyph {
    pub metrics: Metrics,
    pub coverage: Vec<u8>,
}

pub struct Typeface {
    font: Font,
    description: String,
    fallback_index: u16,
    cache_px: Option<u32>,
    glyphs: HashMap<u16, RasterGlyph>,
}

impl Typeface {
    /// Explicit selections fail rather than silently substituting a different face.
    pub fn load(family: Option<&str>, file: Option<&Path>) -> Result<Self> {
        ensure!(
            family.is_none() || file.is_none(),
            "font family and font file are mutually exclusive"
        );
        if let Some(path) = file {
            return Self::from_bytes(
                read_font(path)?,
                0,
                format!("font file {} (face 0)", path.display()),
            );
        }
        if let Some(name) = family {
            ensure!(!name.trim().is_empty(), "font family must not be empty");
        }

        let mut database = Database::new();
        // fontdb discovers standard system and user font directories itself.
        database.load_system_fonts();
        let selected = Self::from_database(&database, family.unwrap_or(PREFERRED_FAMILY));
        if family.is_some() {
            return selected;
        }
        match selected {
            Ok(face) => Ok(face),
            Err(error) => {
                let mut face = Self::bundled()?;
                face.description.push_str(&format!(
                    "; default {PREFERRED_FAMILY:?} unavailable: {error:#}"
                ));
                Ok(face)
            }
        }
    }

    pub fn bundled() -> Result<Self> {
        Self::from_bytes(
            BUNDLED_FONT.to_vec(),
            0,
            "bundled Source Code Pro Regular (base font, not Nerd Font icons)".into(),
        )
    }

    pub fn from_bytes(bytes: Vec<u8>, index: u32, description: String) -> Result<Self> {
        ensure!(bytes.len() <= MAX_FONT_BYTES, "font exceeds 32 MiB limit");
        let font = Font::from_bytes(
            bytes,
            FontSettings {
                collection_index: index,
                ..FontSettings::default()
            },
        )
        .map_err(|error| anyhow!("invalid font {description:?} (face {index}): {error}"))?;
        let fallback_index = ['\u{fffd}', '?']
            .into_iter()
            .map(|c| font.lookup_glyph_index(c))
            .find(|&index| index != 0)
            // If neither replacement character exists, use this face's .notdef.
            .unwrap_or(0);
        Ok(Self {
            font,
            description,
            fallback_index,
            cache_px: None,
            glyphs: HashMap::new(),
        })
    }

    fn from_database(database: &Database, family: &str) -> Result<Self> {
        let id = database
            .query(&Query {
                families: &[Family::Name(family)],
                weight: Weight::NORMAL,
                style: Style::Normal,
                stretch: Stretch::Normal,
            })
            .with_context(|| format!("installed font family {family:?} not found"))?;
        let face = database
            .face(id)
            .context("selected font face disappeared")?;
        // CSS matching can otherwise return a bold/italic/condensed substitute.
        ensure!(
            face.families.iter().any(|(name, _)| name == family)
                && face.weight == Weight::NORMAL
                && face.style == Style::Normal
                && face.stretch == Stretch::Normal,
            "installed font family {family:?} has no Regular face"
        );
        let (source, index) = database
            .face_source(id)
            .context("selected font source disappeared")?;
        let bytes = match source {
            Source::File(path) | Source::SharedFile(path, _) => read_font(&path)?,
            Source::Binary(data) => {
                let bytes = data.as_ref().as_ref();
                ensure!(bytes.len() <= MAX_FONT_BYTES, "font exceeds 32 MiB limit");
                bytes.to_vec()
            }
        };
        Self::from_bytes(
            bytes,
            index,
            format!("installed {family} Regular (face {index})"),
        )
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    /// Single-line advance, including pair kerning, in physical pixels.
    pub fn measure(&self, text: &str, px: f32) -> f32 {
        assert!(px.is_finite() && px > 0.0, "invalid font pixel size");
        let mut width = 0.0;
        let mut previous = None;
        for c in text.chars() {
            let index = self.glyph_index(c);
            if let Some(left) = previous {
                width += self
                    .font
                    .horizontal_kern_indexed(left, index, px)
                    .unwrap_or(0.0);
            }
            width += self.font.metrics_indexed(index, px).advance_width;
            previous = Some(index);
        }
        width
    }

    pub fn line_metrics(&self, px: f32) -> LineMetrics {
        assert!(px.is_finite() && px > 0.0, "invalid font pixel size");
        self.font.horizontal_line_metrics(px).unwrap_or_else(|| {
            // Derive baseline extents from this face if it has no horizontal metrics.
            let upper = self.font.metrics_indexed(self.glyph_index('M'), px).bounds;
            let lower = self.font.metrics_indexed(self.glyph_index('g'), px).bounds;
            let ascent = (upper.ymin + upper.height)
                .max(lower.ymin + lower.height)
                .max(px * 0.8);
            let descent = upper.ymin.min(lower.ymin).min(-px * 0.2);
            LineMetrics {
                ascent,
                descent,
                line_gap: 0.0,
                new_line_size: ascent - descent,
            }
        })
    }

    pub fn kern(&self, previous: char, current: char, px: f32) -> f32 {
        assert!(px.is_finite() && px > 0.0, "invalid font pixel size");
        self.font
            .horizontal_kern_indexed(self.glyph_index(previous), self.glyph_index(current), px)
            .unwrap_or(0.0)
    }

    pub fn glyph(&mut self, c: char, px: f32) -> &RasterGlyph {
        assert!(px.is_finite() && px > 0.0, "invalid font pixel size");
        if self.cache_px != Some(px.to_bits()) {
            self.glyphs.clear();
            self.cache_px = Some(px.to_bits());
        }
        let index = self.glyph_index(c);
        if self.glyphs.len() == MAX_CACHED_GLYPHS && !self.glyphs.contains_key(&index) {
            self.glyphs.clear();
        }
        self.glyphs.entry(index).or_insert_with(|| {
            let (metrics, coverage) = self.font.rasterize_indexed(index, px);
            RasterGlyph { metrics, coverage }
        })
    }

    fn glyph_index(&self, c: char) -> u16 {
        match self.font.lookup_glyph_index(c) {
            0 => self.fallback_index,
            index => index,
        }
    }
}

fn read_font(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("opening font {}", path.display()))?;
    let metadata = file.metadata().context("reading font metadata")?;
    ensure!(metadata.is_file(), "font path must be a regular file");
    ensure!(
        metadata.len() <= MAX_FONT_BYTES as u64,
        "font exceeds 32 MiB limit"
    );
    let mut bytes = Vec::new();
    // A bounded read also covers files that grow after the metadata check.
    file.take(MAX_FONT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading font {}", path.display()))?;
    if bytes.len() > MAX_FONT_BYTES {
        bail!("font exceeds 32 MiB limit");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn bundled_raster_has_antialiased_coverage_and_real_line_metrics() {
        let mut face = Typeface::bundled().unwrap();
        assert!(face.description().contains("bundled"));
        assert!(FALLBACK_FONT_LICENSE.contains("SIL OPEN FONT LICENSE"));
        let glyph = face.glyph('A', 16.0);
        assert_eq!(
            glyph.coverage.len(),
            glyph.metrics.width * glyph.metrics.height
        );
        assert!(glyph.coverage.iter().any(|&value| value > 0 && value < 255));
        let lines = face.line_metrics(16.0);
        assert_eq!(lines, face.font.horizontal_line_metrics(16.0).unwrap());
        assert!(lines.ascent > 0.0 && lines.descent < 0.0);
        assert!(lines.new_line_size > 0.0);
    }

    #[test]
    fn advances_kerning_and_unicode_fallback_match_painting() {
        let mut face = Typeface::bundled().unwrap();
        let missing = '\u{10ffff}';
        assert_eq!(face.font.lookup_glyph_index(missing), 0);
        let replacement = if face.font.lookup_glyph_index('\u{fffd}') != 0 {
            '\u{fffd}'
        } else {
            '?'
        };
        assert_eq!(face.glyph_index(missing), face.glyph_index(replacement));
        let expected = face.glyph(replacement, 16.0).coverage.clone();
        assert_eq!(face.glyph(missing, 16.0).coverage, expected);
        assert_eq!(face.glyphs.len(), 1);
        for px in [0.125, 16.0, 32.0] {
            let text = "AV\u{00e9}\u{1f680}\u{10ffff}?";
            let mut advance = 0.0;
            let mut previous = None;
            for c in text.chars() {
                if let Some(left) = previous {
                    advance += face.kern(left, c, px);
                }
                advance += face.glyph(c, px).metrics.advance_width;
                previous = Some(c);
            }
            assert!((face.measure(text, px) - advance).abs() < 0.0001);
            assert_eq!(face.kern('A', missing, px), face.kern('A', replacement, px));
            assert_eq!(face.kern(missing, 'V', px), face.kern(replacement, 'V', px));
            assert_eq!(face.measure("", px), 0.0);
        }
    }

    #[test]
    fn cache_tracks_exact_physical_size_and_is_bounded() {
        let mut face = Typeface::bundled().unwrap();
        let small = face.glyph('M', 16.0).metrics;
        face.glyph('g', 16.0);
        assert_eq!(face.glyphs.len(), 2);
        let large = face.glyph('M', 32.0).metrics;
        assert_eq!(face.glyphs.len(), 1);
        assert!(large.width > small.width && large.height > small.height);
        assert!((large.advance_width - small.advance_width * 2.0).abs() < 0.0001);
        face.glyph('M', 32.001);
        assert_eq!(face.cache_px, Some(32.001_f32.to_bits()));
        assert_eq!(face.glyphs.len(), 1);

        let mut indices = HashSet::new();
        let mut reached_limit = false;
        for c in (0..=0x10ffff).filter_map(char::from_u32) {
            if !indices.insert(face.glyph_index(c)) {
                continue;
            }
            face.glyph(c, 16.0);
            assert!(face.glyphs.len() <= MAX_CACHED_GLYPHS);
            reached_limit |= face.glyphs.len() == MAX_CACHED_GLYPHS;
            if indices.len() > MAX_CACHED_GLYPHS {
                break;
            }
        }
        assert!(reached_limit, "fixture must exercise cache eviction");
        assert_eq!(face.glyphs.len(), 1);
    }

    #[test]
    fn invalid_and_conflicting_explicit_selections_fail() {
        assert!(Typeface::from_bytes(vec![], 0, "empty".into()).is_err());
        assert!(Typeface::from_bytes(b"not a font".to_vec(), 0, "invalid".into()).is_err());
        assert!(Typeface::load(Some(""), None).is_err());
        assert!(Typeface::load(Some(PREFERRED_FAMILY), Some(Path::new("unused"))).is_err());
        let directory = tempfile::tempdir().unwrap();
        assert!(Typeface::load(None, Some(&directory.path().join("missing.ttf"))).is_err());
        assert!(Typeface::load(None, Some(directory.path())).is_err());
        let invalid = tempfile::NamedTempFile::new().unwrap();
        assert!(Typeface::load(None, Some(invalid.path())).is_err());
    }

    #[test]
    fn explicit_file_uses_the_bundled_fixture_without_system_discovery() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/SourceCodePro-Regular.ttf");
        let mut face = Typeface::load(None, Some(&path)).unwrap();
        let mut bundled = Typeface::bundled().unwrap();
        assert!(face.description().contains("font file"));
        assert_eq!(face.measure("AV?", 16.0), bundled.measure("AV?", 16.0));
        assert_eq!(
            face.glyph('A', 16.0).coverage,
            bundled.glyph('A', 16.0).coverage
        );
    }

    #[test]
    fn exact_regular_family_query_uses_only_fixture_database() {
        let mut database = Database::new();
        database.load_font_data(BUNDLED_FONT.to_vec());
        let family = database.faces().next().unwrap().families[0].0.clone();
        let face = Typeface::from_database(&database, &family).unwrap();
        assert!(face.description().contains(&family));
        assert!(Typeface::from_database(&database, "Numen nonexistent fixture family").is_err());
        assert!(Typeface::from_database(&database, &format!("{family} wrong suffix")).is_err());
    }

    #[test]
    fn font_size_limit_is_checked_before_parsing_or_reading() {
        assert!(Typeface::from_bytes(vec![0; MAX_FONT_BYTES + 1], 0, "oversize".into()).is_err());
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(MAX_FONT_BYTES as u64 + 1).unwrap();
        assert!(Typeface::load(None, Some(file.path())).is_err());
    }

    #[test]
    fn invalid_pixel_sizes_are_rejected() {
        let mut face = Typeface::bundled().unwrap();
        for px in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(std::panic::catch_unwind(|| face.measure("A", px)).is_err());
            assert!(std::panic::catch_unwind(|| face.kern('A', 'V', px)).is_err());
            assert!(std::panic::catch_unwind(|| face.line_metrics(px)).is_err());
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    face.glyph('A', px);
                }))
                .is_err()
            );
        }
    }
}
