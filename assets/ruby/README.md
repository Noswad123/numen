# Ruby runtime assets

## Active HD Source

The active sheet is promoted from owner-approved
`artwork/upscaled-2x/ruby/spritesheet.png`: 3072 x 4576 RGBA,
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
Original manifests/import history remain in `artwork/original-1x/ruby/`;
the old 1x PNG was owner-authorized for removal, with its SHA/path retained in
`artwork/hd-promotion/original-removal.json`.

## Original 1x Import History

The details below describe the historical original, no longer bundled as a PNG,
not the active HD derivative. Original metadata and SHA records are retained.

Ruby (`ruby`) is an owner-supplied/generated character in Numen's embedded
catalog. Character identity is independent of activity source IDs.

`spritesheet.png` is imported byte-for-byte from `ruby/spritesheet.png` in
the owner's local `/Users/jdawson/Downloads/ruby-artifacts.zip` on 2026-11-06.
Only the runtime PNG is imported; prompts, sources, QA, and previews remain
in the local generation archive.

- PNG SHA-256: `15a16848626068d4b12d6ec87f740b1383018830f03d9bd609da71837c8a1584`.
- Archive SHA-256: `b51cb1729cbf2a95cc81e6e2e1133584323a4890fc1175436419fa84269d0808`.
- `ruby/pet.json` identifies Ruby and records `artwork_tool: built-in image_gen`.
- `ruby/request.json` records sprite version 2, dimensions, row order, and counts.

The PNG header confirms 1536 x 2288 pixels, 8-bit RGBA. The atlas has 8 columns
and 11 rows of 192 x 208 cells. Rows 9 and 10 contain 16 directional gaze
poses, not currently exposed as animation clips. The archive's
`ruby/qa/extended-assembly.json` confirms that layout.

Keep the existing alpha channel intact; do not chroma-key the PNG again.
`ruby/qa/chroma-despill-extended.json` records edge-local chroma spill
suppression with `alpha_preserved: true` for the final extended atlas.

`manifest.json` is Numen-owned, with no generation-machine paths. Row order
and frame counts come from `ruby/request.json`; frame durations below were
read directly from the GIF Graphic Control Extensions in
`ruby/previews/<source state>.gif`, not inferred from the combined previews.
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

The combined `all-states.gif` uses uniform 170 ms delays, `idle-jump-idle.gif`
uses 150 ms, and `look-loop.gif` uses 160 ms. None overrides per-state timings.

Artwork is owner-supplied/generated; no broader redistribution license is
asserted by this import.
