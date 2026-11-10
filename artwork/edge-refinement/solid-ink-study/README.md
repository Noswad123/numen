# Solid Contour Study

Historical selection study, subsequently selected by the assistant and reviewed
by main for V2 application. The study itself modified no active PNG, v1 script,
existing report, manifest, candidate, or original-removal state. `report.json`
records the historical execution and its exact v1 recipe. The generator now loads
the archived v1 script, so reproducing it does not reinterpret V2 as v1.
Current asset state is in `../applied.json`. Reproduce only this study with
`python -B artwork/edge-refinement/solid-ink-study/generate.py`.

Each character directory shows idle frame 0 above running-right frame 0. Columns:

1. BASELINE: immutable HD candidate RGBA with retired row 2 cleared.
2. CONTOUR V1: reconstructed from that baseline and verified against active pixels.
3. SOLID CONTOUR V2: same RGB as v1; only selected dark boundary ink with original
   alpha at least 128 is made fully opaque.

`comparison-{light,dark}-{scale}x.png` has 384x416 cells at `1x`, 288x312 at
`0.75x`, 192x208 at `0.5x`, and 144x156 at `0.375x`, nearest-neighbor sampled.
Individual pose PNGs and the transparent three-column comparison are also included.

## Recipe And Invariants

The standalone generator imports v1 `_process` and its contour mask, not a neural
generator. V1 qualification remains existing visible dark pixels, Disk:1 of
original alpha<=127, max RGB<=96 and Rec.709 luminance<=72, without internal ink.
For qualified pixels ONLY, original alpha>=128 selects alpha255; otherwise alpha
is exactly the existing symmetric sigmoid output. V2 never expands the original
alpha-128 silhouette, recolors RGB beyond v1, or binarizes the whole sprite.

All eight sampled frames pass exact pixel checks for zero alpha-128 bit changes,
zero added visible pixels, zero source-zero/source-opaque alpha changes, zero RGB
changes from v1, zero RGBA changes below original alpha128 from v1, zero alpha
changes outside qualifying strong ink from v1, and zero opaque-interior RGB changes
outside Disk:1 from baseline. Every qualified strong ink pixel is RGBA(0,0,0,255).
The generator also passes a synthetic test covering all 256 source alpha values.
V1 stage checks pass before and after generation, and its existing hashes remain
valid. None of the removed old PNGs was recreated.

## Counts

Opaque black means exactly RGBA(0,0,0,255), not merely RGB black with partial alpha.
Counts are v1 -> v2; opaque black gains equal the partial-alpha reduction.

| Character / Pose | Opaque True Black | Partial Alpha |
| --- | --- | --- |
| Djinn idle | 318 -> 1193 | 3819 -> 2944 |
| Djinn running-right | 222 -> 1621 | 4802 -> 3403 |
| Ruby idle | 77 -> 724 | 3732 -> 3085 |
| Ruby running-right | 83 -> 611 | 4224 -> 3696 |
| Hexa idle | 264 -> 680 | 3213 -> 2797 |
| Hexa running-right | 272 -> 783 | 3672 -> 3161 |
| Emerald idle | 223 -> 914 | 3383 -> 2692 |
| Emerald running-right | 227 -> 798 | 3538 -> 2967 |

## Visual Judgment

Inspected all four characters at source/display scales. V2 gives the existing
outer cores firmer black coverage without adding interior shirt/strap scratches.
The result remains localized rather than a broad ink redraw, and the difference
is still modest at small sizes. No obvious important detail loss or strongly
jagged regression was seen in the inspected static previews. Hair pixels below
original alpha128 are byte-identical to v1; stronger hair rims may look slightly
heavier. Static samples do not establish animation stability or final acceptance.
No automatic promotion is implemented in this study.
