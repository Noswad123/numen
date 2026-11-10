# Edge Refinement Prototype

Preview only. Active assets, original backups, manifests, and runtime code are not
modified. Regenerate with `python scripts/refine-sprite-edges.py --preview`.
There is deliberately no apply command.

Each character directory contains three-column comparisons, with idle frame 0
above running-right frame 0. Columns, left to right:

1. CURRENT HD
2. ALPHA ONLY
3. CRISP INK + ALPHA

`comparison-{checker,light,dark}-{scale}x.png` provides source 384x416 cells at
`1x`, default-display 288x312 at `0.75x`, logical 192x208 at `0.5x`, and new-render
144x156 at `0.375x`. These are nearest-neighbor reductions, matching the runtime
sampling rather than introducing a new smoothing filter. Individual transparent
pose PNGs and a transparent comparison are included for independent inspection.
No frame is trimmed, recentered, mirrored, or rescaled during cleanup.

Alpha uses `a*a / (a*a + (255-a)*(255-a))`, rounded to eight bits, with input
`<=4` mapped to zero and `>=251` mapped to 255. Intermediate coverage remains
available, not binary. RGB is untouched by the alpha-only profile.

The crisp profile first selects existing dark neutrals: maximum channel at most
80, and channel range at most 36 or maximum channel at most 32. These become
black only when a luminance closing with Disk:2 detects a narrow dark ridge with
contrast at least 12, or when within Disk:2 of input alpha at most 127. Analysis
uses white behind alpha-zero pixels only; output RGB is never flattened against
a background. Opaque flat dark regions are not globally recolored.

## Review Status

The crisp profile is the stronger black-line comparison, not an approved final
cleanup. Source-size inspection shows some emphasized small shirt creases and
hat seams. At smaller sizes these can look more ink-like, but may still be too
strong. Hair shading and colored fills outside the ink mask are unchanged.

Nonzero-alpha bounding boxes tighten by more than two pixels in some previews
because very faint tails are removed. `report.json` explicitly flags this; the
alpha-128 coverage bounds are recorded separately so invisible fringe loss is
not confused with registration or visible silhouette movement. Do not promote
this prototype without reviewing that deviation and fine hair/outline details.

The report records per-pose zero/partial/opaque alpha counts, visible true-black
counts, mask size, nonzero and coverage bounds, and byte comparisons for opaque
fill outside the mask. The full retired row is checked as all-zero on input;
blank-frame handling and cell registration are also covered by unit tests.
Only the eight selected frames are refined by the preview; the reusable
`refine_sheet` API processes every cell independently when called explicitly.
