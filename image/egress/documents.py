"""One document per kind of external reference a DOCX, ODT or RTF can carry, and per
target: OUT/<kind>--<target>.<ext>. The targets are the listener's two addresses (image/egress/listener.py): `pub`
serves content on PUBLIC:80 (a public address, 198.18.0.0/15 in the test), `int` holds the connection without
answering on PRIVATE:81 (a private address, as a cluster service would have), `tls` is HTTPS to the first; `file`
is a local file of the image. Usage: documents.py OUT [PUBLIC PRIVATE] (default 203.0.113.10 10.231.0.10)"""

import io
import os
import struct
import sys
import zipfile
import zlib

OUT = sys.argv[1]
PUB, INT = sys.argv[2:4] if len(sys.argv) > 3 else ("203.0.113.10", "10.231.0.10")


def png(rgb=(0, 160, 0), w=32, h=32):
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def targets(kind, ext, image):
    local = "file:///usr/lib/libreoffice/program/shell/donate2.png" if image else "file:///etc/passwd"
    return {
        "pub": f"http://{PUB}/{kind}.{ext}",
        "int": f"http://{INT}:81/{kind}.{ext}",
        "tls": f"https://{PUB}/{kind}.{ext}",
        "file": local,
    }


W = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"'
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def para(text):
    return f'<w:p><w:r><w:t xml:space="preserve">{text}</w:t></w:r></w:p>'


def field(instr, result=""):
    return (
        f'<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> {instr} </w:instrText></w:r>'
        f'<w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>{result}</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>'
    )


def drawing(blip):
    return (
        '<w:p><w:r><w:drawing><wp:inline><wp:extent cx="914400" cy="914400"/><wp:docPr id="1" name="p"/>'
        '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic>'
        '<pic:nvPicPr><pic:cNvPr id="1" name="p"/><pic:cNvPicPr/></pic:nvPicPr>'
        f"<pic:blipFill><a:blip {blip}/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"
        '<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="914400" cy="914400"/></a:xfrm><a:prstGeom prst="rect"/></pic:spPr>'
        "</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
    )


def write_docx(path, body, rels=(), settings_extra="", parts=None, doctype=""):
    """rels: (id, type, target, external)."""
    parts = dict(parts or {})
    ct = (
        '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/>'
        '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
        '<Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>'
        + "".join(f'<Override PartName="/{p}" ContentType="{c}"/>' for p, (c, _) in parts.items())
        + "</Types>"
    )
    srels = [r for r in rels if r[0].startswith("s")]
    drels = [r for r in rels if not r[0].startswith("s")]

    def relxml(rs):
        return (
            '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            + "".join(
                f'<Relationship Id="{i}" Type="{REL}/{t}" Target="{g}"' + (' TargetMode="External"' if e else "") + "/>"
                for i, t, g, e in rs
            )
            + "</Relationships>"
        )

    drels = [("rIdSettings", "settings", "settings.xml", False)] + drels
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", ct)
        z.writestr("_rels/.rels", relxml([("rId1", "officeDocument", "word/document.xml", False)]))
        z.writestr(
            "word/document.xml",
            f'<?xml version="1.0" encoding="UTF-8" standalone="no"?>{doctype}<w:document {W}><w:body>{para("EGRESS TEST")}{body}</w:body></w:document>',
        )
        z.writestr("word/_rels/document.xml.rels", relxml(drels))
        z.writestr("word/settings.xml", f'<?xml version="1.0"?><w:settings {W}>{settings_extra}</w:settings>')
        if srels:
            z.writestr("word/_rels/settings.xml.rels", relxml(srels))
        z.writestr("word/media/fallback.png", png())
        for p, (_, data) in parts.items():
            z.writestr(p, data)


