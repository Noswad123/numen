#!/usr/bin/env python3
"""Export isolated 2x neural sprite candidates; never activate them at runtime."""

import argparse
import copy
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


REPO = Path(__file__).resolve().parents[1]
CHARACTERS = {"amethyst": "djinn", "ruby": "ruby", "saphire": "hexa-glasses", "emerald": "emerald"}
MODEL = "realesr-animevideov3"
RELEASE = "https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesrgan-ncnn-vulkan-20220424-ubuntu.zip"


def original_source(character):
    """After promotion, regenerate from immutable 1x backups, never from active HD."""
    backup = REPO / "artwork/original-1x" / character
    if (REPO / "artwork/hd-promotion/original-removal.json").exists():
        raise ValueError("1x originals intentionally removed; supply original sources to regenerate")
    if backup.exists():
        if not all((backup / name).is_file() for name in ("spritesheet.png", "manifest.json")):
            raise ValueError(f"Incomplete original artwork backup: {backup}")
        source = backup
    else:
        source = REPO / "assets" / CHARACTERS[character]
    manifest = json.loads((source / "manifest.json").read_bytes())
    if tuple(manifest[key] for key in ("width", "height", "cell_width", "cell_height")) != (1536, 2288, 192, 208):
        raise ValueError("regeneration requires original 1x geometry, never active HD")
    dimensions = run(["magick", "identify", "-format", "%w %h", source / "spritesheet.png"]).decode()
    if dimensions != "1536 2288":
        raise ValueError("regeneration requires original 1x PNG geometry")
    return source


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(args, data=None, timeout=120):
    return subprocess.run([str(arg) for arg in args], input=data, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, check=True, timeout=timeout).stdout


def scaled_manifest(manifest):
    result = copy.deepcopy(manifest)
    for key in ("width", "height", "cell_width", "cell_height"):
        result[key] *= 2
    return result


def cell(raw, width, x, y, cw, ch):
    return b"".join(raw[((y + row) * width + x) * 4:((y + row) * width + x + cw) * 4]
                    for row in range(ch))


def pack(canvas, width, frame, x, y, cw, ch):
    if len(frame) != cw * ch * 4:
        raise ValueError("Incorrect frame byte length")
    for row in range(ch):
        start = ((y + row) * width + x) * 4
        canvas[start:start + cw * 4] = frame[row * cw * 4:(row + 1) * cw * 4]


