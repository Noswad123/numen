#!/usr/bin/env python3
"""Preview-only solid ink study. Never changes v1 code, reports, or active assets."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
OUTPUT = Path(__file__).resolve().parent
ROOT = OUTPUT.parents[2]
SPEC = importlib.util.spec_from_file_location("contour_v1", ROOT / "artwork/edge-refinement/history/v1/refine-sprite-edges.py")
V1 = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(V1)
CURRENT_SPEC = importlib.util.spec_from_file_location("current_contour", ROOT / "scripts/refine-sprite-edges.py")
CURRENT = importlib.util.module_from_spec(CURRENT_SPEC)
CURRENT_SPEC.loader.exec_module(CURRENT)


def solid_frame(raw, width, height):
    v1, mask = V1._process(raw, width, height, "contour")
    v2 = bytearray(v1)
    for p, selected in enumerate(mask):
        if selected and raw[p * 4 + 3] >= 128:
            v2[p * 4 + 3] = 255
    return v1, bytes(v2), mask


def metrics(raw, width, height):
    return {**V1._metrics(raw, width, height),
            "opaque_true_black": sum(raw[i:i + 4] == b"\0\0\0\xff" for i in range(0, len(raw), 4))}


def audit(raw, v1, v2, mask, width, height):
    boundary = V1._gray_morph(bytes(255 if a < 128 else 0 for a in raw[3::4]), width, height, "Dilate", radius=1)
    coverage = [bytes(a >= 128 for a in frame[3::4]) for frame in (raw, v1, v2)]
    counts = {"alpha128_bit_changes": sum(a != b for a, b in zip(coverage[0], coverage[2])),
              "new_visible_pixels": 0, "source_zero_or_opaque_alpha_changes": 0,
              "rgb_changes_vs_v1": 0, "below128_rgba_changes_vs_v1": 0,
              "nonqualifying_alpha_changes_vs_v1": 0, "qualified_strong_not_opaque_black": 0,
              "opaque_interior_rgb_changes_vs_baseline": 0, "rgb_changes_outside_disk1_vs_baseline": 0}
    for p, i in enumerate(range(0, len(raw), 4)):
        a = raw[i + 3]
        strong_ink = mask[p] and a >= 128
        counts["new_visible_pixels"] += a == 0 and v2[i + 3] > 0
        counts["source_zero_or_opaque_alpha_changes"] += a in (0, 255) and a != v2[i + 3]
        counts["rgb_changes_vs_v1"] += v1[i:i + 3] != v2[i:i + 3]
        counts["below128_rgba_changes_vs_v1"] += a < 128 and v1[i:i + 4] != v2[i:i + 4]
        counts["nonqualifying_alpha_changes_vs_v1"] += not strong_ink and v1[i + 3] != v2[i + 3]
        counts["qualified_strong_not_opaque_black"] += strong_ink and v2[i:i + 4] != b"\0\0\0\xff"
        if not boundary[p]:
            changed = raw[i:i + 3] != v2[i:i + 3]
            counts["rgb_changes_outside_disk1_vs_baseline"] += changed
            counts["opaque_interior_rgb_changes_vs_baseline"] += a == 255 and changed
    if any(counts.values()) or coverage[0] != coverage[1] or coverage[0] != coverage[2]:
        raise ValueError(f"solid contour invariant failure: {counts}")
    return {**counts, "coverage_sha256": [V1._sha(bits) for bits in coverage],
            "qualified_strong_ink_pixels": sum(bool(m and raw[p * 4 + 3] >= 128) for p, m in enumerate(mask)),
            "alpha_pixels_made_opaque_vs_v1": sum(a != b for a, b in zip(v1[3::4], v2[3::4]))}


def main():
    stage = CURRENT.check(root=ROOT)
    promotion = CURRENT._promotion()
    historical = promotion.verify_historical(ROOT)
    # These hashes must remain identical, including all immutable candidate artifacts.
    protected = [ROOT / "scripts/refine-sprite-edges.py", ROOT / "scripts/promote-sprites.py",
                 ROOT / "artwork/edge-refinement/applied.json", ROOT / "artwork/edge-refinement/report.json",
                 ROOT / "artwork/hd-promotion/report.json", ROOT / "artwork/hd-promotion/original-removal.json",
                 ROOT / "artwork/upscaled-2x/provenance.json"]
    for character, directory in promotion.CHARACTERS.items():
        protected.extend((ROOT / f"assets/{directory}").glob("*"))
        protected.extend((ROOT / f"artwork/upscaled-2x/{character}").glob("*"))
        protected.extend((ROOT / f"artwork/original-1x/{character}").glob("*"))
    before_hashes = {str(path.relative_to(ROOT)): V1._sha(path.read_bytes()) for path in protected if path.is_file()}
    # Exercise both sides of 128 and the full partial-alpha range before previews.
    synthetic = b"".join(bytes((25, 35, 80, a)) for a in range(256))
    a, b, mask = solid_frame(synthetic, 16, 16)
    audit(synthetic, a, b, mask, 16, 16)
    if not any(x != y for x, y in zip(a, b)):
        raise ValueError("synthetic solid-ink test did not exercise opacity changes")
    report = {"status": "historical selection study; current asset status is in ../applied.json", "v1_applied_sha256": CURRENT.V1_REPORT_SHA,
              "generator_sha256": V1._sha(Path(__file__).read_bytes()), "v1_recipe": V1.RECIPE,
              "v2_change": "Only contour-v1 mask pixels with ORIGINAL alpha>=128 become alpha255; all RGB identical to v1; no internal ink path",
              "columns": ["BASELINE", "CONTOUR V1", "SOLID CONTOUR V2"],
              "rows": ["idle frame 0", "running-right frame 0"],
              "cell_size": [384, 416], "scales": [1, .75, .5, .375], "sampling": "nearest / point",
              "synthetic_all_256_alpha_values": "passed", "characters": {}}
    for character, directory in promotion.CHARACTERS.items():
        sheet = V1._baseline(ROOT, historical["characters"][character], promotion)
        active = promotion.rgba(ROOT / f"assets/{directory}/spritesheet.png")
        target = OUTPUT / directory
        target.mkdir(exist_ok=True)
        poses, stats = [], {}
        for pose, row in (("idle", 0), ("running-right", 1)):
            raw = V1._cell(sheet, promotion.WIDTH, 0, row * 416, 384, 416)
            v1, v2, mask = solid_frame(raw, 384, 416)
            if (v2 if stage["schema"] == 2 else v1) != V1._cell(active, promotion.WIDTH, 0, row * 416, 384, 416):
                raise ValueError("reconstructed sample differs from current active stage")
            checks = audit(raw, v1, v2, mask, 384, 416)
            poses.append((raw, v1, v2))
            stats[pose] = {"baseline": metrics(raw, 384, 416), "v1": metrics(v1, 384, 416),
                           "v2": metrics(v2, 384, 416), "invariants": checks}
            for label, frame in zip(("baseline", "v1", "v2"), poses[-1]):
                V1._png(target / f"{pose}-{label}.png", frame, 384, 416)
        comparison = b"".join(b"".join(frame[y * 384 * 4:(y + 1) * 384 * 4] for frame in pose)
                              for pose in poses for y in range(416))
        transparent = target / "comparison-transparent.png"
        V1._png(transparent, comparison, 1152, 832)
        for background, color in (("light", "#eeeeee"), ("dark", "#242424")):
            for scale in (1, .75, .5, .375):
                subprocess.run(["magick", "-size", "1152x832", "xc:" + color, str(transparent),
                                "-compose", "over", "-composite", "-filter", "point", "-resize", f"{scale * 100}%",
                                str(target / f"comparison-{background}-{scale:g}x.png")], check=True, timeout=30)
        report["characters"][directory] = stats
    if before_hashes != {name: V1._sha((ROOT / name).read_bytes()) for name in before_hashes}:
        raise ValueError("protected source or v1 stage changed during study")
    CURRENT.check(root=ROOT)
    report["protected_hashes_unchanged"] = before_hashes
    (OUTPUT / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(OUTPUT)


if __name__ == "__main__":
    main()
