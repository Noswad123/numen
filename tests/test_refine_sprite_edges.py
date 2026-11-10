"""Isolated edge prototype tests; no GPU, Pillow, or asset writes."""
import importlib.util
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("edges", Path(__file__).resolve().parents[1] / "scripts/refine-sprite-edges.py")
EDGES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EDGES)


class EdgeTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_active_samples_match_reviewed_solid_study(self):
        root = Path(__file__).resolve().parents[1]
        if json.loads((root / "artwork/edge-refinement/applied.json").read_bytes()).get("schema") != 2:
            self.skipTest("V2 migration not executed yet")
        promotion = EDGES._promotion()
        for directory in promotion.CHARACTERS.values():
            sheet = promotion.rgba(root / f"assets/{directory}/spritesheet.png")
            for pose, row in (("idle", 0), ("running-right", 1)):
                sample = root / f"artwork/edge-refinement/solid-ink-study/{directory}/{pose}-v2.png"
                expected = EDGES.subprocess.check_output(["magick", str(sample), "-depth", "8", "rgba:-"], timeout=30)
                self.assertEqual(EDGES._cell(sheet, 3072, 0, row * 416, 384, 416), expected)

    def test_applied_assets_readonly_and_apply_idempotence(self):
        root = Path(__file__).resolve().parents[1]
        if not (root / "artwork/edge-refinement/applied.json").exists():
            self.skipTest("applied stage not present yet")
        if json.loads((root / "artwork/edge-refinement/applied.json").read_bytes()).get("schema") != 2:
            self.skipTest("V2 migration not executed yet")
        paths = [root / "artwork/edge-refinement/applied.json", root / "artwork/hd-promotion/report.json"]
        paths += [root / f"assets/{name}/spritesheet.png" for name in ("djinn", "ruby", "hexa-glasses", "emerald")]
        before = [(p.read_bytes(), p.stat().st_mtime_ns) for p in paths]
        stage = EDGES.check(root=root)
        EDGES.apply(root=root)
        self.assertEqual(before, [(p.read_bytes(), p.stat().st_mtime_ns) for p in paths])
        for record in stage["characters"].values():
            self.assertEqual((record["occupied_cells"], record["empty_cells"]), (65, 23))
            for cell in record["cells"]:
                self.assertEqual(cell["coverage_before_sha256"], cell["coverage_after_sha256"])
                for key in ("coverage_alpha128_bit_changes", "opaque_interior_rgb_changes", "rgb_changes_outside_boundary", "new_visible_pixels",
                            "below128_rgba_changes_vs_v1", "rgb_changes_vs_v1", "qualified_strong_ink_not_opaque_black"):
                    self.assertEqual(cell[key], 0)

    def test_v1_migration_refuses_stale_report(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            path = root / "artwork/edge-refinement/applied.json"
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps({"schema": 1, "unexpected": True}))
            with mock.patch.object(EDGES, "_promotion"), mock.patch.object(EDGES, "_build_sheet") as build:
                with self.assertRaisesRegex(ValueError, "unknown v1 migration report"):
                    EDGES.apply(root=root)
                build.assert_not_called()

    def test_v1_migration_refuses_modified_active_before_staging(self):
        actual_root = Path(__file__).resolve().parents[1]
        historical = json.loads((actual_root / "artwork/hd-promotion/report.json").read_bytes())
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            directory = root / "artwork/edge-refinement"
            shutil.copytree(actual_root / "artwork/edge-refinement/history/v1", directory / "history/v1")
            shutil.copyfile(directory / "history/v1/applied.json", directory / "applied.json")
            (root / "artwork/hd-promotion").mkdir()
            shutil.copyfile(actual_root / "artwork/hd-promotion/report.json", root / "artwork/hd-promotion/report.json")
            path = root / "assets/djinn/spritesheet.png"
            path.parent.mkdir(parents=True)
            path.write_bytes(b"unknown modified v1 PNG")
            promotion = mock.Mock(CHARACTERS=EDGES._promotion().CHARACTERS)
            promotion.verify_historical.return_value = historical
            with mock.patch.object(EDGES, "_promotion", return_value=promotion), mock.patch.object(EDGES, "_build_sheet") as build:
                with self.assertRaisesRegex(ValueError, "unknown active PNG"):
                    EDGES.apply(root=root)
                build.assert_not_called()
            self.assertEqual(path.read_bytes(), b"unknown modified v1 PNG")
            self.assertEqual(EDGES._sha((directory / "applied.json").read_bytes()), EDGES.V1_REPORT_SHA)

    def test_unknown_active_refused_before_building_or_writing(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            entries = {}
            for name in ("a", "b", "c", "d"):
                path = root / name
                path.write_bytes(b"known" if name != "d" else b"unknown")
                entries[name] = {"active": {"spritesheet.png": {"path": name, "sha256": EDGES._sha(b"known")}},
                                 "active_rgba_sha256": EDGES._sha(b"rgba")}
            promotion = mock.Mock()
            promotion.verify_historical.return_value = {"characters": entries}
            promotion.rgba.return_value = b"rgba"
            with mock.patch.object(EDGES, "_promotion", return_value=promotion), mock.patch.object(EDGES, "_build_sheet") as build:
                with self.assertRaisesRegex(ValueError, "unknown active baseline"):
                    EDGES.apply(root=root)
                build.assert_not_called()
            self.assertEqual((root / "a").read_bytes(), b"known")
            self.assertFalse((root / "artwork").exists())

    def test_stale_report_and_modified_active_rejected(self):
        root = Path(__file__).resolve().parents[1]
        report = root / "artwork/edge-refinement/applied.json"
        if not report.exists():
            self.skipTest("applied stage not present yet")
        stage = json.loads(report.read_bytes())
        if stage.get("schema") != 2:
            self.skipTest("V2 migration not executed yet")
        historical = json.loads((root / "artwork/hd-promotion/report.json").read_bytes())
        promotion = EDGES._promotion()
        with mock.patch.object(EDGES, "RECIPE", {"profile": "crisp"}):
            with self.assertRaisesRegex(ValueError, "stale refinement"):
                EDGES._verify_current(root, historical, stage, promotion)
        with mock.patch.object(EDGES, "_sha", side_effect=lambda raw: "unknown" if raw.startswith(b"\x89PNG") else __import__("hashlib").sha256(raw).hexdigest()):
            with self.assertRaisesRegex(ValueError, "unknown active PNG"):
                EDGES._verify_current(root, historical, stage, promotion)

    def test_lut(self):
        lut = EDGES.ALPHA_LUT
        self.assertEqual(list(lut[:5]), [0] * 5)
        self.assertEqual(list(lut[251:]), [255] * 5)
        self.assertEqual((lut[127], lut[128]), (127, 128))
        self.assertTrue(all(lut[a] + lut[255 - a] == 255 for a in range(256)))
        self.assertEqual(list(lut), sorted(lut))
        self.assertTrue(all((a >= 128) == (lut[a] >= 128) for a in range(256)))
        self.assertTrue(0 < lut[32] < 32 < 223 < lut[223] < 255)

    def test_validation_and_alpha_rgb_preservation(self):
        for raw, w, h in ((b"", 0, 1), (b"123", 1, 1)):
            with self.assertRaises(ValueError):
                EDGES.refine_frame(raw, w, h)
        with self.assertRaises(ValueError):
            EDGES.refine_frame(bytes(4), 1, 1, profile="unknown")
        raw = bytes((20, 40, 60, 64))
        result = EDGES.refine_frame(raw, 1, 1, profile="alpha")
        self.assertEqual(result[:3], raw[:3])
        self.assertTrue(0 < result[3] < 64)
        with self.assertRaises(ValueError):
            EDGES.refine_sheet(bytes(16), 2, 2, 3, 2)

    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_flat_dark_fills_and_black_body_palette_unchanged(self):
        for rgb in ((35, 40, 45), (0, 0, 0), (10, 20, 30), (70, 65, 60)):
            raw = bytes((*rgb, 255)) * 81
            self.assertEqual(EDGES.refine_frame(raw, 9, 9), raw)

    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_thin_ridge_blackens_without_repainting_fill(self):
        raw = bytearray(bytes((210, 180, 140, 255)) * 81)
        for y in range(2, 7):
            raw[(y * 9 + 4) * 4:(y * 9 + 4) * 4 + 4] = bytes((45, 40, 38, 255))
        self.assertEqual(EDGES.refine_frame(bytes(raw), 9, 9), raw)
        result = EDGES.refine_frame(bytes(raw), 9, 9, profile="crisp")
        for p in range(81):
            expected = b"\0\0\0\xff" if p % 9 == 4 and 2 <= p // 9 <= 6 else raw[p * 4:p * 4 + 4]
            self.assertEqual(result[p * 4:p * 4 + 4], expected)
        self.assertEqual(EDGES.refine_frame(bytes(raw), 9, 9, profile="crisp"), result)

    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_contour_default_preserves_opaque_interior_and_bright_edges(self):
        raw = bytearray(9 * 9 * 4)
        for y in range(1, 8):
            for x in range(1, 8):
                rgb = (20, 30, 90) if x in (1, 7) or y in (1, 7) else (45, 40, 38)
                raw[(y * 9 + x) * 4:(y * 9 + x) * 4 + 4] = bytes((*rgb, 255))
        raw[40:44] = bytes((20, 200, 80, 255))
        result = EDGES.refine_frame(bytes(raw), 9, 9)
        self.assertEqual(result, EDGES.refine_frame(bytes(raw), 9, 9, profile="contour"))
        self.assertEqual(result[40:44], raw[40:44])
        for y in range(9):
            for x in range(9):
                i = (y * 9 + x) * 4
                if 2 <= x <= 6 and 2 <= y <= 6 or not raw[i + 3]:
                    self.assertEqual(result[i:i + 4], raw[i:i + 4])
                elif (x, y) != (1, 1):
                    self.assertEqual(result[i:i + 4], b"\0\0\0\xff")

    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_entire_coverage_mask_and_canvas_support_preserved(self):
        raw = b"".join(bytes((25, 35, 80, a)) for a in range(256))
        result = EDGES.refine_frame(raw, 16, 16)
        self.assertEqual(bytes(a >= 128 for a in raw[3::4]), bytes(a >= 128 for a in result[3::4]))
        _, mask = EDGES._process(raw, 16, 16, "contour-v1")
        self.assertEqual(result[3::4], bytes(255 if mask[a] and a >= 128 else EDGES.ALPHA_LUT[a] for a in range(256)))
        self.assertTrue(any(0 < a < 255 for a in result[3::4]))
        self.assertTrue(all(not b or a for a, b in zip(raw[3::4], result[3::4])))

    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_colored_dark_ridge_is_not_ink(self):
        raw = bytearray(bytes((210, 180, 140, 255)) * 81)
        raw[160:164] = bytes((65, 15, 15, 255))
        self.assertEqual(EDGES.refine_frame(bytes(raw), 9, 9), raw)

    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_silhouette_ink_does_not_flatten_partial_rgb(self):
        raw = bytes((40, 40, 40, 64)) + bytes((90, 120, 160, 128)) + bytes(4)
        result = EDGES.refine_frame(raw, 3, 1)
        self.assertEqual(result[:3], bytes(3))
        self.assertTrue(0 < result[3] < 64)
        self.assertEqual(result[4:7], raw[4:7])
        self.assertEqual(result[7], 128)

    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_solid_core_qualifier_and_sub128_v1_identity(self):
        raw = bytes((20, 30, 90, 127, 20, 30, 90, 128, 90, 120, 160, 128, 40, 40, 40, 255, 0, 0, 0, 0))
        v1 = EDGES.refine_frame(raw, 5, 1, profile="contour-v1")
        result = EDGES.refine_frame(raw, 5, 1)
        self.assertEqual(result[:4], v1[:4])
        self.assertEqual(result[4:8], b"\0\0\0\xff")
        self.assertEqual(v1[7], 128)
        self.assertEqual(result[8:12], raw[8:12])
        self.assertEqual(result[15], 255)
        self.assertEqual(result[16:], bytes(4))
        stats = EDGES._audit_frame(raw, result, 5, 1)
        self.assertEqual(stats["solid_ink_opacity_promotions"], 1)
        self.assertGreater(stats["after"]["opaque_true_black"], stats["v1_opaque_true_black"])

    @unittest.skipUnless(shutil.which("magick"), "ImageMagick required")
    def test_row2_blank_and_fixed_cell_registration(self):
        frame = bytes((180, 50, 70, 255)) * 16
        blank = bytes(64)
        sheet = frame + frame + blank + frame
        result = EDGES.refine_sheet(sheet, 4, 16, 4, 4)
        self.assertEqual(result, sheet)
        self.assertEqual(result, EDGES.refine_sheet(sheet, 4, 16, 4, 4))
        self.assertEqual(result[128:192], blank)
        hidden = bytes((10, 20, 30, 0)) * 16
        self.assertEqual(EDGES.refine_frame(hidden, 4, 4), hidden)


if __name__ == "__main__":
    unittest.main()
