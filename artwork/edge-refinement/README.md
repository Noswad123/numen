# Solid Contour V2

The owner requested crisper black edges; the assistant selected solid contour V2
after previews, and main reviewed the variant before application across all cells.
This does not claim that the owner explicitly reviewed edge-treatment samples.
`applied.json` records the complete current stage; no manifests, neural sources,
or runtime code are changed. Regenerate comparisons with
`python -B scripts/refine-sprite-edges.py --preview`. Comparisons always start from
the historical candidate-plus-blank-row baseline, never reprocess active pixels,
and verify sampled applied pixels when the applied stage exists.

Each character directory contains three-column comparisons, with idle frame 0
above running-right frame 0. Columns, left to right:

1. BASELINE HD (before edge treatment)
2. ALPHA ONLY
3. APPLIED SOLID CONTOUR

`comparison-{checker,light,dark}-{scale}x.png` provides source 384x416 cells at
`1x`, default-display 288x312 at `0.75x`, logical 192x208 at `0.5x`, and new-render
144x156 at `0.375x`. These are nearest-neighbor reductions, matching the runtime
sampling rather than introducing a new smoothing filter. Individual transparent
pose PNGs and a transparent comparison are included for independent inspection.
No frame is trimmed, recentered, mirrored, or rescaled during cleanup.

Alpha uses `a*a / (a*a + (255-a)*(255-a))`, rounded to eight bits, with input
`<=4` mapped to zero and `>=251` mapped to 255. Intermediate coverage remains
available, not binary. RGB is untouched by the alpha-only profile.
V2 overrides alpha to 255 ONLY for qualified contour ink with BASELINE alpha>=128.
All other pixels retain the LUT output. This makes strong existing ink cores true
RGBA(0,0,0,255), while preserving the full alpha-128 mask and v1's below-128 pixels.

The default `contour` profile selects only existing visible pixels within Disk:1
of input alpha at most 127, with maximum RGB channel at most 96 and Rec.709
luminance at most 72. Qualified pixels become black. There is no neutral-color
restriction, blackhat, internal ridge detection, or background flattening.
Fully opaque interior RGB farther than that boundary band is byte-identical.
Bright colored edges are preserved and no visible pixels are added to the canvas.
The report includes numerical parameters for downstream callers.

## Review Status

The internal-blackhat `crisp` profile was rejected for scratchy shirt creases and
hat-strap noise. It remains available only as an explicit study profile, never
the API default. Its original report, documentation, and previews are preserved
under `rejected-crisp/`. Explicit `*-crisp.png` files in the character directories
also remain historical, not selected. All current `comparison-*` images use
`contour`; both reusable APIs default to it. The apply path hardcodes `contour`;
there is no switch to apply rejected `crisp`.

Nonzero-alpha bounding boxes tighten by more than two pixels in some previews
because very faint tails are removed. `report.json` explicitly flags this; the
entire alpha-128 coverage bitset is compared, not merely its bounding box, so
invisible fringe loss is not confused with registration or silhouette movement.
Partial transparency gradients remain. Main approved all four contour previews;
animation-wide fine-hair and flicker assessment remains manual, not an automated
acceptance claim.

The report records per-pose zero/partial/opaque alpha counts, visible and opaque true-black
counts, mask size, nonzero and coverage bounds, and byte comparisons for opaque
fill outside the mask and the full opaque interior beyond Disk:1. It also records
coverage-bit changes, added visible pixels, and changes to originally zero or
opaque alpha. The full retired row is checked as all-zero on input;
blank-frame handling and cell registration are also covered by unit tests.
Only the eight selected frames are shown by the preview. `--apply` and the reusable
`refine_sheet` API process every cell independently, including all gaze cells.

## Apply And Verify

`python -B scripts/refine-sprite-edges.py --apply` preflights all four historical
inputs and known active baselines, stages/decodes/audits all outputs, then performs
atomic per-file replacements and writes the new `applied.json`. Repeated apply is
a read-only verification, not another alpha pass. The v1-to-v2 migration accepts
only the pinned v1 report/recipe/script history and all four matching active v1
PNGs; it refuses stale reports or unknown pixels. Unknown stages are refused.

V1 script, applied report, preview report, README and validation text are preserved
byte-for-byte under `history/v1/`, without a full-sheet bitmap backup. The V2
report records their hashes and paths; every current check verifies that history.
Historical v1 wording is preserved as historical text, not a renewed assertion
that the owner personally selected edge samples. `contour-v1` is an explicit
study-only API profile; default `contour` and the apply path use V2 solid ink.

`--check` validates the full historical chain and current PNG/decoded RGBA hashes;
cell audits in its report are cached. `--check --deep` additionally recomputes
every cell and audit from immutable HD candidates. Both modes are read-only.
`--remove-originals` is the separately owner-authorized four-bitmap cleanup after
stage validation, with exact paths/hashes in `../hd-promotion/original-removal.json`.
Historical promotion and neural provenance are never rewritten. See
`../hd-promotion/README.md` for stage-chain and interruption limits.
