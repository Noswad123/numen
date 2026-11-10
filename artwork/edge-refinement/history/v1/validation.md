# Applied Contour Validation

Validated on 2026-11-09 using the versions recorded in `applied.json`.
This is an on-disk asset update; no runtime process, renderer or service was
changed or restarted. No Rust tests were run in this lane; main owns the legacy
fixture replacement and final Rust validation.

## Execution

- `python -B scripts/refine-sprite-edges.py --apply`: completed all-four preflight,
  staged PNG decode verification and all-cell audits before active replacement.
- `python -B scripts/refine-sprite-edges.py --check --deep`: passed exact replay
  from immutable candidates and comparison of every per-cell audit.
- `python -B scripts/promote-sprites.py --check`: passed before and after cleanup.
- `python -B scripts/refine-sprite-edges.py --preview`: regenerated baseline /
  alpha-only / applied-contour comparisons and verified sampled applied pixels.
- `python -B scripts/refine-sprite-edges.py --remove-originals`: completed only
  after deep validation; repeated cleanup verified without additional deletions.
- `python -B -m unittest discover -s tests -p 'test_*sprites.py' -v`: 12 passed.
- `python -B -m unittest discover -s tests -p test_refine_sprite_edges.py -v`:
  12 passed, including repeated apply with identical file bytes and mtimes.
- `git diff --check`: passed.

Fast `--check` validates hashes, decoded RGBA and the historical input chain,
using cached cell audits. It does not pretend to replay the transformation.
The separately executed `--deep` did replay the full transformation and audits.

## Results

Every output remains 3072x4576 RGBA, 384x416 per cell. Each has 65 occupied and
23 empty cells, including all 16 gaze cells; row 2 is all-channel zero. Across
all 352 cells there are zero alpha-128 bit changes, zero opaque-interior RGB
changes beyond Disk:1, zero RGB changes outside the boundary band, zero added
visible pixels, and no changes to originally zero/opaque alpha. Partial alpha
remains. Registration, manifests, aliases, timing and facing metadata are unchanged.

Counts are baseline -> applied. Alpha columns are zero / partial / opaque.

| Character | Baseline Alpha | Applied Alpha | Visible True Black |
| --- | --- | --- | --- |
| Amethyst / djinn | 10944277 / 573213 / 2539982 | 11105651 / 252275 / 2699546 | 70296 -> 203133 |
| Ruby | 12017602 / 527792 / 1512078 | 12162673 / 228364 / 1666435 | 30988 -> 133944 |
| Saphire / hexa-glasses | 12111089 / 490571 / 1455812 | 12250457 / 212496 / 1594519 | 61643 -> 125608 |
| Emerald | 12010707 / 482175 / 1564590 | 12143892 / 210035 / 1703545 | 47506 -> 142720 |

The maximum nonzero-bound tightening is six pixels for each character, caused by
faint-fringe removal, not coverage-mask or registration changes. Recipe: symmetric
alpha sigmoid, input <=4 zero and >=251 opaque; existing visible contour pixels
only, Disk:1 from alpha<=127, maximum RGB<=96 and Rec.709 luminance<=72. No internal
blackhat is used. Static inspection retains interior clothing/hair shading and
avoids rejected crisp scratches; animation-wide flicker remains a manual risk.

## Hashes And Cleanup

`applied.json` SHA256:
`b5ea837e8cfe17e24d5607e653fad58a136af2b079666437abd9ca6f3ab318ad`.

The unchanged historical promotion report SHA256 is
`adcded9bda2dff07eb9766c5fbb87407f84c3f6e8a9223b0457ecbc7e0259292`;
the unchanged neural provenance SHA256 is
`a0244112fc6f2a69e7e28243b861644042720dc63c2c0e5cf3ef43af50765cdf`.
Current full PNG/RGBA hashes, candidate/baseline hashes and unchanged manifest
SHA256 values are recorded per character in `applied.json`.

Exactly these authorized, SHA-verified bitmaps were removed:

- `artwork/original-1x/amethyst/spritesheet.png`
- `artwork/original-1x/ruby/spritesheet.png`
- `artwork/original-1x/saphire/spritesheet.png`
- `artwork/original-1x/emerald/spritesheet.png`

Their exact original SHA256 values and owner purpose are recorded in
`../hd-promotion/original-removal.json`, SHA256
`293b638528f607f2d06278a167e8dd4277cf8c83145f705ee08b09dddf60b261`.
Retained original manifests and import READMEs match historical hashes. Immutable
candidate PNGs, manifests, previews and provenance match all recorded hashes.
Neural regeneration now refuses intentionally removed originals and never falls
back to active HD; matching owner-supplied 1x originals are required externally.