def chart_part(ext_rid):
    return (
        '<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" '
        'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        '<c:chart><c:plotArea><c:layout/><c:barChart><c:barDir val="col"/><c:grouping val="clustered"/>'
        '<c:ser><c:idx val="0"/><c:order val="0"/><c:val><c:numRef><c:f>[1]Sheet1!$A$1:$A$2</c:f><c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="2"/>'
        '<c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser>'
        '<c:axId val="1"/><c:axId val="2"/></c:barChart><c:catAx><c:axId val="1"/><c:scaling/><c:axPos val="b"/><c:crossAx val="2"/></c:catAx>'
        '<c:valAx><c:axId val="2"/><c:scaling/><c:axPos val="l"/><c:crossAx val="1"/></c:valAx></c:plotArea></c:chart>'
        f'<c:externalData r:id="{ext_rid}"><c:autoUpdate val="1"/></c:externalData></c:chartSpace>'
    )


def workbook(u):
    """An .xlsx that reaches for u twice on a recalculation, which fullCalcOnLoad asks for: a WEBSERVICE formula and
    a cell of an external workbook (an externalLink part with an external target)."""
    x = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    rel = '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr(
            "[Content_Types].xml",
            '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
            '<Override PartName="/xl/externalLinks/externalLink1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml"/>'
            "</Types>",
        )
        z.writestr(
            "_rels/.rels",
            rel + f'<Relationship Id="rId1" Type="{REL}/officeDocument" Target="xl/workbook.xml"/></Relationships>',
        )
        z.writestr(
            "xl/workbook.xml",
            f'<?xml version="1.0"?><workbook xmlns="{x}" xmlns:r="{REL}"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>'
            '<externalReferences><externalReference r:id="rId2"/></externalReferences><calcPr fullCalcOnLoad="1"/></workbook>',
        )
        z.writestr(
            "xl/_rels/workbook.xml.rels",
            rel + f'<Relationship Id="rId1" Type="{REL}/worksheet" Target="worksheets/sheet1.xml"/>'
            f'<Relationship Id="rId2" Type="{REL}/externalLink" Target="externalLinks/externalLink1.xml"/></Relationships>',
        )
        z.writestr(
            "xl/worksheets/sheet1.xml",
            f'<?xml version="1.0"?><worksheet xmlns="{x}"><sheetData><row r="1">'
            f'<c r="A1" t="str"><f>WEBSERVICE("{u}")</f><v>old</v></c><c r="B1"><f>[1]Sheet1!A1</f><v>1</v></c>'
            "</row></sheetData></worksheet>",
        )
        z.writestr(
            "xl/externalLinks/externalLink1.xml",
            f'<?xml version="1.0"?><externalLink xmlns="{x}" xmlns:r="{REL}"><externalBook r:id="rId1"><sheetNames>'
            '<sheetName val="Sheet1"/></sheetNames><sheetDataSet><sheetData sheetId="0"><row r="1"><cell r="A1"><v>1</v></cell>'
            "</row></sheetData></sheetDataSet></externalBook></externalLink>",
        )
        z.writestr(
            "xl/externalLinks/_rels/externalLink1.xml.rels",
            rel
            + f'<Relationship Id="rId1" Type="{REL}/externalLinkPath" Target="{u}" TargetMode="External"/></Relationships>',
        )
    return out.getvalue()


