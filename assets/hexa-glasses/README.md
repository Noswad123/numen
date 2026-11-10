# Saphire runtime assets

## Active HD Source

The active sheet is promoted from owner-approved
`artwork/upscaled-2x/saphire/spritesheet.png`: 3072 x 4576 RGBA,
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
Original manifests/import history remain in `artwork/original-1x/saphire/`;
the old 1x PNG was owner-authorized for removal, with its SHA/path retained in
`artwork/hd-promotion/original-removal.json`.

## Original 1x Import History

The details below describe the historical original, no longer bundled as a PNG,
not the active HD derivative. Original metadata and SHA records are retained.

Saphire (`saphire`) is an owner-supplied/generated character in
Numen's embedded catalog. Character identity is independent of activity source IDs.

The runtime identity replaces Hexa (Glasses), with no legacy character-ID alias.
The directory remains `assets/hexa-glasses` to preserve artwork import provenance;
original archive paths and internal metadata names such as Hexa remain unchanged.

`spritesheet.png` is imported byte-for-byte from `hexa-glasses/spritesheet.png`
in the owner's local `/Users/jdawson/Downloads/hexa-glasses-artifacts.zip` on
2026-11-06. Only the runtime PNG is imported; prompts, sources, metadata, QA,
and previews remain in the local generation archive.

- PNG SHA-256: `0e0a7156911895f7941962bbc09f1fa942a6f6097767a68478646f3f467be2a3` (1,878,499 bytes).
- Archive SHA-256: `b08dec068de7f75907bb830a24f6c8011a55658a1162cde452d9c61e6ff47c19`.
- `hexa-glasses/pet.json` identifies `name: Hexa`, ID `pet_6aee2bb262488191bae20e94ddfcdb6a`, and `artwork_tool: built-in image_gen`. Its change note records connected black rectangular glasses replacing headphones across all 73 frames, a restored natural hair crown, and processing gestures that adjust glasses. The variant is identified by the archive path and change note, not a separate variant field.
- `hexa-glasses/request.json` records `pet_id: hexa`, `display_name: Hexa`, sprite version 2, dimensions, row order, and frame counts. These are original generation metadata, not runtime identity: Numen uses ID `saphire` and display name `Saphire`.

The PNG header confirms 1536 x 2288 pixels, 8-bit RGBA. The atlas has 8 columns
and 11 rows of 192 x 208 cells, matching the request and
`hexa-glasses/qa/extended-assembly.json`. Rows 0 through 8 contain 57 animation
frames. Rows 9 and 10 contain 16 directional gaze poses in 22.5-degree steps
from 0 through 337.5 degrees, not currently exposed as animation clips.

Keep the existing alpha channel intact; do not chroma-key the PNG again.
`hexa-glasses/qa/chroma-despill-extended.json` records edge-local chroma spill
suppression with `alpha_preserved: true`, 120,978 changed pixels, and 102,892
decontaminated pixels. Runtime atlas tests verify transparent, opaque, and
partially transparent pixels in every exposed frame, plus exact RGBA crops.

`manifest.json` is Numen-owned, with no generation-machine paths. Row order
and frame counts come from `hexa-glasses/request.json`; durations below were
read directly from each GIF's Graphic Control Extensions in
`hexa-glasses/previews/<source state>.gif`, not inferred from combined previews.
They match the existing catalog's per-state timings. Numen maps `jumping` to
`succeeded` and `running` to `working`. Loop policy is Numen-owned: waving,
succeeded, and failed play once; other clips loop.

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

All per-state GIFs are 192 x 208, mark every frame as transparent, use disposal
method 2 (restore to background), and specify infinite NETSCAPE looping. GIF
transparency is indexed/binary, not a replacement for the PNG's full alpha.
The combined `all-states.gif` is 256 x 256 with 57 frames at 170 ms and disposal
method 0; only some frames mark transparency. `idle-jump-idle.gif` has 17 frames
at 150 ms; `look-loop.gif` has 16 frames at 160 ms. Both are 192 x 208 with
transparency and disposal method 2. All combined previews also loop infinitely.
None overrides per-state timing or Numen's one-shot policy.

Artwork is owner-supplied/generated; no broader redistribution license is
asserted by this import.
