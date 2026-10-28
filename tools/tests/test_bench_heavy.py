"""heavy.py: the streamed PNG and DOCX are valid, carry what they say, and are the same bytes twice."""

import hashlib
import os
import re
import struct
import sys
import tempfile
import unittest
import zipfile
import zlib
from xml.dom import minidom

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bench"))

import heavy  # noqa: E402


def png_chunks(data):
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    at, chunks = 8, []
    while at < len(data):
        (size,) = struct.unpack(">I", data[at : at + 4])
        kind, body = data[at + 4 : at + 8], data[at + 8 : at + 8 + size]
        (crc,) = struct.unpack(">I", data[at + 8 + size : at + 12 + size])
        assert crc == zlib.crc32(kind + body) & 0xFFFFFFFF, kind
        chunks.append((kind, body))
        at += 12 + size
    return chunks


def png_rows(data):
    chunks = png_chunks(data)
    width, height, depth, colour = struct.unpack(">IIBB", chunks[0][1][:10])
    raw = zlib.decompress(b"".join(body for kind, body in chunks if kind == b"IDAT"))
    stride = width * {0: 1, 2: 3}[colour] + 1
    return width, height, depth, colour, [raw[i * stride : (i + 1) * stride] for i in range(height)]


class Png(unittest.TestCase):
    def test_decodes_to_the_side_with_a_frame_and_a_diagonal(self):
        width, height, depth, colour, rows = png_rows(heavy.flat_png(300))
        self.assertEqual((width, height, depth, colour), (300, 300, 8, 0))
        self.assertEqual(len(rows), 300)
        self.assertTrue(all(row[0] == 0 and len(row) == 301 for row in rows))  # filter byte 0
        self.assertEqual(rows[0][1:], b"\x00" * 300)  # step 1: the top row is all dots
        self.assertEqual(rows[150][1 + 150], 0)  # the diagonal
        self.assertEqual(rows[150][1 + 149], 255)

    def test_rgb_has_three_bytes_a_pixel(self):
        width, _, depth, colour, rows = png_rows(heavy.flat_png(100, channels=3))
        self.assertEqual((width, depth, colour), (100, 8, 2))
        self.assertEqual(len(rows[50]), 1 + 300)
        self.assertEqual(rows[50][1 + 150 : 1 + 153], b"\x00\x00\x00")  # the diagonal, all three channels
        self.assertEqual(rows[50][1 + 147 : 1 + 150], b"\xff\xff\xff")

    def test_variants_differ_and_each_is_stable(self):
        self.assertEqual(heavy.flat_png(500, 1), heavy.flat_png(500, 1))
        self.assertNotEqual(heavy.flat_png(500, 0), heavy.flat_png(500, 1))

    def test_a_large_side_stays_small(self):
        data = heavy.flat_png(8000)  # 64 megapixels
        self.assertLess(len(data), 200_000)
        self.assertEqual(struct.unpack(">II", png_chunks(data)[0][1][:8]), (8000, 8000))


def docx(body, images=()):
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "a.docx")
        heavy.write_docx(path, body, images)
        with open(path, "rb") as f:
            raw = f.read()
        with zipfile.ZipFile(path) as z:
            parts = {n: z.read(n) for n in z.namelist()}
            stored = {i.filename: i.compress_type for i in z.infolist()}
    return raw, parts, stored


class Docx(unittest.TestCase):
    def test_every_xml_part_is_well_formed_and_the_document_counts_match(self):
        _, parts, _ = docx(heavy.tables_body("t", 3))
        for name, data in parts.items():
            if name.endswith(".xml") or name.endswith(".rels"):
                minidom.parseString(data)
        document = parts["word/document.xml"].decode()
        self.assertEqual(document.count("<w:tbl>"), 3)
        self.assertEqual(document.count("<w:tr>"), 3 * 20)
        self.assertEqual(document.count("<w:tc>"), 3 * 20 * 6)
        self.assertIn('w:styleId="TableGrid"', parts["word/styles.xml"].decode())

    def test_text_has_five_paragraphs_a_page_and_pages_bomb_repeats_one(self):
        _, parts, _ = docx(heavy.text_body("x", 4))
        self.assertEqual(parts["word/document.xml"].decode().count("<w:p>"), 1 + 4 * 5)
        _, bomb, _ = docx(heavy.pages_bomb_body("y", 4))
        paragraphs = re.findall(r"<w:p><w:r>.*?</w:p>", bomb["word/document.xml"].decode())
        self.assertEqual(len(paragraphs), 4 * 5)
        self.assertEqual(len(set(paragraphs)), 1)

    def test_pictures_are_stored_and_referenced(self):
        images, body, meta = heavy.pixels("p", 0.04, count=2)  # two 200 x 200 PNGs
        self.assertEqual(meta, {"images": 2, "decoded_mpx": 0.08, "placed_dpi": 300})
        _, parts, stored = docx(body, images)
        self.assertEqual(parts["word/media/image2.png"], images[1][0])
        self.assertEqual(stored["word/media/image1.png"], zipfile.ZIP_STORED)
        rels = parts["word/_rels/document.xml.rels"].decode()
        self.assertIn(
            'Id="rId101" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" '
            'Target="media/image2.png"',
            rels,
        )
        self.assertIn('r:embed="rId101"', parts["word/document.xml"].decode())
        self.assertIn('Extension="png"', parts["[Content_Types].xml"].decode())

    def test_same_arguments_same_bytes(self):
        one = hashlib.sha256(docx(heavy.text_body("same", 3))[0]).hexdigest()
        two = hashlib.sha256(docx(heavy.text_body("same", 3))[0]).hexdigest()
        self.assertEqual(one, two)

    def test_a_page_of_the_pages_bomb_is_under_4000_bytes(self):
        """generate.py asks for ZIP64 above 2**31 / 4000 pages."""
        page = next(p for i, p in enumerate(heavy.pages_bomb_body("pages-500000p", 2)) if i == 1)
        self.assertLess(len(page), 4000)


if __name__ == "__main__":
    unittest.main()
