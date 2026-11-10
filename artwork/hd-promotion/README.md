# Approved HD Stages

The owner-approved candidates in `../upscaled-2x/<character>/` are immutable.
Their neural generation provenance, previews and comparisons remain unchanged.
`report.json` is immutable historical evidence of the initial HD promotion. Its
active hashes describe that baseline, NOT the current contour-treated PNGs. Its
script digest identifies the historical execution, not today's verifier source.
It records candidate provenance, original/candidate/baseline hashes and RGBA hashes.

The original historical execution of `python3 scripts/promote-sprites.py` verified PNG/manifest
hashes against candidate provenance and created verified backups of all four original PNGs,
manifests and import READMEs into `artwork/original-1x/<character>/`. Existing
metadata is never overwritten; a mismatching backup is an error. Those backups
were outside active assets and were not embedded or catalogued.
The current script is verification-only: both its normal command and `--check`
require the recorded promotion and never rewrite active files or backups. Fresh
promotion is retired. Without `report.json`, both invocations raise
`Fresh promotion is retired; recorded promotion required` before reading candidate
provenance or making any mutation. The current tool does not create a new
promotion record, reconstruct a fresh installation, or restore removed originals.

Active path mapping: `amethyst -> assets/djinn`, `ruby -> assets/ruby`,
`saphire -> assets/hexa-glasses`, `emerald -> assets/emerald`.

Promotion preserves the candidate 3072 x 4576 RGBA grid (8 x 11 cells of
384 x 416), but clears **all four channels** in zero-based row 2, y=832..1247.
In that historical baseline, every pixel outside the row was identical to its candidate, including
gaze rows 9/10. There is no reindexing, AI regeneration, or mirrored sheet.
`running-left` uses `"source": "running-right"` with unchanged durations and
looping. The loader shares canonical storage; painting applies chosen facing
once to both color and alpha. Logical dimensions remain 192 x 208.

## Current Solid Contour V2

The owner requested crisper black edges; the assistant selected boundary-only
solid contour V2 after preview, and main reviewed the variant. This is not a claim
that the owner explicitly reviewed edge samples. The treatment covers all 65 occupied cells
per character, including gaze rows. All 23 empty cells and retired row 2 remain
RGBA zero. `../edge-refinement/applied.json` records the current PNG/RGBA hashes,
unchanged manifest SHA, exact recipe, tool/script versions, historical input-chain
hashes, per-cell audits, alpha categories, and weak-fringe tightening. Internal
blackhat `crisp` was rejected and is never used by the apply path.
V2 makes qualified Disk:1 dark ink fully opaque only where BASELINE alpha>=128;
everywhere else the same smooth alpha LUT applies. Opaque true-black counts are
recorded separately from merely RGB-black partial pixels. This is not a promise
to remove all antialiasing or every soft pixel.

V1 text evidence is immutable under `../edge-refinement/history/v1/`. Current
`applied.json` is schema 2 and links those hashes/paths. `--apply` upgrades only a
known pinned v1 report plus matching active v1 PNGs; a V2 rerun is verification
only. Every current check verifies archived report/script hashes as well as the
current stage. There is no full-sheet v1 bitmap backup.

`python3 scripts/refine-sprite-edges.py --apply` preflights all four candidates,
original metadata and known active baselines before staging any output. It always
starts from immutable candidate RGBA with row 2 cleared, never current active
pixels. All staged PNGs are decoded and compared before atomic per-file replaces;
the complete applied stage is verified before writing its new report. A completed
rerun verifies without writing or repeating the alpha curve. Unknown/stale stages
are refused; there is no force option. The four replacements are individually
atomic, not a cross-file transaction; interruption during replacement requires
manual investigation, not automatic acceptance of a mixed stage.

```sh
python3 -B scripts/promote-sprites.py --check
python3 -B scripts/refine-sprite-edges.py --check
python3 -B scripts/refine-sprite-edges.py --check --deep
```

The promotion check verifies all historical candidate artifacts, retained original
metadata, reconstructed baseline RGBA, alias/timing manifests, then the current
stage. Fast refinement checks verify PNG hashes and decoded RGBA hashes plus the
input/recipe chain; cell audits are cached, not recomputed proof. `--deep` replays
the contour transformation and every cell audit from immutable candidates.
Neither check writes files. No neural inference is run.

## Original Bitmap Removal

After full active-stage validation the owner authorized removal of exactly four
superseded 1x `spritesheet.png` files. `original-removal.json` records the exact
paths, original SHA256 values, owner purpose and historical-report hash. Missing
PNGs are errors without that exact authorization; present mismatching PNGs are
always errors. `--remove-originals` checks the current stage and every original
hash before recording authorization and deleting only those four bitmaps. Reruns
verify the same state. Original manifests, per-character import READMEs and SHA
history remain unchanged. Original bitmaps are no longer bundled; regeneration or
1x rollback requires owner-supplied matching originals from the external archives.