def docx_kinds():
    yield (
        "d-linked-image",
        "png",
        True,
        lambda u: dict(body=drawing('r:link="rIdL"'), rels=[("rIdL", "image", u, True)]),
    )
    yield (
        "d-linked-image-x10",
        "png",
        True,
        lambda u: dict(
            body="".join(drawing(f'r:link="rIdL{i}"') for i in range(10)),
            rels=[(f"rIdL{i}", "image", u.replace(".png", f"-{i}.png"), True) for i in range(10)],
        ),
    )
    yield (
        "d-embed-and-link",
        "png",
        True,
        lambda u: dict(
            body=drawing('r:embed="rIdE" r:link="rIdL"'),
            rels=[("rIdE", "image", "media/fallback.png", False), ("rIdL", "image", u, True)],
        ),
    )
    yield (
        "d-vml-imagedata",
        "png",
        True,
        lambda u: dict(
            body='<w:p><w:r><w:pict><v:shape style="width:72pt;height:72pt"><v:imagedata r:id="rIdL"/></v:shape></w:pict></w:r></w:p>',
            rels=[("rIdL", "image", u, True)],
        ),
    )
    yield "d-includepicture", "png", True, lambda u: dict(body=field(f'INCLUDEPICTURE "{u}" \\d'))
    yield "d-includepicture-merge", "png", True, lambda u: dict(body=field(f'INCLUDEPICTURE "{u}" \\* MERGEFORMATINET'))
    yield "d-includetext", "txt", False, lambda u: dict(body=field(f'INCLUDETEXT "{u}"', "old result"))
    yield "d-includetext-docx", "docx", False, lambda u: dict(body=field(f'INCLUDETEXT "{u}"', "old result"))
    yield (
        "d-template",
        "dotx",
        False,
        lambda u: dict(
            body=para("template"),
            settings_extra='<w:attachedTemplate r:id="sIdT"/>',
            rels=[("sIdT", "attachedTemplate", u, True)],
        ),
    )
    yield (
        "d-ole-link",
        "xlsx",
        False,
        lambda u: dict(
            body=(
                '<w:p><w:r><w:object w:dxaOrig="1440" w:dyaOrig="1440"><v:shape id="ole1" style="width:72pt;height:72pt">'
                '<v:imagedata r:id="rIdE"/></v:shape><o:OLEObject Type="Link" ProgID="Excel.Sheet.12" ShapeID="ole1" DrawAspect="Content" '
                'r:id="rIdO" UpdateMode="Always"><o:LinkType>EnhancedMetaFile</o:LinkType></o:OLEObject></w:object></w:r></w:p>'
            ),
            rels=[("rIdE", "image", "media/fallback.png", False), ("rIdO", "oleObject", u, True)],
        ),
    )
    # An embedded spreadsheet is loaded by Calc, which the image carries so that its cells are drawn.
    yield (
        "d-ole-embedded-sheet",
        "xlsx",
        False,
        lambda u: dict(
            body=(
                '<w:p><w:r><w:object w:dxaOrig="1440" w:dyaOrig="1440"><v:shape id="ole2" style="width:72pt;height:72pt">'
                '<v:imagedata r:id="rIdE"/></v:shape><o:OLEObject Type="Embed" ProgID="Excel.Sheet.12" ShapeID="ole2" '
                'DrawAspect="Content" ObjectID="_1" r:id="rIdP"/></w:object></w:r></w:p>'
            ),
            rels=[("rIdE", "image", "media/fallback.png", False), ("rIdP", "package", "embeddings/Sheet1.xlsx", False)],
            parts={
                "word/embeddings/Sheet1.xlsx": (
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                    workbook(u),
                ),
            },
        ),
    )
    yield (
        "d-chart-external",
        "xlsx",
        False,
        lambda u: dict(
            body=(
                '<w:p><w:r><w:drawing><wp:inline><wp:extent cx="2743200" cy="1828800"/><wp:docPr id="2" name="c"/>'
                '<a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart r:id="rIdC"/>'
                "</a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"
            ),
            rels=[("rIdC", "chart", "charts/chart1.xml", False)],
            parts={
                "word/charts/chart1.xml": (
                    "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
                    chart_part("rIdX"),
                ),
                "word/charts/_rels/chart1.xml.rels": (
                    "application/vnd.openxmlformats-package.relationships+xml",
                    f'<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdX" Type="{REL}/oleObject" Target="{u}" TargetMode="External"/></Relationships>',
                ),
            },
        ),
    )
    yield (
        "d-subdoc",
        "docx",
        False,
        lambda u: dict(body='<w:p><w:subDoc r:id="rIdS"/></w:p>', rels=[("rIdS", "subDocument", u, True)]),
    )
    yield (
        "d-altchunk",
        "docx",
        False,
        lambda u: dict(body='<w:altChunk r:id="rIdA"/>', rels=[("rIdA", "aFChunk", u, True)]),
    )
    yield "d-xxe", "dtd", False, lambda u: dict(body=para("&ext;"), doctype=f'<!DOCTYPE w:document SYSTEM "{u}">')
    yield (
        "d-hyperlink",
        "html",
        False,
        lambda u: dict(
            body='<w:p><w:hyperlink r:id="rIdH"><w:r><w:t>link</w:t></w:r></w:hyperlink></w:p>',
            rels=[("rIdH", "hyperlink", u, True)],
        ),
    )


