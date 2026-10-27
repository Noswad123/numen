# Bubble font fallback

## Overview

Numen prefers installed **SauceCodePro Nerd Font Regular**, matching the owner's
configured Kitty family. Discovery uses `fontdb` standard system and user font
directories, not runtime parsing of Kitty configuration or hardcoded machine
paths. The default UI size is 16 logical canvas units, not Kitty's runtime size.
Installed Nerd Fonts are not copied into this project.

`SourceCodePro-Regular.ttf` is the embedded portable fallback and positive-test
fixture from Adobe's Source Code Pro project, under SIL Open Font License 1.1.
It is the upstream base text font, **not** a Nerd Font or a substitute for its
icon coverage. The repository filename used by source is
`assets/fonts/SourceCodePro-Regular.ttf` (not `SCP-Regular.ttf`).

Provenance: downloaded as-is from
[adobe-fonts/source-code-pro, release/TTF/SourceCodePro-Regular.ttf](https://github.com/adobe-fonts/source-code-pro/blob/release/TTF/SourceCodePro-Regular.ttf),
not copied from a locally installed Nerd Font. Supplied SHA-256:
`74bd80d3e42a08517cd7e1108ba3d86f2da29ac0f3065be95e0357956ab9db37`
(not recomputed in this docs-only update).

## API / Interfaces

- Default: `Typeface::load(None, None)` selects installed SauceCodePro Nerd Font
  Regular; if unavailable/unloadable it uses bundled Source Code Pro Regular.
- `run --font-family NAME`: exact installed family with Normal weight, style and
  stretch (Regular), not a bold/italic/condensed substitute.
- `run --font-file PATH`: TTF/OTF override at face 0; a TTC uses collection index 0.
  This conflicts with `--font-family`.
- Explicit missing/invalid family/file selections **error**, never silently fall
  back. `--font-size` accepts finite 8–32, default 16 logical canvas units, scaled
  with `--scale` and physical display density.
- `numen font-license` prints the **full embedded SIL OFL 1.1 license text** from
  [`OFL.txt`](OFL.txt); it does not load fonts or open a GUI.

## Data Flow

Bubble-enabled GUI initialization → `Typeface::load` → installed exact Regular
face or bundled default fallback → `fontdue` metrics/advances/kerning and glyph
coverage → physical-pixel CPU bubble composition → 1:1 GPU presentation.
`fontdb` and `fontdue` replace the removed `font8x8` bitmap dependency.

Glyphs rasterize at `font_size × physical scene scale` with grayscale coverage
alpha-blended onto ivory. Wrapping/clipping measures actual widths and uses
U+2026 `…`, with up to three summary lines depending on line height and available
space. Larger font sizes can reduce visible lines without resizing the sprite.
The logical canvas remains 384 × 352, with the 192 × 208 sprite at `(96, 144)`;
only the sprite is nearest-resampled, not text or the whole canvas.

## Edge Cases

- Unicode and Nerd icons require coverage in the selected face. Missing glyphs
  use that face's U+FFFD, then `?`, then `.notdef`. No full script shaping, color
  emoji or fallback-font chain is implemented.
- Selected font data has a 32 MiB limit, with bounded reads even if a file grows.
  The `fontdb` system metadata scan is outside that cap. The raster cache holds
  at most 512 glyph entries at one exact physical pixel size; size/DPI changes
  clear it.
- `run --no-bubble` skips font discovery/loading even for an invalid file or
  nonexistent family. CLI syntax/conflict/range validation still applies.
  `listen`, `publish`, `status`, `focus`, `check-assets`, and `font-license` never
  initialize fonts. Hiding the bubble does not redact metadata elsewhere.
- Positive font tests use this bundled fixture without OS-font requirements.
  The separately run ignored preview uses default discovery: supplied validation
  reports the installed preferred face and inspected 1×/2× images, not actual
  mixed-monitor DPI acceptance. See the [validation record](../../README.md#validation)
  for reported results and pending manual font-error/fallback/privacy checks.

## Example usage

```sh
cargo run --locked -- run --font-family 'SauceCodePro Nerd Font' --font-size 18
cargo run --locked -- run --font-file assets/fonts/SourceCodePro-Regular.ttf
cargo run --locked -- font-license
```

## Related files

- [`src/typography.rs`](../../src/typography.rs): selection, bounded reads, metrics and cache.
- [`src/bubble.rs`](../../src/bubble.rs): measured layout, alpha composition and previews.
- [`src/window.rs`](../../src/window.rs): font initialization and physical-size buffer/surface updates.
- [`src/main.rs`](../../src/main.rs): CLI validation and license command.
- [README](../../README.md), [Architecture](../../ARCHITECTURE.md): usage and validation.
- [`OFL.txt`](OFL.txt): bundled fallback license, embedded verbatim by source.
