# 2x Sprite Candidates

## Overview

These immutable preview-first exports were owner-approved for promotion. Their
**promoted derivatives are now ACTIVE** at the four existing runtime asset paths;
candidate files here are not directly embedded. Candidate PNGs, manifests,
previews and `provenance.json` remain unchanged historical generation records.
Original 1× manifests and import READMEs are preserved under
`../original-1x/<character>/`; the owner authorized removal of the four old PNGs
after contour-stage validation. Their paths/SHA history remain in
`../hd-promotion/original-removal.json`. See the [promotion contract](../hd-promotion/README.md)
and [hash/pixel report](../hd-promotion/report.json).

## Contents

Each character directory contains a 3072x4576 lossless RGBA `spritesheet.png`, a
derived `manifest.json`, a 768x832 `comparison.png`, and an `idle-comparison.gif`.
Comparison LEFT is the original with nearest-neighbor 2x scaling; RIGHT is neural
2x. The static comparison shows first idle above first running-right, on identical
light/dark checkers. The six-frame idle GIF uses the original durations, quantized
to GIF hundredths of a second, and a checker background. GIF color quantization is
preview-only; authoritative candidate PNGs retain unquantized RGBA.

All 88 candidate grid slots remain in place: 73 occupied frames, including the 16 gaze
frames absent from manifest clips, and 15 empty cells. Each 192x208 cell is
processed independently into 384x416, without trimming, recentering, or neighboring
frame bleed. Clips, frame durations, grid indices and looping flags are unchanged.

## Method And Limits

Real-ESRGAN ncnn Vulkan release v0.2.5.0, model `realesr-animevideov3` with its x2
weights, produces neural RGB and separately bicubic-resampled alpha. No flattening,
chroma key, or palette conversion is applied to candidate PNGs. Inferred details
are not artist-authored HD originals or ground truth. Inspect animation for flicker
and checkers for halos; runtime facing/animation visual acceptance remains manual
despite approval of the candidate look.

The isolated tool and bundled model were downloaded from the official upstream:
https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesrgan-ncnn-vulkan-20220424-ubuntu.zip

The ncnn/Vulkan runner is MIT-licensed; the upstream Real-ESRGAN repository uses
[BSD-3-Clause](https://github.com/xinntao/Real-ESRGAN/blob/master/LICENSE).
No executable or model weights are vendored here. Existing artwork provenance
and rights are unchanged. `provenance.json` records local SHA256 digests of the downloaded
archive (when present), executable, weights, sources and outputs. These are local
integrity records, NOT verified upstream signatures or published checksums.

## API / Interfaces

Promotion uses `scripts/promote-sprites.py`; `--check` verifies historical sources,
reconstructed baseline pixels, alias manifests, authorized bitmap removal, and the
current contour-stage hashes without rewriting files. The original promotion
report describes historical baseline hashes; current hashes and per-cell audits
are in `../edge-refinement/applied.json`. Generation provenance remains historical.

## Data Flow

Immutable 1× backups → isolated candidate generation → approved exports → promoted
active derivatives. Promotion clears every RGBA byte in zero-based row 2
(y=832..1247), leaving **65 occupied / 23 empty cells**; all other pixels remain
bit-exact to candidates in that historical baseline, including unused gaze rows
9/10. The current active stage adds approved contour/alpha cleanup to all cells.
Active `running-left`
aliases canonical `running-right` with unchanged timing/loops. Runtime paints
facing once for color and alpha, with 57 semantic frames sharing 49 source frames;
logical sprite size remains 192 × 208. Candidate manifests retain historical rows.

## Edge Cases

Candidate files and provenance are never rewritten by promotion. Original import
hash history stays historical; active asset READMEs identify promoted artwork.
Rollback needs a matching original PNG/manifest pair. The loader normalizes even
legacy concrete left-run art to canonical right-run storage, avoiding a double flip.
The scaler requires actual 1x source geometry and rejects active HD inputs.
After authorized original removal it fails clearly rather than falling back to
active assets and upscaling them a second time.

## Example usage: reproduce

The four original PNGs are intentionally no longer bundled. Reproduction first
requires owner-supplied matching 1x source PNGs from the external archives in a
separate checkout with those originals restored and no removal record. This
command cannot regenerate from the current checkout by default, and must never
use active HD as original input. It also requires existing ImageMagick, Python
3.11+ standard library and a separately supplied compatible executable/model
directory. No downloads or installations are performed.
Use absolute paths, and a fresh output root or a not-yet-exported character:

```sh
python3 scripts/upscale-sprites.py \
  --upscaler /absolute/path/realesrgan-ncnn-vulkan \
  --models /absolute/path/models \
  --output /tmp/opencode/fresh-numen-candidates
```

Omit `--output` for `artwork/upscaled-2x`. Repeat `--character amethyst`, `ruby`,
`saphire`, or `emerald` to select a subset. Existing character outputs are refused,
including incomplete runs; choose a fresh root rather than overwriting. Temporary
cell files live under `/tmp/opencode` and are automatically removed. GPU 0, tile
192 and jobs `1:1:1` are explicit. This exporter does not promote assets or control
services; activation is the separate promotion step above.

Validation checks every frame's size, occupied/empty correspondence, transparent,
partial and opaque alpha categories, and alpha bounds within four output pixels of
doubled source bounds (bicubic's two-source-pixel support can extend faint edges
beyond a two-output-pixel limit). Per-cell offsets are recorded in provenance.
It also checks full-sheet lossless RGBA round-trip, packing,
unchanged clip metadata, and byte-identical hashes for all original PNGs/manifests
supplied to that historical generation run. This is not a claim that old bitmaps
remain bundled after the separately recorded cleanup.

```sh
python3 -B -m unittest discover -s tests -p test_upscale_sprites.py -v
```

## Related files

- [`scripts/upscale-sprites.py`](../../scripts/upscale-sprites.py): candidate export.
- [`scripts/promote-sprites.py`](../../scripts/promote-sprites.py): promotion/check.
- [Promotion report](../hd-promotion/report.json): distinct file/pixel hashes.
- [Current HD status](../../README.md#current-hd-artwork-and-facing) and
  [scene composition](../../ARCHITECTURE.md#scene-composition): runtime and evidence limits.
