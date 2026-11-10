# Applied Solid Contour V2 Validation

Validated on 2026-11-09 using the versions recorded in schema-2 `applied.json`.
The owner requested crisper black edges; the assistant selected solid contour V2
after preview and main reviewed it. This is not a claim that the owner explicitly
reviewed edge-treatment samples. V1 text evidence, including its original wording,
was archived byte-for-byte under `history/v1/` before the migration. No full-sheet
v1 bitmap backup was created.
This is an on-disk asset update; no runtime process, renderer or service was
changed or restarted. No Rust tests were run in this lane; main owns the legacy
fixture replacement and final Rust validation.

## Execution

- `python -B scripts/refine-sprite-edges.py --apply`: completed pinned-v1-report,
  archived-script/recipe and all-four-known-v1-PNG preflight,
  staged PNG decode verification and all-cell audits before active replacement.
- `python -B scripts/refine-sprite-edges.py --check --deep`: passed exact replay
  from immutable candidates and comparison of every per-cell audit.
- `python -B scripts/promote-sprites.py --check`: passed with V2 and archived v1 history.
- `python -B scripts/refine-sprite-edges.py --preview`: regenerated baseline /
  alpha-only / applied-solid-contour comparisons and verified sampled applied pixels.
- Historical v1 `--remove-originals` cleanup remains authorized by the unchanged
  record. All four original PNGs remain absent; no new files were deleted for V2.
- `python -B -m unittest discover -s tests -p 'test_*sprites.py' -v`: 12 passed.
- `python -B -m unittest discover -s tests -p test_refine_sprite_edges.py -v`:
  16 passed, including reviewed-study pixel equivalence, migration refusals and
  repeated apply with identical file bytes and mtimes.
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
RGB is byte-identical to v1 everywhere; every baseline-below128 pixel is fully
RGBA-identical to v1. Every qualified strong contour core is RGBA(0,0,0,255).

Counts are baseline -> applied. Alpha columns are zero / partial / opaque.

| Character | Baseline Alpha | V2 Alpha | Opaque True Black: Baseline -> V2 |
| --- | --- | --- | --- |
| Amethyst / djinn | 10944277 / 573213 / 2539982 | 11105651 / 185940 / 2765881 | 17360 -> 84461 |
| Ruby | 12017602 / 527792 / 1512078 | 12162673 / 189296 / 1705503 | 5221 -> 44641 |
| Saphire / hexa-glasses | 12111089 / 490571 / 1455812 | 12250457 / 183189 / 1623826 | 18454 -> 48628 |
| Emerald | 12010707 / 482175 / 1564590 | 12143892 / 169726 / 1743854 | 14171 -> 55089 |

Opaque true black means exactly RGBA(0,0,0,255), not merely black RGB. Compared
directly with v1, V2 changes only qualifying strong ink alpha:

| Character | V1 Opaque True Black | V2 Opaque True Black | V1 -> V2 Partial Alpha |
| --- | --- | --- | --- |
| Amethyst / djinn | 18126 | 84461 | 252275 -> 185940 |
| Ruby | 5573 | 44641 | 228364 -> 189296 |
| Saphire / hexa-glasses | 19321 | 48628 | 212496 -> 183189 |
| Emerald | 14780 | 55089 | 210035 -> 169726 |

The maximum nonzero-bound tightening is six pixels for each character, caused by
faint-fringe removal, not coverage-mask or registration changes. Recipe: symmetric
alpha sigmoid, input <=4 zero and >=251 opaque; existing visible contour pixels
only, Disk:1 from alpha<=127, maximum RGB<=96 and Rec.709 luminance<=72. Qualified
ink with BASELINE alpha>=128 overrides the LUT to alpha255; all other pixels use
the unchanged LUT. No internal blackhat is used. Outer antialiasing is intentionally
retained, not every soft pixel removed. Static inspection retains interior
clothing/hair shading and avoids rejected crisp scratches; animation-wide flicker
remains a manual risk.

## Hashes And Cleanup

`applied.json` SHA256:
`4fece3ceae100ab8b5594b43d31db17f10e03d9c5ee784aaddf1e9dffaa4fc62`.

The V2 script SHA256 is
`5f127f6d52e5b92a7c4aa6432bd9801cbcaf9690df7e3b34e0e06ffb031d4937`.
The pinned prior v1 report SHA256 remains
`b5ea837e8cfe17e24d5607e653fad58a136af2b079666437abd9ca6f3ab318ad`;
the archived v1 script SHA256 remains
`c58eec6a8efec342180f23238b05d98799aa375da03f993a87074193b358b9c3`.
Current checks verify both the archived script/report and all recorded history
hashes without trying to match historical v1 outputs to current V2 PNGs.

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
