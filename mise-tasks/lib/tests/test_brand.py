"""Check that brand generation keeps assets/brand and its manifest consistent."""

import json
import shutil
import struct
import tempfile
import unittest
from pathlib import Path

from spaceterm_tasks import ROOT, TaskError, brand


def fake_renderers():
    """Deterministic stand-ins for resvg, ictool, tiffutil and fonttools."""
    return brand.Renderers(
        rasterize=lambda drawing, width, height: f"{width}x{height}:{drawing}".encode(),
        icon_composer=lambda document, platform, rendition, size: (
            (f"{platform}:{rendition}:{size}:").encode()
            + (document / "Assets" / "Orbits.svg").read_bytes()
        ),
        hidpi_tiff=lambda standard, retina: standard + retina,
        outline=lambda root, palettes: "<svg/>",
    )


class BrandGenerationTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        for path in brand.SOURCES:
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(
            ROOT / brand.DOCUMENT / "icon.json", self.root / brand.DOCUMENT / "icon.json"
        )
        for path in (brand.GENERATOR, brand.WORDMARK_FONT, brand.TAGLINE_FONT):
            (self.root / path).write_bytes(path.name.encode())
        (self.root / brand.BRAND / "README.md").write_text("Brand\n")
        brand.generate(self.root, fake_renderers())

    def test_check_accepts_a_freshly_generated_tree(self):
        brand.check(self.root)

    def test_check_rejects_an_edited_output(self):
        (self.root / brand.BRAND / "logo/spaceterm-glyph.svg").write_text("<svg/>")
        with self.assertRaises(TaskError):
            brand.check(self.root)

    def test_check_rejects_an_edited_source(self):
        document = self.root / brand.DOCUMENT / "icon.json"
        document.write_text(document.read_text().replace("1.00000", "0.90000", 1))
        with self.assertRaises(TaskError):
            brand.check(self.root)

    def test_check_rejects_a_file_generation_did_not_write(self):
        (self.root / brand.BRAND / "macos/stray.png").write_bytes(b"png")
        with self.assertRaises(TaskError):
            brand.check(self.root)

    def test_generation_removes_files_it_no_longer_writes(self):
        retired = self.root / brand.BRAND / "ipados/appearances/Default.png"
        retired.parent.mkdir(parents=True)
        retired.write_bytes(b"png")
        recorded = json.loads((self.root / brand.MANIFEST).read_text())
        recorded["outputs"]["ipados/appearances/Default.png"] = "0" * 64
        (self.root / brand.MANIFEST).write_text(json.dumps(recorded))

        brand.generate(self.root, fake_renderers())

        self.assertFalse(retired.parent.parent.exists())
        brand.check(self.root)

    def test_icon_document_layers_are_generated_from_the_mark(self):
        orbits = (self.root / brand.DOCUMENT / "Assets/Orbits.svg").read_text()
        appearance = (self.root / brand.BRAND / "macos/appearances/TintedDark.png").read_bytes()
        # ictool renders the document with the artwork generation just wrote.
        self.assertTrue(appearance.endswith(orbits.encode()))


class IcoTests(unittest.TestCase):
    def test_ico_lists_every_image_with_its_size_and_location(self):
        images = [(16, b"a" * 10), (32, b"b" * 20), (256, b"c" * 30)]
        data = brand.ico(images)

        reserved, kind, count = struct.unpack_from("<HHH", data)
        self.assertEqual((reserved, kind, count), (0, 1, 3))
        for index, (size, png) in enumerate(images):
            width, height, _, _, planes, bits, length, offset = struct.unpack_from(
                "<BBBBHHII", data, 6 + 16 * index
            )
            # A stored dimension of 0 means 256 pixels.
            self.assertEqual((width or 256, height or 256), (size, size))
            self.assertEqual((planes, bits), (1, 32))
            self.assertEqual(data[offset : offset + length], png)


if __name__ == "__main__":
    unittest.main()
