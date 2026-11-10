import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "upscale_sprites", Path(__file__).resolve().parents[1] / "scripts/upscale-sprites.py")
upscale = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(upscale)


class UpscaleTests(unittest.TestCase):
    def test_promoted_hd_is_never_used_as_the_original_source(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch.object(upscale, "REPO", Path(tmp)):
            root = Path(tmp)
            active = root / "assets/djinn"
            active.mkdir(parents=True)
            metadata = {"width": 1536, "height": 2288, "cell_width": 192, "cell_height": 208}
            (active / "manifest.json").write_text(json.dumps(metadata))
            with mock.patch.object(upscale, "run", return_value=b"1536 2288"):
                self.assertEqual(upscale.original_source("amethyst"), active)
            (active / "manifest.json").write_text(json.dumps({**metadata, "width": 3072}))
            with self.assertRaisesRegex(ValueError, "1x geometry"):
                upscale.original_source("amethyst")
            backup = root / "artwork/original-1x/amethyst"
            backup.mkdir(parents=True)
            with self.assertRaises(ValueError):
                upscale.original_source("amethyst")
            (backup / "spritesheet.png").write_bytes(b"original")
            (backup / "manifest.json").write_text(json.dumps(metadata))
            with mock.patch.object(upscale, "run", return_value=b"1536 2288"):
                self.assertEqual(upscale.original_source("amethyst"), backup)
            with mock.patch.object(upscale, "run", return_value=b"3072 4576"):
                with self.assertRaisesRegex(ValueError, "1x PNG"):
                    upscale.original_source("amethyst")
            removed = root / "artwork/hd-promotion/original-removal.json"
            removed.parent.mkdir(parents=True)
            removed.write_text("{}")
            with self.assertRaisesRegex(ValueError, "1x originals intentionally removed"):
                upscale.original_source("amethyst")

    def test_manifest_only_scales_dimensions(self):
        original = {"width": 16, "height": 22, "cell_width": 2, "cell_height": 2,
                    "columns": 8, "rows": 11,
                    "clips": {"idle": {"row": 0, "frame_durations_ms": [280, 110]}}}
        scaled = upscale.scaled_manifest(original)
        self.assertEqual(scaled["width"], 32)
        self.assertEqual(scaled["height"], 44)
        self.assertEqual(scaled["cell_width"], 4)
        self.assertEqual(scaled["cell_height"], 4)
        self.assertEqual(scaled["clips"], original["clips"])
        self.assertEqual(scaled["rows"], 11)
        self.assertEqual(scaled["columns"], 8)
        self.assertEqual(original["width"], 16)

    def test_cell_registration_and_empty_padding(self):
        canvas = bytearray(8 * 6 * 4)
        frame = bytes([10, 20, 30, 255]) * 6
        upscale.pack(canvas, 8, frame, 3, 2, 2, 3)
        self.assertEqual(upscale.cell(canvas, 8, 3, 2, 2, 3), frame)
        self.assertEqual(upscale.cell(canvas, 8, 0, 0, 3, 6), bytes(3 * 6 * 4))
        with self.assertRaises(ValueError):
            upscale.pack(canvas, 8, b"", 0, 0, 2, 3)

    def test_alpha_bounds(self):
        raw = bytes([0, 0, 0, 0, 1, 2, 3, 128, 1, 2, 3, 255, 0, 0, 0, 0])
        self.assertEqual(upscale.alpha_stats(raw, 2),
                         {"transparent": 2, "opaque": 1, "partial": 1, "bbox": [0, 0, 2, 2]})
        self.assertIsNone(upscale.alpha_stats(bytes(16), 2)["bbox"])

    def test_output_refuses_overwrite(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = upscale.reserve_output(root, "amethyst")
            sentinel = target / "keep.txt"
            sentinel.write_text("unchanged")
            with self.assertRaises(FileExistsError):
                upscale.reserve_output(root, "amethyst")
            self.assertEqual(sentinel.read_text(), "unchanged")

    def test_nearest_preserves_rgba(self):
        pixel = bytes([10, 20, 30, 128])
        self.assertEqual(upscale.nearest(pixel, 1, 1), pixel * 4)


if __name__ == "__main__":
    unittest.main()
