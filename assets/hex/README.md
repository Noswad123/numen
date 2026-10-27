# Hex runtime assets

`spritesheet.png` is imported without modification from
`final/spritesheet-extended.png` inside the owner's `hex-artifacts.zip`.

SHA-256: `149020622d30f9a5e7414148181426215aea4258653d85e21274f83fe592ff04`.

The atlas is 1536 × 2288 RGBA pixels, arranged as 8 columns × 11 rows
of 192 × 208 cells. The first nine rows contain animation clips; the final
two contain directional gaze poses, unused by this spike.

Use the existing alpha channel. The final atlas has chroma despill already
applied; do not chroma-key it again. Keep the full generation archive as
local provenance rather than shipping its intermediate artwork and prompts.

`manifest.json` uses project-relative data, normalized state names, and frame
timings read from `previews/final/states/*.gif`. Semantic-to-art mapping is
Numen-owned: `working` uses the running row, and `succeeded` uses jumping.

Artwork is owner-supplied/generated; no broader redistribution license is
asserted by this import.
