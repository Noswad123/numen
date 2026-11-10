#!/usr/bin/env python3
"""Guarded contour refinement from immutable HD candidates; never rewrites manifests."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
ALPHA_LUT = bytes(0 if a <= 4 else 255 if a >= 251 else
                  round(255 * a * a / (a * a + (255 - a) ** 2)) for a in range(256))
PROFILES = ("alpha", "contour", "contour-v1", "crisp")
V1_RECIPE = {"version": 1, "profile": "contour", "cell_size": [384, 416],
          "alpha": "round(255*a*a/(a*a+(255-a)**2)); input<=4 zero; input>=251 opaque",
          "radius": 1, "max_channel": 96, "max_luminance": 72,
          "luminance_weights": [.2126, .7152, .0722], "boundary": "input alpha<=127",
          "internal_blackhat": False}
RECIPE = {**V1_RECIPE, "version": 2,
          "ink_alpha_override": "255 only for contour mask AND BASELINE alpha>=128; otherwise alpha LUT"}
V1_SCRIPT_SHA = "c58eec6a8efec342180f23238b05d98799aa375da03f993a87074193b358b9c3"
V1_REPORT_SHA = "b5ea837e8cfe17e24d5607e653fad58a136af2b079666437abd9ca6f3ab318ad"


def _promotion():
    spec = importlib.util.spec_from_file_location("promotion", Path(__file__).with_name("promote-sprites.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _sha(raw):
    return hashlib.sha256(raw).hexdigest()


def _validate(raw, width, height, profile):
    if width <= 0 or height <= 0 or len(raw) != width * height * 4:
        raise ValueError("expected positive dimensions and exactly width*height*4 RGBA bytes")
    if profile not in PROFILES:
        raise ValueError(f"profile must be one of {PROFILES}")


def _gray_morph(raw, width, height, operation, radius=2):
    return subprocess.run(
        ["magick", "-limit", "thread", "1", "-size", f"{width}x{height}",
         "-depth", "8", "gray:-", "-virtual-pixel", "edge", "-morphology",
         operation, f"Disk:{radius}", "-depth", "8", "gray:-"],
        input=raw, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
        timeout=30).stdout


def _process(raw, width, height, profile):
    _validate(raw, width, height, profile)
    mask = bytearray(width * height)
    if not any(raw[3::4]):
        return bytes(raw), mask
    if profile in ("contour", "contour-v1"):
        boundary = _gray_morph(bytes(255 if a <= 127 else 0 for a in raw[3::4]),
                               width, height, "Dilate", radius=1)
        for p, i in enumerate(range(0, len(raw), 4)):
            r, g, b, a = raw[i:i + 4]
            mask[p] = bool(a and boundary[p] and max(r, g, b) <= 96 and
                           .2126 * r + .7152 * g + .0722 * b <= 72)
    elif profile == "crisp":
        # Ignore hidden RGB only in analysis; never flatten output against white.
        gray = bytes(round(.2126 * raw[i] + .7152 * raw[i + 1] + .0722 * raw[i + 2])
                     if raw[i + 3] else 255 for i in range(0, len(raw), 4))
        closed = _gray_morph(gray, width, height, "Close")
        boundary = _gray_morph(bytes(255 if a <= 127 else 0 for a in raw[3::4]),
                               width, height, "Dilate")
        for p, i in enumerate(range(0, len(raw), 4)):
            high, low = max(raw[i:i + 3]), min(raw[i:i + 3])
            mask[p] = bool(raw[i + 3] and high <= 80 and
                           (high - low <= 36 or high <= 32) and
                           (closed[p] - gray[p] >= 12 or boundary[p]))
    result = bytearray(raw)
    for p, i in enumerate(range(0, len(raw), 4)):
        result[i + 3] = ALPHA_LUT[raw[i + 3]]
        if mask[p]:
            result[i:i + 3] = b"\0\0\0"
            if profile == "contour" and raw[i + 3] >= 128:
                result[i + 3] = 255
    return bytes(result), mask


def refine_frame(raw: bytes, width: int, height: int, *, profile: str = "contour") -> bytes:
    """Keep registration and RGB fills; sharpen alpha and optionally existing ink."""
    return _process(raw, width, height, profile)[0]


def _cell(raw, width, x, y, cw, ch):
    return b"".join(raw[((y + row) * width + x) * 4:
                        ((y + row) * width + x + cw) * 4] for row in range(ch))


def refine_sheet(raw, width, height, cell_width, cell_height, *, profile="contour") -> bytes:
    """Refine independent fixed-grid cells, including blank and unreferenced cells."""
    _validate(raw, width, height, profile)
    if cell_width <= 0 or cell_height <= 0 or width % cell_width or height % cell_height:
        raise ValueError("cell dimensions must divide the sheet exactly")
    result = bytearray(raw)
    for y in range(0, height, cell_height):
        for x in range(0, width, cell_width):
            frame = refine_frame(_cell(raw, width, x, y, cell_width, cell_height),
                                 cell_width, cell_height, profile=profile)
            for row in range(cell_height):
                start = ((y + row) * width + x) * 4
                result[start:start + cell_width * 4] = frame[row * cell_width * 4:(row + 1) * cell_width * 4]
    return bytes(result)


def _metrics(raw, width, height):
    alpha = raw[3::4]
    points = [(p % width, p // width) for p, a in enumerate(alpha) if a]
    coverage = [(p % width, p // width) for p, a in enumerate(alpha) if a >= 128]
    return {
        "alpha": {"zero": alpha.count(0), "partial": sum(0 < a < 255 for a in alpha),
                  "opaque": alpha.count(255)},
        "visible_true_black": sum(raw[i:i + 3] == b"\0\0\0" and raw[i + 3] > 0
                                   for i in range(0, len(raw), 4)),
        "opaque_true_black": sum(raw[i:i + 4] == b"\0\0\0\xff" for i in range(0, len(raw), 4)),
        "bbox": [min(x for x, y in points), min(y for x, y in points),
                 max(x for x, y in points) + 1, max(y for x, y in points) + 1] if points else None,
        "coverage_bbox_alpha128": [min(x for x, y in coverage), min(y for x, y in coverage),
                                   max(x for x, y in coverage) + 1, max(y for x, y in coverage) + 1] if coverage else None,
    }


def _png(path, raw, width, height):
    subprocess.run(["magick", "-size", f"{width}x{height}", "-depth", "8", "rgba:-",
                    "PNG32:" + str(path)], input=raw, check=True, timeout=30)


def _baseline(root, entry, promotion):
    raw = promotion.rgba(root / entry["candidate"]["spritesheet.png"]["path"])
    return raw[:promotion.START] + bytes(promotion.END - promotion.START) + raw[promotion.END:]


def _audit_frame(before, after, width, height):
    boundary = _gray_morph(bytes(255 if a <= 127 else 0 for a in before[3::4]), width, height, "Dilate", radius=1)
    old, new = _metrics(before, width, height), _metrics(after, width, height)
    coverage_before = bytes(a >= 128 for a in before[3::4])
    coverage_after = bytes(a >= 128 for a in after[3::4])
    interior = [i for p, i in enumerate(range(0, len(before), 4)) if before[i + 3] == 255 and not boundary[p]]
    expected_alpha = bytearray(before[3::4].translate(ALPHA_LUT))
    v1 = bytearray(before)
    promotions, failed_cores = 0, 0
    for p, i in enumerate(range(0, len(before), 4)):
        r, g, b, a = before[i:i + 4]
        ink = a and boundary[p] and max(r, g, b) <= 96 and .2126 * r + .7152 * g + .0722 * b <= 72
        v1[i + 3] = ALPHA_LUT[a]
        if ink:
            v1[i:i + 3] = b"\0\0\0"
            if a >= 128:
                promotions += expected_alpha[p] < 255
                expected_alpha[p] = 255
                failed_cores += after[i:i + 4] != b"\0\0\0\xff"
    stats = {"before": old, "after": new,
             "coverage_before_sha256": _sha(coverage_before), "coverage_after_sha256": _sha(coverage_after),
             "coverage_alpha128_bit_changes": sum(a != b for a, b in zip(coverage_before, coverage_after)),
             "opaque_interior_pixels": len(interior),
             "opaque_interior_rgb_changes": sum(before[i:i + 3] != after[i:i + 3] for i in interior),
             "rgb_changes_outside_boundary": sum(before[p * 4:p * 4 + 3] != after[p * 4:p * 4 + 3]
                                                 for p, value in enumerate(boundary) if not value),
             "new_visible_pixels": sum(a == 0 and b > 0 for a, b in zip(before[3::4], after[3::4])),
             "alpha_changed_from_opaque_or_zero": sum(a != b and a in (0, 255) for a, b in zip(before[3::4], after[3::4])),
             "below128_rgba_changes_vs_v1": sum(v1[i:i + 4] != after[i:i + 4] for i in range(0, len(before), 4) if before[i + 3] < 128),
             "rgb_changes_vs_v1": sum(v1[i:i + 3] != after[i:i + 3] for i in range(0, len(before), 4)),
             "qualified_strong_ink_not_opaque_black": failed_cores,
             "solid_ink_opacity_promotions": promotions,
             "v1_opaque_true_black": new["opaque_true_black"] - promotions,
             "v1_partial_alpha": new["alpha"]["partial"] + promotions,
             "blank_rgba_unchanged": before == after if not any(before[3::4]) else None,
             "bbox_delta": [b - a for a, b in zip(old["bbox"], new["bbox"])] if old["bbox"] and new["bbox"] else None}
    if any(stats[key] for key in ("coverage_alpha128_bit_changes", "opaque_interior_rgb_changes",
                                  "rgb_changes_outside_boundary", "new_visible_pixels", "alpha_changed_from_opaque_or_zero",
                                  "below128_rgba_changes_vs_v1", "rgb_changes_vs_v1", "qualified_strong_ink_not_opaque_black")):
        raise ValueError("contour invariant failed")
    if stats["blank_rgba_unchanged"] is False or after[3::4] != expected_alpha:
        raise ValueError("blank registration or alpha recipe changed")
    return stats


def _build_sheet(raw, promotion):
    width, height = promotion.WIDTH, promotion.HEIGHT
    result, cells = bytearray(raw), []
    for y in range(0, height, 416):
        for x in range(0, width, 384):
            before = _cell(raw, width, x, y, 384, 416)
            after = refine_frame(before, 384, 416, profile="contour")
            stats = _audit_frame(before, after, 384, 416)
            stats["cell"] = len(cells)
            cells.append(stats)
            for row in range(416):
                start = ((y + row) * width + x) * 4
                result[start:start + 384 * 4] = after[row * 384 * 4:(row + 1) * 384 * 4]
    occupied = sum(c["before"]["bbox"] is not None for c in cells)
    if occupied != 65 or any(result[promotion.START:promotion.END]) or any(
            any(_cell(raw, width, (c["cell"] % 8) * 384, (c["cell"] // 8) * 416, 384, 416))
            for c in cells if c["before"]["bbox"] is None):
        raise ValueError("expected 65 occupied cells and all-zero retired row")
    totals = {}
    for stage in ("before", "after"):
        totals[stage] = {"alpha": {key: sum(c[stage]["alpha"][key] for c in cells) for key in ("zero", "partial", "opaque")},
                         "visible_true_black": sum(c[stage]["visible_true_black"] for c in cells),
                         "opaque_true_black": sum(c[stage]["opaque_true_black"] for c in cells)}
    totals["v1_comparison"] = {key: sum(c[key] for c in cells) for key in
                               ("v1_opaque_true_black", "v1_partial_alpha", "solid_ink_opacity_promotions")}
    return bytes(result), {"occupied_cells": occupied, "empty_cells": 88 - occupied, "cells": cells,
                           "totals": totals, "row2_all_rgba_zero": True,
                           "max_weak_fringe_tightening": max(abs(d) for c in cells for d in (c["bbox_delta"] or [0]))}


def _v1_history(root):
    history = root / "artwork/edge-refinement/history/v1"
    records = {name: {"path": str((history / name).relative_to(root)), "sha256": _sha((history / name).read_bytes())}
               for name in ("refine-sprite-edges.py", "applied.json", "validation.md", "README.md", "report.json")}
    if records["refine-sprite-edges.py"]["sha256"] != V1_SCRIPT_SHA or records["applied.json"]["sha256"] != V1_REPORT_SHA:
        raise ValueError("unknown or modified v1 history")
    old = json.loads((history / "applied.json").read_bytes())
    if old["schema"] != 1 or old["recipe"] != V1_RECIPE or old["script_sha256"] != V1_SCRIPT_SHA:
        raise ValueError("unexpected v1 historical schema")
    return records


def _verify_current(root, historical, stage, promotion, *, v1=False):
    """Hash/decode validation only; cached audits are not a transformation replay."""
    if (stage.get("schema") != (1 if v1 else 2) or stage["recipe"] != (V1_RECIPE if v1 else RECIPE) or
            stage["script_sha256"] != (V1_SCRIPT_SHA if v1 else _sha(Path(__file__).read_bytes()))):
        raise ValueError("stale refinement recipe/script report")
    if not v1 and stage["prior_v1_history"] != _v1_history(root):
        raise ValueError("stale v1 history chain")
    if (stage["historical_promotion_sha256"] != _sha((root / "artwork/hd-promotion/report.json").read_bytes()) or
            stage["candidate_provenance_sha256"] != historical["candidate_provenance"]["sha256"] or
            set(stage["characters"]) != set(promotion.CHARACTERS)):
        raise ValueError("stale refinement input chain")
    for character in promotion.CHARACTERS:
        entry, record = historical["characters"][character], stage["characters"][character]
        path = root / entry["active"]["spritesheet.png"]["path"]
        if (record["candidate_png_sha256"] != entry["candidate"]["spritesheet.png"]["sha256"] or
                record["candidate_rgba_sha256"] != entry["candidate_rgba_sha256"] or
                record["baseline_rgba_sha256"] != entry["active_rgba_sha256"] or
                record["manifest_sha256"] != entry["active"]["manifest.json"]["sha256"]):
            raise ValueError("stale character input chain")
        if _sha(path.read_bytes()) != record["active_png_sha256"]:
            raise ValueError(f"unknown active PNG: {path}")
        raw = promotion.rgba(path)
        if _sha(raw) != record["active_rgba_sha256"] or any(raw[promotion.START:promotion.END]):
            raise ValueError(f"active RGBA mismatch: {path}")
        if [cell["cell"] for cell in record["cells"]] != list(range(88)):
            raise ValueError("incomplete cell audit")


def check(root=ROOT, *, deep=False, historical=None):
    promotion = _promotion()
    historical = promotion.verify_historical(root) if historical is None else historical
    stage = json.loads((root / "artwork/edge-refinement/applied.json").read_bytes())
    _verify_current(root, historical, stage, promotion)
    if deep:
        for character, entry in historical["characters"].items():
            expected, audit = _build_sheet(_baseline(root, entry, promotion), promotion)
            record = stage["characters"][character]
            if _sha(expected) != record["active_rgba_sha256"] or any(record[key] != value for key, value in audit.items()):
                raise ValueError(f"deep replay mismatch: {character}")
    print("Verified contour stage (" + ("deep per-cell replay" if deep else "hashes/decode; cached cell audits, no replay") + ")")
    return stage


def apply(root=ROOT):
    promotion = _promotion()
    historical = promotion.verify_historical(root)
    report_path = root / "artwork/edge-refinement/applied.json"
    prior_digest = None
    if report_path.exists():
        prior_digest = _sha(report_path.read_bytes())
        prior = json.loads(report_path.read_bytes())
        if prior.get("schema") == 2:
            return check(root=root, historical=historical)
        if prior_digest != V1_REPORT_SHA:
            raise ValueError("refusing unknown v1 migration report")
        _v1_history(root)
        _verify_current(root, historical, prior, promotion, v1=True)
        expected_active = {c: r["active_png_sha256"] for c, r in prior["characters"].items()}
    else:
        expected_active = {c: r["active"]["spritesheet.png"]["sha256"] for c, r in historical["characters"].items()}
    # Preflight ALL inputs and active baselines before staging or replacing any PNG.
    for character, entry in historical["characters"].items():
        path = root / entry["active"]["spritesheet.png"]["path"]
        expected_rgba = prior["characters"][character]["active_rgba_sha256"] if prior_digest else entry["active_rgba_sha256"]
        if _sha(path.read_bytes()) != expected_active[character] or _sha(promotion.rgba(path)) != expected_rgba:
            raise ValueError(f"refusing unknown active baseline: {path}")
    prior_history = _v1_history(root)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    stage = {"schema": 2, "recipe": RECIPE,
             "selection": "Owner requested crisper black edges; assistant selected solid contour after preview; main reviewed V2; internal-blackhat crisp rejected",
             "prior_v1_history": prior_history,
             "script_sha256": _sha(Path(__file__).read_bytes()), "python": sys.version,
             "imagemagick": subprocess.check_output(["magick", "-version"], text=True).splitlines()[0],
             "historical_promotion_sha256": _sha((root / "artwork/hd-promotion/report.json").read_bytes()),
             "candidate_provenance_sha256": historical["candidate_provenance"]["sha256"], "characters": {}}
    with tempfile.TemporaryDirectory(prefix="apply-", dir=report_path.parent) as temp:
        temp = Path(temp)
        for character, entry in historical["characters"].items():
            print(f"Refining and auditing all cells: {character}", flush=True)
            raw = _baseline(root, entry, promotion)
            result, audit = _build_sheet(raw, promotion)
            output = temp / f"{character}.png"
            _png(output, result, promotion.WIDTH, promotion.HEIGHT)
            if promotion.rgba(output) != result:
                raise ValueError("staged PNG lossless round-trip failed")
            stage["characters"][character] = {
                "candidate_png_sha256": entry["candidate"]["spritesheet.png"]["sha256"],
                "candidate_rgba_sha256": entry["candidate_rgba_sha256"], "baseline_rgba_sha256": _sha(raw),
                "manifest_sha256": entry["active"]["manifest.json"]["sha256"],
                "active_png_sha256": _sha(output.read_bytes()), "active_rgba_sha256": _sha(result), **audit}
        # Recheck the entire preflight after expensive work, before the first replace.
        promotion.verify_historical(root)
        if (prior_digest is not None and _sha(report_path.read_bytes()) != prior_digest) or (prior_digest is None and report_path.exists()):
            raise ValueError("active stage report changed during staging")
        if _v1_history(root) != prior_history:
            raise ValueError("v1 history changed during staging")
        for character, entry in historical["characters"].items():
            if _sha((root / entry["active"]["spritesheet.png"]["path"]).read_bytes()) != expected_active[character]:
                raise ValueError("active baseline changed during staging")
        for character, entry in historical["characters"].items():
            (temp / f"{character}.png").replace(root / entry["active"]["spritesheet.png"]["path"])
        _verify_current(root, historical, stage, promotion)
        staged_report = temp / "applied.json"
        staged_report.write_text(json.dumps(stage, indent=2) + "\n")
        staged_report.replace(report_path)
    return stage


def remove_originals(root=ROOT):
    """Owner-authorized cleanup only after the complete applied stage validates."""
    check(root=root)
    promotion = _promotion()
    historical = promotion.verify_historical(root)
    files = [historical["characters"][c]["original_backup"]["spritesheet.png"] for c in promotion.CHARACTERS]
    record = {"historical_promotion_sha256": _sha((root / "artwork/hd-promotion/report.json").read_bytes()),
              "requested_by": "owner", "purpose": "Remove superseded 1x bitmaps after approved contour HD validation; retain metadata and SHA history",
              "files": files}
    path = root / "artwork/hd-promotion/original-removal.json"
    if path.exists():
        if json.loads(path.read_bytes()) != record:
            raise ValueError("unexpected removal record")
    else:
        # verify_historical above has checked every original SHA before this record.
        with path.open("x") as stream:
            stream.write(json.dumps(record, indent=2) + "\n")
    for item in files:
        original = root / item["path"]
        if original.exists():
            if _sha(original.read_bytes()) != item["sha256"]:
                raise ValueError("original changed during cleanup")
            original.unlink()
    promotion.verify_originals(root, historical)
    print("Verified owner-authorized removal of exactly four original PNGs")


def preview():
    output = ROOT / "artwork/edge-refinement"
    output.mkdir(parents=True, exist_ok=True)
    promotion = _promotion()
    historical = promotion.verify_historical(ROOT)
    applied = (output / "applied.json").exists()
    if applied:
        check(historical=historical)
    previous_report = output / "report.json"
    if previous_report.exists() and "selected_profile" not in json.loads(previous_report.read_bytes()):
        archive = output / "rejected-crisp"
        archive.mkdir(exist_ok=True)
        for character in ("djinn", "ruby", "hexa-glasses", "emerald"):
            if not (archive / character).exists():
                shutil.copytree(output / character, archive / character)
        for name in ("report.json", "README.md"):
            if not (archive / name).exists():
                shutil.copy2(output / name, archive / name)
    report = {"status": "solid contour V2 applied" if applied else "solid contour V2 preview",
              "selection": "Owner requested crisper black edges; assistant selected after preview; main reviewed V2",
              "source": "immutable candidate RGBA with retired row 2 zeroed; never reprocess current active",
              "selected_profile": "contour",
              "rejected_profile": "crisp: internal blackhat ridges create scratchy creases and strap noise; historical previews in rejected-crisp/",
              "warnings": ["Nonzero-alpha bounds can tighten more than two pixels when faint tails are removed; inspect bbox_delta before approval.",
                           "Contour treatment is boundary-only; inspect fine hair tips and partial-alpha edges before approval."],
              "parameters": {"alpha": RECIPE["alpha"], "ink_alpha_override": RECIPE["ink_alpha_override"],
                             "ink": "input alpha>0 AND max RGB<=96 AND Rec.709 luminance<=72 AND Disk:1 dilation of input alpha<=127; no blackhat",
                             "contour_radius": 1, "max_channel": 96, "max_luminance": 72,
                             "luminance_weights": [.2126, .7152, .0722],
                             "columns": ["BASELINE HD", "ALPHA ONLY", "APPLIED SOLID CONTOUR" if applied else "SOLID CONTOUR"],
                             "rows": ["idle frame 0", "running-right frame 0"],
                             "scales": {"source": 1, "default-1x": .75, "logical": .5, "main-new-render": .375},
                             "resize": "nearest (point), no filtering"}, "characters": {}}
    for character in ("djinn", "ruby", "hexa-glasses", "emerald"):
        source = ROOT / "assets" / character
        manifest = json.loads((source / "manifest.json").read_bytes())
        width, height = manifest["width"], manifest["height"]
        cw, ch = manifest["cell_width"], manifest["cell_height"]
        if (width, height, cw, ch) != (3072, 4576, 384, 416):
            raise ValueError("unexpected preview geometry")
        original_id = next(c for c, directory in promotion.CHARACTERS.items() if directory == character)
        sheet = _baseline(ROOT, historical["characters"][original_id], promotion)
        active = promotion.rgba(source / "spritesheet.png") if applied else None
        _validate(sheet, width, height, "contour")
        directory = output / character
        directory.mkdir(exist_ok=True)
        frames, stats = [], {}
        for pose in ("idle", "running-right"):
            row = manifest["clips"][pose]["row"]
            before = _cell(sheet, width, 0, row * ch, cw, ch)
            alpha = refine_frame(before, cw, ch, profile="alpha")
            contour, mask = _process(before, cw, ch, "contour")
            if applied and contour != _cell(active, width, 0, row * ch, cw, ch):
                raise ValueError("preview does not match applied pixels")
            frames.append((before, alpha, contour))
            for label, data in zip(("current", "alpha", "contour"), frames[-1]):
                _png(directory / f"{pose}-{label}.png", data, cw, ch)
            opaque_outside = [i for p, i in enumerate(range(0, len(before), 4))
                              if before[i + 3] == 255 and not mask[p]]
            boundary = _gray_morph(bytes(255 if a <= 127 else 0 for a in before[3::4]), cw, ch, "Dilate", radius=1)
            interior = [i for p, i in enumerate(range(0, len(before), 4))
                        if before[i + 3] == 255 and not boundary[p]]
            old, new = _metrics(before, cw, ch), _metrics(contour, cw, ch)
            stats[pose] = {"current": old, "alpha": _metrics(alpha, cw, ch), "contour": new,
                           "matches_applied_pixels": True if applied else None,
                           "ink_mask_pixels": sum(mask),
                           "opaque_fill_outside_mask_pixels": len(opaque_outside),
                           "opaque_fill_outside_mask_rgb_changes": sum(before[i:i + 3] != contour[i:i + 3] for i in opaque_outside),
                           "opaque_interior_pixels": len(interior),
                           "opaque_interior_rgb_changes": sum(before[i:i + 3] != contour[i:i + 3] for i in interior),
                           "coverage_alpha128_bit_changes": sum((a >= 128) != (b >= 128) for a, b in zip(before[3::4], contour[3::4])),
                           "new_visible_pixels": sum(a == 0 and b > 0 for a, b in zip(before[3::4], contour[3::4])),
                           "alpha_changed_from_opaque_or_zero": sum(a != b and a in (0, 255) for a, b in zip(before[3::4], contour[3::4])),
                           "bbox_delta": [b - a for a, b in zip(old["bbox"], new["bbox"])],
                           "nonzero_bbox_within_2px": all(abs(b - a) <= 2 for a, b in zip(old["bbox"], new["bbox"])),
                           "coverage_bbox_unchanged": old["coverage_bbox_alpha128"] == new["coverage_bbox_alpha128"]}
        blank = _cell(sheet, width, 0, ch * 2, width, ch)
        stats["row2_all_rgba_zero"] = not any(blank)
        stats["blank_cell_stays_identical"] = refine_frame(bytes(cw * ch * 4), cw, ch) == bytes(cw * ch * 4)
        # Three columns per pose, two rows; backgrounds are composited only for viewing.
        comparison = b"".join(b"".join(frame[y * cw * 4:(y + 1) * cw * 4] for frame in pose)
                              for pose in frames for y in range(ch))
        transparent = directory / "comparison-transparent.png"
        _png(transparent, comparison, cw * 3, ch * 2)
        for name, color in (("checker", None), ("light", "#eeeeee"), ("dark", "#242424")):
            background = ["-size", f"{cw * 3}x{ch * 2}", "pattern:checkerboard"] if color is None else ["-size", f"{cw * 3}x{ch * 2}", "xc:" + color]
            for scale in (1, .75, .5, .375):
                subprocess.run(["magick", *background, str(transparent), "-compose", "over", "-composite",
                                "-filter", "point", "-resize", f"{scale * 100}%",
                                str(directory / f"comparison-{name}-{scale:g}x.png")], check=True, timeout=30)
        report["characters"][character] = stats
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--preview", action="store_true",
                        help="write comparisons and report only under artwork/edge-refinement")
    mode.add_argument("--apply", action="store_true", help="apply approved contour from immutable candidates; refuse unknown stages")
    mode.add_argument("--check", action="store_true", help="read-only hashes/decode and cached audits")
    mode.add_argument("--remove-originals", action="store_true", help="owner-authorized four-PNG cleanup after applied-stage verification")
    parser.add_argument("--deep", action="store_true", help="with --check, replay contour and all cell audits from candidates")
    args = parser.parse_args()
    if args.deep and not args.check:
        parser.error("--deep requires --check")
    if args.preview:
        preview()
    elif args.apply:
        apply()
    elif args.check:
        check(deep=args.deep)
    else:
        remove_originals()
