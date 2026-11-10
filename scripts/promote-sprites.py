#!/usr/bin/env python3
"""Verify recorded HD promotion and the current asset stage; fresh writes are retired."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
CHARACTERS = {"amethyst": "djinn", "ruby": "ruby", "saphire": "hexa-glasses", "emerald": "emerald"}
WIDTH, HEIGHT = 3072, 4576
START, END = WIDTH * 832 * 4, WIDTH * (832 + 416) * 4
HISTORICAL_SCRIPT_SHA = "22bee9afd09f80091094e63e852df798c192c0bd2346ceaacbffd4ab14e61590"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def rgba(path):
    header = subprocess.check_output(["magick", "identify", "-format", "%w %h %[channels]", str(path)], text=True)
    if header.split()[:3] != ["3072", "4576", "srgba"]:
        raise ValueError(f"not approved-size RGBA: {path}: {header}")
    data = subprocess.check_output(["magick", str(path), "-depth", "8", "rgba:-"])
    if len(data) != WIDTH * HEIGHT * 4:
        raise ValueError(f"invalid decoded size: {path}")
    return data


def promoted_manifest(original):
    result = json.loads(original)
    for key in ("width", "height", "cell_width", "cell_height"):
        result[key] *= 2
    left = result["clips"]["running-left"]
    if left.pop("row") != 2:
        raise ValueError("unexpected original running-left row")
    left["source"] = "running-right"
    return result


def load_refinement():
    spec = importlib.util.spec_from_file_location("refine_edges", Path(__file__).with_name("refine-sprite-edges.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def verify_originals(root, report):
    """Missing bitmaps require the exact owner-authorized four-file removal record."""
    expected = [report["characters"][c]["original_backup"]["spritesheet.png"] for c in CHARACTERS]
    for character, record in zip(CHARACTERS, expected):
        if record["path"] != f"artwork/original-1x/{character}/spritesheet.png":
            raise ValueError("unexpected original bitmap path")
    removal_path = root / "artwork/hd-promotion/original-removal.json"
    removal = json.loads(removal_path.read_bytes()) if removal_path.exists() else None
    if removal is not None and (removal.get("files") != expected or
            removal.get("requested_by") != "owner" or not removal.get("purpose") or
            removal.get("historical_promotion_sha256") != sha((root / "artwork/hd-promotion/report.json").read_bytes())):
        raise ValueError("invalid original-removal authorization")
    for character in CHARACTERS:
        for name, record in report["characters"][character]["original_backup"].items():
            if record["path"] != f"artwork/original-1x/{character}/{name}":
                raise ValueError("unexpected original metadata path")
            path = root / record["path"]
            if not path.exists() and name == "spritesheet.png" and removal is not None:
                continue
            if not path.is_file() or sha(path.read_bytes()) != record["sha256"]:
                raise ValueError(f"missing or mismatched original backup: {path}")


def verify_historical(root=ROOT):
    """Reconstruct the historical promoted RGBA, without requiring it to remain active."""
    report = json.loads((root / "artwork/hd-promotion/report.json").read_bytes())
    if report["promotion_script"] != {"path": "scripts/promote-sprites.py", "sha256": HISTORICAL_SCRIPT_SHA}:
        raise ValueError("unknown historical promotion schema/script")
    if set(report["characters"]) != set(CHARACTERS):
        raise ValueError("unexpected historical character set")
    provenance_path = root / "artwork/upscaled-2x/provenance.json"
    if report["candidate_provenance"] != {"path": "artwork/upscaled-2x/provenance.json", "sha256": sha(provenance_path.read_bytes())}:
        raise ValueError("candidate provenance changed")
    provenance = json.loads(provenance_path.read_bytes())
    verify_originals(root, report)
    for character, directory in CHARACTERS.items():
        entry = report["characters"][character]
        for name, digest in provenance["characters"][character]["output_sha256"].items():
            if Path(name).name != name or sha((root / "artwork/upscaled-2x" / character / name).read_bytes()) != digest:
                raise ValueError(f"candidate artifact changed: {character}/{name}")
        for name, record in entry["candidate"].items():
            if record["path"] != f"artwork/upscaled-2x/{character}/{name}" or sha((root / record["path"]).read_bytes()) != record["sha256"]:
                raise ValueError(f"candidate changed: {character}/{name}")
        original = json.loads((root / f"artwork/original-1x/{character}/manifest.json").read_bytes())
        candidate_manifest = json.loads((root / f"artwork/upscaled-2x/{character}/manifest.json").read_bytes())
        for key in ("width", "height", "cell_width", "cell_height"):
            original[key] *= 2
        if candidate_manifest != original:
            raise ValueError("candidate manifest differs from original grid/timings")
        manifest_path = root / f"assets/{directory}/manifest.json"
        if entry["active"]["manifest.json"] != {"path": str(manifest_path.relative_to(root)), "sha256": sha(manifest_path.read_bytes())}:
            raise ValueError("active manifest changed")
        expected = promoted_manifest((root / f"artwork/original-1x/{character}/manifest.json").read_bytes())
        if json.loads(manifest_path.read_bytes()) != expected:
            raise ValueError("active aliases/grid/timings changed")
        source = rgba(root / entry["candidate"]["spritesheet.png"]["path"])
        baseline = source[:START] + bytes(END - START) + source[END:]
        if (sha(source) != entry["candidate_rgba_sha256"] or sha(baseline) != entry["active_rgba_sha256"] or
                sha(source[:START] + source[END:]) != entry["outside_retired_row_sha256"] or
                sha(baseline[START:END]) != entry["blank_row_rgba_sha256"]):
            raise ValueError(f"historical pixel reconstruction failed: {character}")
        for name in ("spritesheet.png", "manifest.json"):
            if provenance["source_sha256"][f"assets/{directory}/{name}"] != entry["original_backup"][name]["sha256"]:
                raise ValueError("original source history mismatch")
    return report


def promote(root=ROOT, check=False):
    # Keep check for callers/CLI compatibility; both invocations are read-only.
    report_path = root / "artwork/hd-promotion/report.json"
    if not report_path.exists():
        raise ValueError("Fresh promotion is retired; recorded promotion required")
    old_report = verify_historical(root)
    if (root / "artwork/edge-refinement/applied.json").exists():
        load_refinement().check(root=root, historical=old_report)
    else:
        for character in CHARACTERS:
            entry = old_report["characters"][character]
            path = root / entry["active"]["spritesheet.png"]["path"]
            if sha(path.read_bytes()) != entry["active"]["spritesheet.png"]["sha256"] or sha(rgba(path)) != entry["active_rgba_sha256"]:
                raise ValueError(f"active differs from historical promotion: {path}")
    print("Verified historical inputs and current active stage")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify backups, candidates, active pixels and report without writing")
    args = parser.parse_args()
    promote(check=args.check)