def alpha_stats(raw, width):
    alpha = raw[3::4]
    transparent = alpha.count(0)
    opaque = alpha.count(255)
    occupied = [i for i, value in enumerate(alpha) if value]
    bbox = None
    if occupied:
        bbox = [min(i % width for i in occupied), occupied[0] // width,
                max(i % width for i in occupied) + 1, occupied[-1] // width + 1]
    return {"transparent": transparent, "opaque": opaque,
            "partial": len(alpha) - transparent - opaque, "bbox": bbox}


def decode(magick, path, width, height):
    dimensions = run([magick, "identify", "-format", "%w %h", path]).decode()
    if dimensions != f"{width} {height}":
        raise ValueError(f"Unexpected image size for {path}: {dimensions}")
    raw = run([magick, path, "-alpha", "on", "-depth", "8", "rgba:-"])
    if len(raw) != width * height * 4:
        raise ValueError(f"Invalid RGBA length: {path}")
    return raw


def encode(magick, raw, width, height, path):
    run([magick, "-size", f"{width}x{height}", "-depth", "8", "rgba:-",
         "-strip", f"PNG32:{path}"], raw)


def nearest(raw, width, height):
    rows = []
    for y in range(height):
        row = b"".join(raw[(y * width + x) * 4:(y * width + x + 1) * 4] * 2
                       for x in range(width))
        rows.extend([row, row])
    return b"".join(rows)


def comparison(left, right, width, height):
    result = bytearray(width * 2 * height * 4)
    pack(result, width * 2, left, 0, 0, width, height)
    pack(result, width * 2, right, width, 0, width, height)
    # An identical light/dark checker behind each side exposes alpha fringes.
    for i in range(0, len(result), 4):
        pixel = i // 4
        x, y = pixel % (width * 2), pixel // (width * 2)
        bg = 210 if ((x % width) // 32 + y // 32) % 2 else 64
        alpha = result[i + 3]
        for channel in range(3):
            result[i + channel] = (result[i + channel] * alpha + bg * (255 - alpha) + 127) // 255
        result[i + 3] = 255
    return result


def reserve_output(root, character):
    target = root / character
    target.mkdir(parents=True, exist_ok=False)
    return target


def generate(character, source, target, tool, models, magick, temp):
    manifest = json.loads((source / "manifest.json").read_text())
    w, h, cw, ch = (manifest[key] for key in ("width", "height", "cell_width", "cell_height"))
    if w != manifest["columns"] * cw or h != manifest["rows"] * ch:
        raise ValueError("Manifest grid does not cover canvas")
    original = decode(magick, source / "spritesheet.png", w, h)
    inputs, outputs = temp / "input", temp / "output"
    inputs.mkdir()
    outputs.mkdir()
    frames = {}
    for row in range(manifest["rows"]):
        for col in range(manifest["columns"]):
            index = row * manifest["columns"] + col
            raw = cell(original, w, col * cw, row * ch, cw, ch)
            stats = alpha_stats(raw, cw)
            frames[index] = stats
            if stats["bbox"]:
                encode(magick, raw, cw, ch, inputs / f"{index:03}.png")
    command = [tool, "-i", inputs, "-o", outputs, "-m", models, "-n", MODEL,
               "-s", "2", "-j", "1:1:1", "-t", "192", "-g", "0", "-f", "png"]
    # Keep verbose GPU progress out of the terminal; retain a useful error excerpt.
    with (temp / "inference.log").open("wb") as log:
        process = subprocess.run([str(arg) for arg in command], stdout=log, stderr=log, timeout=120)
    if process.returncode:
        raise RuntimeError((temp / "inference.log").read_text(errors="replace")[-6000:])
    canvas = bytearray(w * h * 16)
    metrics = []
    for index, before in frames.items():
        row, col = divmod(index, manifest["columns"])
        if before["bbox"]:
            raw = decode(magick, outputs / f"{index:03}.png", cw * 2, ch * 2)
            after = alpha_stats(raw, cw * 2)
            if not all(after[key] for key in ("transparent", "opaque", "partial")):
                raise ValueError(f"Lost alpha categories in cell {index}")
            # Bicubic support extends two source pixels, or four output pixels.
            if not after["bbox"] or any(abs(a - b * 2) > 4 for a, b in zip(after["bbox"], before["bbox"])):
                raise ValueError(f"Alpha bounds shifted in cell {index}: {before} -> {after}")
            pack(canvas, w * 2, raw, col * cw * 2, row * ch * 2, cw * 2, ch * 2)
        else:
            after = alpha_stats(bytes(cw * ch * 16), cw * 2)
        offsets = ([a - b * 2 for a, b in zip(after["bbox"], before["bbox"])]
                   if before["bbox"] else None)
        metrics.append({"cell": index, "source": before, "candidate": after,
                        "alpha_bbox_offsets_output_pixels": offsets})
    encode(magick, canvas, w * 2, h * 2, target / "spritesheet.png")
    reloaded = decode(magick, target / "spritesheet.png", w * 2, h * 2)
    if reloaded != canvas:
        raise ValueError("Lossless PNG round-trip failed")
    for metric in metrics:
        row, col = divmod(metric["cell"], manifest["columns"])
        actual = alpha_stats(cell(reloaded, w * 2, col * cw * 2, row * ch * 2, cw * 2, ch * 2), cw * 2)
        if actual != metric["candidate"]:
            raise ValueError("Cell packing validation failed")
    derived = scaled_manifest(manifest)
    (target / "manifest.json").write_text(json.dumps(derived, indent=2) + "\n")
    if derived["clips"] != manifest["clips"]:
        raise ValueError("Clip metadata changed")
    preview = bytearray(cw * 4 * ch * 4 * 4)
    for slot, clip in enumerate(("idle", "running-right")):
        row = manifest["clips"][clip]["row"]
        left = nearest(cell(original, w, 0, row * ch, cw, ch), cw, ch)
        right = cell(canvas, w * 2, 0, row * ch * 2, cw * 2, ch * 2)
        pair = comparison(left, right, cw * 2, ch * 2)
        pack(preview, cw * 4, pair, 0, slot * ch * 2, cw * 4, ch * 2)
    encode(magick, preview, cw * 4, ch * 4, target / "comparison.png")
    gif_args = [magick]
    idle = manifest["clips"]["idle"]
    for col, duration in enumerate(idle["frame_durations_ms"]):
        left = nearest(cell(original, w, col * cw, idle["row"] * ch, cw, ch), cw, ch)
        right = cell(canvas, w * 2, col * cw * 2, idle["row"] * ch * 2, cw * 2, ch * 2)
        path = temp / f"preview-{col}.png"
        encode(magick, comparison(left, right, cw * 2, ch * 2), cw * 4, ch * 2, path)
        gif_args.extend(["-delay", str(max(1, round(duration / 10))), path])
    run(gif_args + ["-loop", "0", target / "idle-comparison.gif"])
    return {"source": str(source.relative_to(REPO)), "source_size": [w, h],
            "candidate_size": [w * 2, h * 2], "occupied_cells": sum(bool(s["bbox"]) for s in frames.values()),
            "empty_cells": sum(not s["bbox"] for s in frames.values()), "cells": metrics,
            "candidate_sha256": sha256(target / "spritesheet.png"),
            "candidate_bytes": (target / "spritesheet.png").stat().st_size,
            "output_sha256": {p.name: sha256(p) for p in sorted(target.iterdir())},
            "clips_and_timings_unchanged": True, "lossless_round_trip": True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upscaler", required=True, type=Path)
    parser.add_argument("--models", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=REPO / "artwork/upscaled-2x")
    parser.add_argument("--character", choices=CHARACTERS, action="append")
    args = parser.parse_args()
    for path in (args.upscaler, args.models, args.output):
        if not path.is_absolute():
            parser.error("Tool, model and output paths must be absolute")
    root = args.output.resolve()
    owned = REPO / "artwork/upscaled-2x"
    if root.is_relative_to(REPO) and root != owned:
        parser.error("Repository outputs are restricted to artwork/upscaled-2x")
    if not args.upscaler.is_file() or not args.models.is_dir():
        parser.error("Provide an existing isolated executable and model directory")
    magick = shutil.which("magick")
    if not magick:
        parser.error("ImageMagick is required; nothing will be installed")
    selected = args.character or list(CHARACTERS)
    if len(set(selected)) != len(selected) or any((root / name).exists() for name in selected):
        parser.error("Character output already exists or was selected twice; choose a new output root")
    sources = [original_source(character) / name for character in CHARACTERS
               for name in ("spritesheet.png", "manifest.json")]
    before = {str(path.relative_to(REPO)): sha256(path) for path in sources}
    tool_hashes = {"executable": sha256(args.upscaler)}
    for suffix in ("bin", "param"):
        tool_hashes[f"{MODEL}-x2.{suffix}"] = sha256(args.models / f"{MODEL}-x2.{suffix}")
    archive = args.upscaler.parent / "realesrgan-ncnn-vulkan-20220424-ubuntu.zip"
    if archive.is_file():
        tool_hashes["release_zip"] = sha256(archive)
    provenance_path = root / "provenance.json"
    provenance = json.loads(provenance_path.read_text()) if provenance_path.exists() else {}
    if provenance and (provenance.get("tool_sha256") != tool_hashes or provenance.get("source_sha256") != before):
        parser.error("Existing provenance uses different sources or tools; choose a new output root")
    provenance.update({"release_url": RELEASE, "release_version": "v0.2.5.0",
                       "digest_status": "Locally recorded SHA256 only; no verified upstream digest or signature",
                       "tool_sha256": tool_hashes, "source_sha256": before, "model": MODEL,
                       "parameters": {"scale": 2, "jobs": "1:1:1", "tile": 192, "gpu": 0},
                       "alpha": "Separate bicubic alpha; neural RGB; no flattening or quantization in candidate PNG",
                       "method": "Independent fixed-grid cells, including unlisted gaze rows; no trimming or recentering"})
    provenance.setdefault("characters", {})
    try:
        for character in selected:
            target = reserve_output(root, character)
            print(f"Generating {character} (individual cells, GPU 0)...", flush=True)
            with tempfile.TemporaryDirectory(prefix="numen-sprites-", dir="/tmp/opencode") as work:
                result = generate(character, original_source(character), target,
                                  args.upscaler, args.models, magick, Path(work))
            provenance["characters"][character] = result
            provenance_path.write_text(json.dumps(provenance, indent=2) + "\n")
            print(f"Validated {character}: {result['occupied_cells']} occupied, {result['empty_cells']} empty; "
                  f"{result['candidate_bytes']:,} PNG bytes", flush=True)
    finally:
        after = {str(path.relative_to(REPO)): sha256(path) for path in sources}
        if after != before:
            raise RuntimeError("Source hashes changed during generation")
    provenance["all_source_hashes_preserved"] = True
    provenance_path.write_text(json.dumps(provenance, indent=2) + "\n")
    print("All original PNG and manifest hashes unchanged. Candidates are NOT runtime-active.")


if __name__ == "__main__":
    main()
