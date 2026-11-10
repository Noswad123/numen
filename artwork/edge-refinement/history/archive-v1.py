#!/usr/bin/env python3
"""One-time, text-only v1 archive before the solid-ink migration."""
import hashlib
import importlib.util
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parents[3]
DEST = Path(__file__).resolve().parent / "v1"
EXPECTED = {
    "scripts/refine-sprite-edges.py": "c58eec6a8efec342180f23238b05d98799aa375da03f993a87074193b358b9c3",
    "artwork/edge-refinement/applied.json": "b5ea837e8cfe17e24d5607e653fad58a136af2b079666437abd9ca6f3ab318ad",
}


def main():
    for name, digest in EXPECTED.items():
        if hashlib.sha256((ROOT / name).read_bytes()).hexdigest() != digest:
            raise ValueError(f"not the known v1 stage: {name}")
    spec = importlib.util.spec_from_file_location("v1", ROOT / "scripts/refine-sprite-edges.py")
    v1 = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(v1)
    v1.check(root=ROOT)
    sources = [ROOT / name for name in EXPECTED]
    sources += [ROOT / "artwork/edge-refinement" / name for name in ("validation.md", "README.md", "report.json")]
    DEST.mkdir(exist_ok=True)
    for source in sources:
        target = DEST / source.name
        if target.exists():
            if target.read_bytes() != source.read_bytes():
                raise ValueError(f"refusing changed history: {target}")
        else:
            shutil.copyfile(source, target)
    print("Archived v1 text only:", DEST)


if __name__ == "__main__":
    main()