ODT_NS = (
    'xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
    'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink" '
    'xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" office:version="1.3"'
)


def write_odt(path, body):
    with zipfile.ZipFile(path, "w") as z:
        z.writestr(zipfile.ZipInfo("mimetype"), "application/vnd.oasis.opendocument.text")
        z.writestr(
            "META-INF/manifest.xml",
            '<?xml version="1.0"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.3"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>',
        )
        z.writestr(
            "content.xml",
            f'<?xml version="1.0" encoding="UTF-8"?><office:document-content {ODT_NS}><office:body><office:text><text:p>EGRESS TEST</text:p>{body}</office:text></office:body></office:document-content>',
        )


def odt_kinds():
    yield (
        "o-linked-image",
        "png",
        True,
        lambda u: (
            '<text:p><draw:frame svg:width="2cm" svg:height="2cm" text:anchor-type="as-char">'
            f'<draw:image xlink:href="{u}" xlink:type="simple" xlink:show="embed" xlink:actuate="onLoad"/></draw:frame></text:p>'
        ),
    )
    yield (
        "o-section-link",
        "txt",
        False,
        lambda u: (
            f'<text:section text:name="S1"><text:section-source xlink:href="{u}" xlink:type="simple"/>'
            "<text:p>old</text:p></text:section>"
        ),
    )
    yield (
        "o-section-link-docx",
        "docx",
        False,
        lambda u: (
            f'<text:section text:name="S1"><text:section-source xlink:href="{u}" xlink:type="simple"/>'
            "<text:p>old</text:p></text:section>"
        ),
    )
    yield (
        "o-ole-link",
        "ods",
        False,
        lambda u: (
            '<text:p><draw:frame svg:width="2cm" svg:height="2cm" text:anchor-type="as-char">'
            f'<draw:object xlink:href="{u}" xlink:type="simple" xlink:show="embed" xlink:actuate="onLoad"/></draw:frame></text:p>'
        ),
    )


def rtf_kinds():
    yield (
        "r-includepicture",
        "png",
        True,
        lambda u: '{\\field{\\*\\fldinst INCLUDEPICTURE "' + u + '" \\\\d}{\\fldrslt }}\\par',
    )
    yield "r-includetext", "txt", False, lambda u: '{\\field{\\*\\fldinst INCLUDETEXT "' + u + '"}{\\fldrslt old}}\\par'
    yield "r-template", "dotx", False, lambda u: None  # header control word, see below


os.makedirs(OUT, exist_ok=True)
for kind, ext, image, make in docx_kinds():
    for t, u in targets(kind, ext, image).items():
        if (
            t == "tls"
            and kind not in ("d-linked-image", "d-includepicture", "d-includetext")
            or kind == "d-linked-image-x10"
            and t not in ("pub", "int")
        ):
            continue
        write_docx(f"{OUT}/{kind}--{t}.docx", **make(u))
for kind, ext, image, make in odt_kinds():
    for t, u in targets(kind, ext, image).items():
        if t == "tls":
            continue
        write_odt(f"{OUT}/{kind}--{t}.odt", make(u))
for kind, ext, image, make in rtf_kinds():
    for t, u in targets(kind, ext, image).items():
        if t == "tls":
            continue
        if kind == "r-template":
            src = "{\\rtf1\\ansi{\\*\\template " + u + "}EGRESS TEST\\par}"
        else:
            src = "{\\rtf1\\ansi EGRESS TEST\\par " + make(u) + "}"
        open(f"{OUT}/{kind}--{t}.rtf", "w").write(src)
print(len(os.listdir(OUT)), "documents")
