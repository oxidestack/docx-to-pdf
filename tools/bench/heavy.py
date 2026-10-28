"""Heavy documents: what python-docx cannot write in a session's memory, streamed. Standard library only.

- `flat_png(side, variant)`: a grayscale PNG of side × side pixels, white with a dotted frame and diagonal, written
  row by row through zlib: four gigapixels in a few megabytes, without holding the image.
- `write_docx(path, body, images)`: a minimal DOCX (content types, relationships, styles with `Heading1` and
  `TableGrid`, the document) whose `word/document.xml` is streamed from `body`, an iterable of XML strings; `images`
  are stored uncompressed as `word/media/imageN.<ext>`, referenced as `rId<100 + N>`. Every entry carries a fixed
  date, so the same arguments give the same bytes.
- Body builders: `heading`, `paragraph`, `table`, `picture`.
- The documents: `text_body` (text by pages, about 450 different words a page), `pages_bomb_body` (one paragraph
  repeated: the most pages per byte deflate allows), `tables_body` (20 × 6 tables, as generate.py's `tables`).
"""

import hashlib
import random
import struct
import zipfile
import zlib
from xml.sax.saxutils import escape

ZIP_DATE = (2026, 1, 1, 0, 0, 0)
EMU_PER_INCH = 914400
PAGE_WIDTH_IN = 6.5
WORDS = (
    "the of and to in report patient result value test sample analysis normal range date clinic laboratory "
    "method reference unit measured within above below level control protocol observation summary"
).split()

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
WP = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
A = "http://schemas.openxmlformats.org/drawingml/2006/main"
PIC = "http://schemas.openxmlformats.org/drawingml/2006/picture"
IMAGE_TYPES = {"png": "image/png", "jpeg": "image/jpeg"}
RID_BASE = 100


def seeded(name):
    return random.Random(int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "little"))


def png_chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def flat_png(side, variant=0, channels=1):
    """A side × side 8-bit PNG, grayscale (`channels` 1) or RGB (3): white, black dots every `side // 4096` pixels
    along the frame and along a diagonal shifted by `variant` (so two variants are different files), compressed a
    row at a time."""
    step = max(1, side // 4096)
    n = channels
    white = b"\x00" + b"\xff" * (side * n)  # filter byte 0, then the row
    dotted = bytearray(white)
    for x in range(0, side, step):
        dotted[1 + x * n : 1 + (x + 1) * n] = b"\x00" * n
    dotted = bytes(dotted)
    colour = {1: 0, 3: 2}[channels]
    out = [b"\x89PNG\r\n\x1a\n", png_chunk(b"IHDR", struct.pack(">IIBBBBB", side, side, 8, colour, 0, 0, 0))]
    z = zlib.compressobj(9)
    pending = []
    pending_size = 0

    def emit(data):
        nonlocal pending_size
        if data:
            pending.append(data)
            pending_size += len(data)
        if pending_size >= 1 << 20:
            out.append(png_chunk(b"IDAT", b"".join(pending)))
            pending.clear()
            pending_size = 0

    for y in range(side):
        if y == 0 or y == side - 1:
            row = dotted
        elif y % step == 0:
            row = bytearray(white)
            for x in (0, side - 1, (y + variant) % side):
                row[1 + x * n : 1 + (x + 1) * n] = b"\x00" * n
            row = bytes(row)
        else:
            row = white
        emit(z.compress(row))
    emit(z.flush())
    if pending:
        out.append(png_chunk(b"IDAT", b"".join(pending)))
    out.append(png_chunk(b"IEND", b""))
    return b"".join(out)


def heading(text):
    return f'<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>{escape(text)}</w:t></w:r></w:p>'


def paragraph(text):
    return f'<w:p><w:r><w:t xml:space="preserve">{escape(text)}</w:t></w:r></w:p>'


def table(rows, col_twips=1440):
    """A `TableGrid` table, the first row the header; `rows` a list of lists of cell strings."""
    cols = len(rows[0])
    cell = '<w:tc><w:tcPr><w:tcW w:w="%d" w:type="dxa"/></w:tcPr><w:p><w:r><w:t>%s</w:t></w:r></w:p></w:tc>'
    parts = [
        '<w:tbl><w:tblPr><w:tblStyle w:val="TableGrid"/><w:tblW w:w="0" w:type="auto"/></w:tblPr><w:tblGrid>',
        f'<w:gridCol w:w="{col_twips}"/>' * cols,
        "</w:tblGrid>",
    ]
    for row in rows:
        parts.append("<w:tr>" + "".join(cell % (col_twips, escape(v)) for v in row) + "</w:tr>")
    parts.append("</w:tbl>")
    return "".join(parts)


def picture(index, px_width, px_height, dpi=300):
    """An inline picture of image `index` (write_docx's images), placed at `dpi` but no wider than the text."""
    width_in = min(px_width / dpi, PAGE_WIDTH_IN)
    cx = int(width_in * EMU_PER_INCH)
    cy = int(cx * px_height / px_width)
    n = index + 1
    return (
        f'<w:p><w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/>'
        f'<wp:docPr id="{n}" name="Picture {n}"/><wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect="1"/>'
        f'</wp:cNvGraphicFramePr><a:graphic><a:graphicData uri="{PIC}"><pic:pic><pic:nvPicPr>'
        f'<pic:cNvPr id="{n}" name="image{n}"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill>'
        f'<a:blip r:embed="rId{RID_BASE + index}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr>'
        f'<a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/>'
        f"</a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
    )


STYLES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
    f'<w:styles xmlns:w="{W}">'
    '<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault>'
    '<w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault>'
    "</w:docDefaults>"
    '<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>'
    '<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/>'
    '<w:next w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="480" w:after="0"/><w:outlineLvl w:val="0"/>'
    '</w:pPr><w:rPr><w:b/><w:sz w:val="28"/></w:rPr></w:style>'
    '<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:tblPr>'
    '<w:tblCellMar><w:left w:w="108" w:type="dxa"/><w:right w:w="108" w:type="dxa"/></w:tblCellMar></w:tblPr>'
    "</w:style>"
    '<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/><w:basedOn w:val="TableNormal"/>'
    '<w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:tblPr><w:tblBorders>'
    + "".join(
        f'<w:{side} w:val="single" w:sz="4" w:space="0" w:color="auto"/>'
        for side in ("top", "left", "bottom", "right", "insideH", "insideV")
    )
    + "</w:tblBorders></w:tblPr></w:style></w:styles>"
)

