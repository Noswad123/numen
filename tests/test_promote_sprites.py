"""Read-only recorded promotion verification and fresh-write retirement tests."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("promote_sprites", ROOT / "scripts/promote-sprites.py")
PROMOTE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROMOTE)


class PromotionTests(unittest.TestCase):
    def test_exact_active_pixels_manifests_and_all_reported_hashes(self):
        paths = [ROOT / "artwork/hd-promotion/report.json", ROOT / "artwork/hd-promotion/original-removal.json",
                 ROOT / "artwork/upscaled-2x/provenance.json", ROOT / "artwork/edge-refinement/applied.json"]
        paths += list((ROOT / "assets").glob("*/spritesheet.png"))
        paths += list((ROOT / "assets").glob("*/manifest.json"))
        paths += [p for p in (ROOT / "artwork/original-1x").glob("*/*") if p.is_file()]
        before = [(p.read_bytes(), p.stat().st_mtime_ns) for p in paths]
        for check in (False, True):
            PROMOTE.promote(check=check)
            self.assertEqual(before, [(p.read_bytes(), p.stat().st_mtime_ns) for p in paths])

    def test_fresh_invocations_refuse_without_any_mutation(self):
        for populated in (False, True):
            with self.subTest(populated=populated), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                if populated:
                    candidate = root / "artwork/upscaled-2x/provenance.json"
                    candidate.parent.mkdir(parents=True)
                    candidate.write_text("invalid JSON must not be read")
                    active = root / "assets/djinn/spritesheet.png"
                    active.parent.mkdir(parents=True)
                    active.write_bytes(b"original unchanged")

                def snapshot():
                    return {str(p.relative_to(root)): (p.is_dir(), p.stat().st_mtime_ns,
                            p.read_bytes() if p.is_file() else None) for p in [root, *root.rglob("*")]}

                before = snapshot()
                for check in (False, True):
                    with self.assertRaisesRegex(ValueError, "Fresh promotion is retired; recorded promotion required"):
                        PROMOTE.promote(root=root, check=check)
                    self.assertEqual(snapshot(), before)

    def test_retirement_preserves_existing_mismatched_backup(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidates = root / "artwork/upscaled-2x"
            candidates.mkdir(parents=True)
            (candidates / "provenance.json").write_text(json.dumps({
                "source_sha256": {"assets/djinn/spritesheet.png": PROMOTE.sha(b"original")}
            }))
            backup = root / "artwork/original-1x/amethyst"
            backup.mkdir(parents=True)
            (backup / "spritesheet.png").write_bytes(b"wrong backup")
            with self.assertRaisesRegex(ValueError, "Fresh promotion is retired"):
                PROMOTE.promote(root=root)
            self.assertEqual((backup / "spritesheet.png").read_bytes(), b"wrong backup")
            self.assertFalse((root / "assets").exists())

    def test_retirement_preserves_original_without_creating_backups(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidates = root / "artwork/upscaled-2x"
            candidates.mkdir(parents=True)
            (candidates / "provenance.json").write_text(json.dumps({
                "source_sha256": {"assets/djinn/spritesheet.png": PROMOTE.sha(b"original")}
            }))
            active = root / "assets/djinn"
            active.mkdir(parents=True)
            (active / "spritesheet.png").write_bytes(b"unexpected")
            with self.assertRaisesRegex(ValueError, "Fresh promotion is retired"):
                PROMOTE.promote(root=root)
            self.assertEqual((active / "spritesheet.png").read_bytes(), b"unexpected")
            self.assertFalse((root / "artwork/original-1x").exists())

    def test_promotion_changes_only_dimensions_and_left_source(self):
        original = (ROOT / "artwork/original-1x/amethyst/manifest.json").read_bytes()
        before = json.loads(original)
        after = PROMOTE.promoted_manifest(original)
        for key in ("width", "height", "cell_width", "cell_height"):
            self.assertEqual(after[key], before[key] * 2)
            after[key] //= 2
        left = after["clips"]["running-left"]
        self.assertEqual(left.pop("source"), "running-right")
        left["row"] = 2
        self.assertEqual(after, before)

    def test_missing_original_requires_exact_removal_authorization(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            historical = {"characters": {}}
            files = []
            for character in PROMOTE.CHARACTERS:
                records = {}
                for name in ("spritesheet.png", "manifest.json", "README.md"):
                    relative = f"artwork/original-1x/{character}/{name}"
                    record = {"path": relative, "sha256": PROMOTE.sha(name.encode())}
                    records[name] = record
                    path = root / relative
                    path.parent.mkdir(parents=True, exist_ok=True)
                    if name != "spritesheet.png":
                        path.write_bytes(name.encode())
                historical["characters"][character] = {"original_backup": records}
                files.append(records["spritesheet.png"])
            directory = root / "artwork/hd-promotion"
            directory.mkdir()
            (directory / "report.json").write_text(json.dumps(historical))
            with self.assertRaisesRegex(ValueError, "missing or mismatched"):
                PROMOTE.verify_originals(root, historical)
            removal = {"files": files, "requested_by": "owner", "purpose": "approved cleanup",
                       "historical_promotion_sha256": PROMOTE.sha((directory / "report.json").read_bytes())}
            (directory / "original-removal.json").write_text(json.dumps(removal))
            PROMOTE.verify_originals(root, historical)
            restored = root / files[-1]["path"]
            restored.write_bytes(b"unexpected")
            with self.assertRaisesRegex(ValueError, "mismatched"):
                PROMOTE.verify_originals(root, historical)
            restored.unlink()
            removal = json.loads(json.dumps(removal))
            removal["files"][0]["sha256"] = "wrong"
            (directory / "original-removal.json").write_text(json.dumps(removal))
            with self.assertRaisesRegex(ValueError, "invalid original-removal"):
                PROMOTE.verify_originals(root, historical)

    def test_removed_bitmap_state_and_retained_metadata(self):
        path = ROOT / "artwork/hd-promotion/original-removal.json"
        if not path.exists():
            self.skipTest("original cleanup not executed yet")
        historical = json.loads((ROOT / "artwork/hd-promotion/report.json").read_bytes())
        PROMOTE.verify_originals(ROOT, historical)
        for item in json.loads(path.read_bytes())["files"]:
            self.assertFalse((ROOT / item["path"]).exists())


if __name__ == "__main__":
    unittest.main()
