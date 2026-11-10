# Emerald runtime assets

## Active HD Source

The active sheet is promoted from owner-approved
`artwork/upscaled-2x/emerald/spritesheet.png`: 3072 x 4576 RGBA,
8 x 11 cells of 384 x 416, with logical size still 192 x 208.
Retired row 2 is RGBA zero. All 65 occupied cells, including gaze rows 9/10,
receive boundary-only solid `contour` V2 ink plus a smooth alpha curve. The owner
requested crisper black edges; the assistant selected this recipe after preview
and main reviewed it. Qualified ink with baseline alpha>=128 becomes fully opaque
black; other pixels keep the alpha LUT, including antialiased outer fringes.
Opaque interior fills beyond the one-pixel boundary band are byte-identical;
the entire alpha-128 coverage mask is unchanged. Internal blackhat was rejected.
`running-left` aliases `running-right`, mirrored once at painting; timings,
registration and all row indices are unchanged. Current hashes and per-cell
audits are in `artwork/edge-refinement/applied.json`; historical baseline evidence
is in `artwork/hd-promotion/report.json`. Neural provenance remains untouched.
Original manifests/import history remain in `artwork/original-1x/emerald/`;
the old 1x PNG was owner-authorized for removal, with its SHA/path retained in
`artwork/hd-promotion/original-removal.json`.

## Original 1x Import History

The details below describe the historical original, no longer bundled as a PNG,
not the active HD derivative. Original metadata and SHA records are retained.

Emerald (`emerald`) is an owner-supplied/generated character in Numen's embedded
catalog. Its original archive name is Groove; the runtime identity is Emerald,
independent of activity source IDs.

`spritesheet.png` is imported byte-for-byte from `groove/spritesheet.png` in
`/Users/jdawson/Downloads/groove-artifacts.zip` on 2026-11-06. Only the final
runtime PNG is imported, not `groove/groove.png`, source images, prompts, QA,
contact sheets, or previews. Those remain in the original archive.

- Archive SHA-256: `baebf34de14f98a7265334066c6ec73dc553dbe5f386ac959648d9b246716524`.
- PNG SHA-256: `f3ec3797e71ada6ea60c9b0562f489e09fc41e26a80eb9409264011fcb7dccdc`.
- PNG size: 1,884,385 bytes.
- `groove/pet.json`: name `Groove`, ID `pet_6aee6ebcba2c81918afc861a878b2a65`,
  `is_custom: true`, `is_active: false`, `artwork_tool: built-in image_gen`.
- Original description: "A laid-back green male djinn with dark dreadlocks,
  red-gold-green wraps, a charcoal tank, and easygoing rhythmic gestures."
- `groove/request.json`: `pet_id: groove`, `display_name: Groove`, sprite
  version 2, `created_at: 2026-11-07T00:02:29.693816+00:00`.

The final PNG header confirms 1536 x 2288 pixels, 8-bit RGBA, non-interlaced.
The atlas has 8 columns and 11 rows of 192 x 208 cells, matching the request
and `groove/qa/extended-assembly.json`. Decoded alpha inspection found
2,937,126 fully transparent, 440,920 fully opaque, and 136,322 partially
transparent pixels. Occupied columns match the frame counts below; rows 9
and 10 each have eight occupied cells for 16 clockwise directional gaze
poses (0 through 337.5 degrees in 22.5-degree steps), not exposed as clips.

Preserve the existing alpha channel; do not chroma-key or re-encode this PNG.
`groove/qa/chroma-despill-extended.json` records edge-local chroma spill
suppression with `alpha_preserved: true` for the final extended atlas.

`manifest.json` is Numen-owned and contains no generation-machine paths.
Rows and frame counts come from `groove/request.json`, checked against the
final PNG's occupied cells. Durations were read directly from GIF Graphic
Control Extensions in `groove/previews/<source state>.gif`, not inferred
from combined previews. They match the existing catalog's exact atlas test.
Numen maps `jumping` to `succeeded` and `running` to `working`. Loop policy is
Numen-owned: waving, succeeded, and failed play once; other clips loop.

| Numen clip | Source state | Row | Frame durations (ms) |
| --- | --- | --- | --- |
| idle | idle | 0 | 280, 110, 110, 140, 140, 320 |
| running-right | running-right | 1 | 120, 120, 120, 120, 120, 120, 120, 220 |
| running-left | running-left | 2 | 120, 120, 120, 120, 120, 120, 120, 220 |
| waving | waving | 3 | 140, 140, 140, 280 |
| succeeded | jumping | 4 | 140, 140, 140, 140, 280 |
| failed | failed | 5 | 140, 140, 140, 140, 140, 140, 140, 240 |
| waiting | waiting | 6 | 150, 150, 150, 150, 150, 260 |
| working | running | 7 | 120, 120, 120, 120, 120, 220 |
| review | review | 8 | 150, 150, 150, 150, 150, 280 |

The combined `all-states.gif` has 57 frames at 170 ms each;
`idle-jump-idle.gif` has 17 at 150 ms; `look-loop.gif` has 16 at 160 ms.
None overrides the per-state timings above.

Artwork is owner-supplied/generated; no broader redistribution license is
asserted by this import.