SECTION = (
    '<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" '
    'w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr>'
)


def _entry(zf, name, data, stored=False):
    info = zipfile.ZipInfo(name, ZIP_DATE)
    info.compress_type = zipfile.ZIP_STORED if stored else zipfile.ZIP_DEFLATED
    info.external_attr = 0o644 << 16
    zf.writestr(info, data)


def write_docx(path, body, images=(), large=False):
    """Writes a DOCX at `path`: `body` (iterable of XML strings) streamed into `word/document.xml`; `images` a list
    of (bytes, "png" | "jpeg"). `large`: the document part may pass 2 GiB uncompressed (ZIP64 for that entry)."""
    images = list(images)
    types = [
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n',
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">',
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>',
        '<Default Extension="xml" ContentType="application/xml"/>',
    ]
    types += [
        f'<Default Extension="{ext}" ContentType="{IMAGE_TYPES[ext]}"/>' for ext in sorted({e for _, e in images})
    ]
    types += [
        '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.'
        'wordprocessingml.document.main+xml"/>',
        '<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.'
        'wordprocessingml.styles+xml"/>',
        "</Types>",
    ]
    rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/></Relationships>'
    )
    doc_rels = [
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="{R}/styles" Target="styles.xml"/>'
    ]
    doc_rels += [
        f'<Relationship Id="rId{RID_BASE + i}" Type="{R}/image" Target="media/image{i + 1}.{ext}"/>'
        for i, (_, ext) in enumerate(images)
    ]
    doc_rels.append("</Relationships>")
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        _entry(zf, "[Content_Types].xml", "".join(types))
        _entry(zf, "_rels/.rels", rels)
        _entry(zf, "word/_rels/document.xml.rels", "".join(doc_rels))
        _entry(zf, "word/styles.xml", STYLES)
        for i, (data, ext) in enumerate(images):
            _entry(zf, f"word/media/image{i + 1}.{ext}", data, stored=True)
        info = zipfile.ZipInfo("word/document.xml", ZIP_DATE)
        info.compress_type = zipfile.ZIP_DEFLATED
        info.external_attr = 0o644 << 16
        with zf.open(info, "w", force_zip64=large) as f:
            buffer, size = [], 0
            for piece in [
                '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n',
                f'<w:document xmlns:w="{W}" xmlns:r="{R}" xmlns:wp="{WP}" xmlns:a="{A}" xmlns:pic="{PIC}"><w:body>',
                *body,
                SECTION,
                "</w:body></w:document>",
            ]:
                buffer.append(piece)
                size += len(piece)
                if size >= 1 << 20:
                    f.write("".join(buffer).encode())
                    buffer, size = [], 0
            f.write("".join(buffer).encode())


def sentence(r, words):
    return " ".join(r.choices(WORDS, k=words)).capitalize() + "."


def text_body(name, pages):
    """`pages` pages of text: 5 paragraphs of 6 sentences of 15 words a page (about 450 words), all drawn at
    random, so deflate cannot fold them."""
    r = seeded(name)
    yield heading(f"Text, {pages} pages")
    for _ in range(pages * 5):
        yield paragraph(" ".join(sentence(r, 15) for _ in range(6)))


def pages_bomb_body(name, pages):
    """`pages` pages of the same paragraph: deflate folds the repeats, so a megabyte holds hundreds of thousands of
    pages."""
    r = seeded(name)
    one = paragraph(" ".join(sentence(r, 15) for _ in range(6)))
    yield heading(f"The same paragraph, {pages} pages")
    for _ in range(pages):
        yield one * 5


def tables_body(name, count):
    """`count` tables of 20 rows by 6 columns under a caption each, as generate.py's `tables` class."""
    r = seeded(name)
    yield heading(f"Tables, {count}")
    for t in range(count):
        yield paragraph(f"Table {t + 1}. " + sentence(r, 8))
        rows = [[f"Column {j + 1}" for j in range(6)]]
        rows += [[f"{r.randrange(100000) / 100:.2f}" for _ in range(6)] for _ in range(19)]
        yield table(rows)


def pixels(name, mpx, count=1, channels=1):
    """`count` different flat PNGs of `mpx` megapixels each (grayscale, or RGB with `channels` 3), one per page:
    (images, body, meta)."""
    side = int(round((mpx * 1_000_000) ** 0.5))
    images = [(flat_png(side, variant=v, channels=channels), "png") for v in range(count)]
    body = [heading(f"{count} flat PNG{'s' if count > 1 else ''}, {mpx} megapixels each")]
    body += [picture(i, side, side) for i in range(count)]
    dpi = side / min(side / 300, PAGE_WIDTH_IN)
    return images, body, {"images": count, "decoded_mpx": count * side * side / 1e6, "placed_dpi": round(dpi)}
