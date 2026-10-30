"""The corpus's feature documents: DOCX files this project wrote, one a feature set, under Apache-2.0.

    python3 generate.py [OUT]          (default: this script's folder)

Each document is assembled part by part from the WordprocessingML written below, so every byte is this project's
own: the text, the pictures (drawn from a seeded generator with Pillow) and the markup. The same Pillow and zlib give
the same bytes (fixed ZIP dates, a fixed part order); the corpus manifest pins each document's sha256, so a run with
other versions shows up as a changed hash, not as a silent change. Each document covers what one LibreOffice
regression document covered before (README.md beside this script names which feature each one exercises).
"""

import hashlib
import io
import os
import random
import re
import struct
import sys
import zipfile
from xml.dom import minidom
from xml.sax.saxutils import escape, quoteattr

from PIL import Image, ImageDraw, ImageFilter

ZIP_DATE = (2026, 1, 1, 0, 0, 0)
STAMP = "2026-01-01T00:00:00Z"
AUTHOR = "docx-to-pdf contributors"

# Namespaces, transitional.
NS_W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
NS_R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS_A = "http://schemas.openxmlformats.org/drawingml/2006/main"
NS_WP = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
NS_PIC = "http://schemas.openxmlformats.org/drawingml/2006/picture"
NS_C = "http://schemas.openxmlformats.org/drawingml/2006/chart"
NS_DGM = "http://schemas.openxmlformats.org/drawingml/2006/diagram"
NS_M = "http://schemas.openxmlformats.org/officeDocument/2006/math"
NS_DSP = "http://schemas.microsoft.com/office/drawing/2008/diagram"
REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/"
PKG_REL = "http://schemas.openxmlformats.org/package/2006/relationships"
CT = "application/vnd.openxmlformats-officedocument.wordprocessingml."

ROOT_NS = (
    'xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" '
    'xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" '
    'xmlns:o="urn:schemas-microsoft-com:office:office" '
    f'xmlns:r="{NS_R}" xmlns:m="{NS_M}" xmlns:v="urn:schemas-microsoft-com:vml" '
    'xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" '
    f'xmlns:wp="{NS_WP}" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w="{NS_W}" '
    'xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" '
    'xmlns:w15="http://schemas.microsoft.com/office/word/2012/wordml" '
    'xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup" '
    'xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" '
    f'xmlns:a="{NS_A}" xmlns:pic="{NS_PIC}" mc:Ignorable="w14 w15 wp14"'
)
DECL = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'

# Page sizes in twentieths of a point.
LETTER = (12240, 15840)
A4 = (11906, 16838)
A5 = (8391, 11907)
A6 = (5954, 8392)


def rng(name):
    return random.Random(int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "little"))


# ---------------------------------------------------------------------------------------------------------------
# Text. Every sentence is generated from these word lists or written here.

EN = (
    "the report shows a steady rise in measured values across every site while the control group stays within "
    "its expected range and the method agrees with earlier work on samples taken under the same protocol so the "
    "summary below lists each result beside its reference and notes where a reading falls outside the limits"
).split()

DE = [
    "Die Messung beginnt am Morgen und endet am späten Nachmittag.",
    "Jede Probe wird zweimal gewogen, bevor sie in den Ofen kommt.",
    "Für die Auswertung gelten die Grenzwerte der Prüfvorschrift.",
    "Abweichungen über fünf Prozent werden im Protokoll begründet.",
    "Die Ergebnisse stehen im Anhang, geordnet nach Datum und Ort.",
    "Wärmeübertragung und Strömung werden getrennt betrachtet.",
]
HU = [
    "A mérés reggel kezdődik és késő délután ér véget.",
    "Minden mintát kétszer mérünk meg, mielőtt a kemencébe kerül.",
    "Az eredmények a mellékletben találhatók, dátum szerint rendezve.",
]
ES = [
    "La orquesta municipal ofrece mañana un concierto gratuito en la plaza.",
    "Las entradas se reparten desde las diez en la oficina del distrito.",
    "El programa incluye obras de compositores locales y una pieza estrenada este año.",
]
AR = [
    "يبدأ القياس في الصباح وينتهي في المساء.",
    "تُوزن كل عينة مرتين قبل أن تدخل الفرن.",
    "النتائج مرتبة حسب التاريخ والمكان في الملحق.",
    "اضغط على زر التشغيل حتى تظهر الشاشة الرئيسية.",
    "يمكن إعادة تشغيل الجهاز من القائمة في أي وقت.",
]
HE = ["הדוח מציג את תוצאות המדידה לפי תאריך.", "כל דגימה נשקלת פעמיים."]
ZH = [
    "本文档说明系统的设计目标和测试方法。",
    "每个模块在发布之前都要经过单元测试和集成测试。",
    "测试结果按照日期和负责人记录在下表中。",
    "如果结果超出允许范围，需要在修订记录中说明原因。",
    "文档负责人负责审核所有修订并更新版本号。",
]
JA = [
    "彼は開いている扉の方へゆっくりと歩いて行った。",
    "この扉の向こうに何があるのか、誰も知らなかった。",
    "背中を壁に付けたまま、彼は静かに息を整えた。",
    "外では雨が降り始め、通りの明かりがにじんで見えた。",
    "やがて足音が遠ざかり、廊下には再び静けさが戻った。",
]


def sentence(r, n=None):
    n = n or r.randint(8, 16)
    words = [EN[r.randrange(len(EN))] for _ in range(n)]
    return " ".join(words).capitalize() + "."


def prose(r, sentences=5):
    return " ".join(sentence(r) for _ in range(sentences))


def x(text):
    return escape(text)


# ---------------------------------------------------------------------------------------------------------------
# WordprocessingML snippets.


def rpr(
    *,
    b=False,
    i=False,
    caps=False,
    small_caps=False,
    sz=None,
    color=None,
    font=None,
    rtl=False,
    style=None,
    vert=None,
    extra="",
):
    out = ""
    if style:
        out += f'<w:rStyle w:val="{style}"/>'
    if font:
        out += f'<w:rFonts w:ascii="{font}" w:hAnsi="{font}" w:eastAsia="{font}" w:cs="{font}"/>'
    if b:
        out += "<w:b/><w:bCs/>"
    if i:
        out += "<w:i/><w:iCs/>"
    if caps:
        out += "<w:caps/>"
    if small_caps:
        out += "<w:smallCaps/>"
    if color:
        out += f'<w:color w:val="{color}"/>'
    if sz:
        out += f'<w:sz w:val="{sz}"/><w:szCs w:val="{sz}"/>'
    if vert:
        out += f'<w:vertAlign w:val="{vert}"/>'
    if rtl:
        out += "<w:rtl/>"
    out += extra
    return f"<w:rPr>{out}</w:rPr>" if out else ""


def R(text, props="", **kw):
    """A run of text; tabs written as \t and line breaks as \n become w:tab and w:br."""
    p = props or rpr(**kw)
    parts = []
    for k, chunk in enumerate(text.split("\n")):
        if k:
            parts.append("<w:br/>")
        for j, piece in enumerate(chunk.split("\t")):
            if j:
                parts.append("<w:tab/>")
            if piece:
                parts.append(f'<w:t xml:space="preserve">{x(piece)}</w:t>')
    return f"<w:r>{p}{''.join(parts)}</w:r>"


def P(*runs, style=None, ppr="", jc=None, bidi=False, num=None, sect=""):
    pp = ""
    if style:
        pp += f'<w:pStyle w:val="{style}"/>'
    if num:
        pp += f'<w:numPr><w:ilvl w:val="{num[1]}"/><w:numId w:val="{num[0]}"/></w:numPr>'
    pp += ppr
    if bidi:
        pp += "<w:bidi/>"
    if jc:
        pp += f'<w:jc w:val="{jc}"/>'
    pp += sect
    body = "".join(r if r.startswith("<") else R(r) for r in runs)
    return f"<w:p>{f'<w:pPr>{pp}</w:pPr>' if pp else ''}{body}</w:p>"


def field(instr, result="", props=""):
    """A complex field with its cached result."""
    return (
        f'<w:r>{props}<w:fldChar w:fldCharType="begin"/></w:r>'
        f'<w:r>{props}<w:instrText xml:space="preserve"> {x(instr)} </w:instrText></w:r>'
        f'<w:r>{props}<w:fldChar w:fldCharType="separate"/></w:r>'
        + (R(result, props) if result else "")
        + f'<w:r>{props}<w:fldChar w:fldCharType="end"/></w:r>'
    )


def simple(instr, result, props=""):
    return f"<w:fldSimple w:instr={quoteattr(' ' + instr + ' ')}>{R(result, props)}</w:fldSimple>"


def bookmark(bid, name, inner):
    return f'<w:bookmarkStart w:id="{bid}" w:name="{x(name)}"/>{inner}<w:bookmarkEnd w:id="{bid}"/>'


def page_break():
    return '<w:r><w:br w:type="page"/></w:r>'


def column_break():
    return '<w:r><w:br w:type="column"/></w:r>'


def tracked(kind, rid, text, props="", author="Reviewer One"):
    if kind == "ins":
        return f'<w:ins w:id="{rid}" w:author="{author}" w:date="{STAMP}">{R(text, props)}</w:ins>'
    return (
        f'<w:del w:id="{rid}" w:author="{author}" w:date="{STAMP}">'
        f'<w:r>{props}<w:delText xml:space="preserve">{x(text)}</w:delText></w:r></w:del>'
    )


def borders(tag, color="auto", sz=4, val="single", sides=("top", "left", "bottom", "right")):
    inner = "".join(f'<w:{s} w:val="{val}" w:sz="{sz}" w:space="0" w:color="{color}"/>' for s in sides)
    return f"<w:{tag}>{inner}</w:{tag}>"


def cell(content, width, tcpr=""):
    paras = content if isinstance(content, list) else [content]
    body = "".join(p if p.startswith("<w:p") or p.startswith("<w:tbl") else P(p) for p in paras) or "<w:p/>"
    return f'<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/>{tcpr}</w:tcPr>{body}</w:tc>'


def table(rows, widths, *, tblpr="", border_color="auto", grid=True, trpr=None, style=None):
    """`rows`: lists of cells, each a string, a list of paragraphs, or (content, tcPr) or a raw '<w:tc' string."""
    pr = f'<w:tblStyle w:val="{style}"/>' if style else ""
    pr += tblpr + f'<w:tblW w:w="{sum(widths)}" w:type="dxa"/>'
    if grid:
        pr += borders("tblBorders", border_color, sides=("top", "left", "bottom", "right", "insideH", "insideV"))
    pr += '<w:tblLayout w:type="fixed"/><w:tblLook w:val="04A0"/>'
    out = f"<w:tbl><w:tblPr>{pr}</w:tblPr><w:tblGrid>"
    out += "".join(f'<w:gridCol w:w="{w}"/>' for w in widths) + "</w:tblGrid>"
    for k, row in enumerate(rows):
        tr = (trpr[k] if isinstance(trpr, list) else trpr) or ""
        out += f"<w:tr>{f'<w:trPr>{tr}</w:trPr>' if tr else ''}"
        for j, c in enumerate(row):
            if isinstance(c, str) and c.startswith("<w:tc"):
                out += c
            elif isinstance(c, tuple):
                out += cell(c[0], widths[j], c[1])
            else:
                out += cell(c, widths[j])
        out += "</w:tr>"
    return out + "</w:tbl>"


def shd(fill, val="clear", color="auto"):
    return f'<w:shd w:val="{val}" w:color="{color}" w:fill="{fill}"/>'


def emu(points):
    return int(points * 12700)


class Doc:
    """One document under construction: the body, its parts and their relationships."""

    def __init__(self, name, title):
        self.name = name
        self.title = title
        self.r = rng(name)
        self.body = []
        self.parts = {}
        self.rels = []
        self.overrides = {}
        self.defaults = {"rels": "application/vnd.openxmlformats-package.relationships+xml", "xml": "application/xml"}
        self.styles = []
        self.abstract = []
        self.nums = []
        self.notes = {"footnote": [], "endnote": []}
        self.note_separators = {}
        self.comments = []
        self.settings = ""
        self.background = ""
        self.ids = 0
        self.media_count = 0
        self.strict = False
        self.root_attrs = ""

    # Parts and relationships.
    def next_id(self):
        self.ids += 1
        return self.ids

    def rel(self, kind, target, external=False):
        rid = f"rId{len(self.rels) + 10}"
        self.rels.append((rid, kind if kind.startswith("http") else REL + kind, target, external))
        return rid

    def part(self, path, data, content_type=None):
        self.parts[path] = data
        if content_type:
            self.overrides["/" + path] = content_type

    def media(self, data, ext, name=None, rel_owner=None):
        self.media_count += 1
        name = name or f"image{self.media_count}.{ext}"
        self.part(f"word/media/{name}", data)
        self.defaults.setdefault(
            ext,
            {"png": "image/png", "jpeg": "image/jpeg", "gif": "image/gif", "wmf": "image/x-wmf", "emf": "image/x-emf"}[
                ext
            ],
        )
        if rel_owner is not None:
            return rel_owner.append(("image", f"../media/{name}"))
        return self.rel("image", f"media/{name}")

    def header(self, *paras, kind="header"):
        n = sum(1 for p in self.parts if p.startswith(f"word/{kind}")) + 1
        path = f"word/{kind}{n}.xml"
        tag = "hdr" if kind == "header" else "ftr"
        self.part(path, f"{DECL}<w:{tag} {ROOT_NS}>{''.join(paras)}</w:{tag}>", CT + f"{kind}+xml")
        return self.rel(kind, f"{kind}{n}.xml")

    def footer(self, *paras):
        return self.header(*paras, kind="footer")

    def note(self, kind, *paras):
        nid = len(self.notes[kind]) + 1
        ref_style = "FootnoteReference" if kind == "footnote" else "EndnoteReference"
        text_style = "FootnoteText" if kind == "footnote" else "EndnoteText"
        first = f'<w:r>{rpr(style=ref_style)}<w:{kind}Ref/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r>'
        body = []
        for k, p in enumerate(paras):
            runs = (first if k == 0 else "") + R(p)
            body.append(f'<w:p><w:pPr><w:pStyle w:val="{text_style}"/></w:pPr>{runs}</w:p>')
        self.notes[kind].append(f'<w:{kind} w:id="{nid}">{"".join(body)}</w:{kind}>')
        return f'<w:r>{rpr(style=ref_style)}<w:{kind}Reference w:id="{nid}"/></w:r>'

    def comment(self, text, author="Reviewer One"):
        cid = len(self.comments)
        para_id = f"{0x10000000 + cid:08X}"
        self.comments.append((cid, author, text, para_id))
        return cid

    def abstract_num(self, levels):
        """`levels`: (fmt, text, indent) per level, from level 0; fmt "bullet" takes `text` as the bullet."""
        aid = len(self.abstract)
        lv = ""
        for k, (fmt, text, ind, *rest) in enumerate(levels):
            extra = rest[0] if rest else ""
            lv += (
                f'<w:lvl w:ilvl="{k}"><w:start w:val="1"/><w:numFmt w:val="{fmt}"/><w:lvlText w:val="{x(text)}"/>'
                f'<w:lvlJc w:val="left"/><w:pPr><w:ind w:left="{ind}" w:hanging="360"/>{extra}</w:pPr>'
                + (
                    '<w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol" w:hint="default"/></w:rPr>'
                    if fmt == "bullet" and text == ""
                    else ""
                )
                + "</w:lvl>"
            )
        self.abstract.append(
            f'<w:abstractNum w:abstractNumId="{aid}"><w:multiLevelType w:val="multilevel"/>{lv}</w:abstractNum>'
        )
        self.nums.append(aid)
        return len(self.nums)

    def add(self, *xml):
        self.body.extend(xml)

    def p(self, *runs, **kw):
        self.add(P(*runs, **kw))

    # Pictures and drawings.
    def inline_picture(self, rid, w_pt, h_pt, name="Picture"):
        did = self.next_id()
        cx, cy = emu(w_pt), emu(h_pt)
        return (
            f'<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/>'
            f'<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="{did}" name="{name} {did}"/>'
            '<wp:cNvGraphicFramePr><a:graphicFrameLocks noChangeAspect="1"/></wp:cNvGraphicFramePr>'
            f'<a:graphic><a:graphicData uri="{NS_PIC}"><pic:pic><pic:nvPicPr><pic:cNvPr id="0" name="{name}"/>'
            f'<pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/>'
            f'</a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/>'
            '</a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData>'
            "</a:graphic></wp:inline></w:drawing></w:r>"
        )

    def anchor(
        self,
        graphic,
        w_pt,
        h_pt,
        x_pt,
        y_pt,
        *,
        h_from="column",
        v_from="paragraph",
        wrap="square",
        behind=False,
        rel_width=None,
    ):
        did = self.next_id()
        cx, cy = emu(w_pt), emu(h_pt)
        wraps = {
            "square": '<wp:wrapSquare wrapText="bothSides"/>',
            "none": "<wp:wrapNone/>",
            "topbottom": "<wp:wrapTopAndBottom/>",
            "through": '<wp:wrapThrough wrapText="bothSides"><wp:wrapPolygon edited="0"><wp:start x="0" y="0"/>'
            '<wp:lineTo x="0" y="21600"/><wp:lineTo x="21600" y="21600"/><wp:lineTo x="21600" y="0"/>'
            '<wp:lineTo x="0" y="0"/></wp:wrapPolygon></wp:wrapThrough>',
        }
        size_rel = (
            f'<wp14:sizeRelH relativeFrom="margin"><wp14:pctWidth>{rel_width * 1000}</wp14:pctWidth></wp14:sizeRelH>'
            '<wp14:sizeRelV relativeFrom="margin"><wp14:pctHeight>0</wp14:pctHeight></wp14:sizeRelV>'
            if rel_width
            else ""
        )
        return (
            f'<wp:anchor distT="0" distB="0" distL="114300" distR="114300" simplePos="0" '
            f'relativeHeight="{251658240 + did}" behindDoc="{int(behind)}" locked="0" layoutInCell="1" '
            f'allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="{h_from}">'
            f'<wp:posOffset>{emu(x_pt)}</wp:posOffset></wp:positionH><wp:positionV relativeFrom="{v_from}">'
            f'<wp:posOffset>{emu(y_pt)}</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/>'
            f'<wp:effectExtent l="0" t="0" r="0" b="0"/>{wraps[wrap]}<wp:docPr id="{did}" name="Shape {did}"/>'
            f"<wp:cNvGraphicFramePr/>{graphic(cx, cy)}{size_rel}</wp:anchor>"
        )

    def anchored_picture(self, rid, w_pt, h_pt, x_pt, y_pt, **kw):
        def graphic(cx, cy):
            return (
                f'<a:graphic><a:graphicData uri="{NS_PIC}"><pic:pic><pic:nvPicPr><pic:cNvPr id="0" name="p"/>'
                f'<pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/>'
                f'</a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/>'
                '</a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData>'
                "</a:graphic>"
            )

        return f"<w:r><w:drawing>{self.anchor(graphic, w_pt, h_pt, x_pt, y_pt, **kw)}</w:drawing></w:r>"

    def text_box(self, paras, w_pt, h_pt, x_pt, y_pt, *, fill="FFFFFF", line="1F4E79", geom="rect", **kw):
        """A DrawingML text box with its VML fallback, as Word writes one."""
        content = "".join(paras)

        def graphic(cx, cy):
            return (
                '<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">'
                f'<wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" '
                f'cy="{cy}"/></a:xfrm><a:prstGeom prst="{geom}"><a:avLst/></a:prstGeom><a:solidFill>'
                f'<a:srgbClr val="{fill}"/></a:solidFill><a:ln w="12700"><a:solidFill><a:srgbClr val="{line}"/>'
                f"</a:solidFill></a:ln></wps:spPr><wps:txbx><w:txbxContent>{content}</w:txbxContent></wps:txbx>"
                '<wps:bodyPr rot="0" vert="horz" wrap="square" lIns="91440" tIns="45720" rIns="91440" bIns="45720" '
                'anchor="t" anchorCtr="0"><a:noAutofit/></wps:bodyPr></wps:wsp></a:graphicData></a:graphic>'
            )

        drawing = self.anchor(graphic, w_pt, h_pt, x_pt, y_pt, **kw)
        vml = (
            f'<w:pict><v:shape id="tb{self.ids}" type="#_x0000_t202" style="position:absolute;margin-left:{x_pt}pt;'
            f'margin-top:{y_pt}pt;width:{w_pt}pt;height:{h_pt}pt;z-index:{self.ids}" fillcolor="#{fill}" '
            f'strokecolor="#{line}"><v:textbox><w:txbxContent>{content}</w:txbxContent></v:textbox>'
            '<w10:wrap type="square"/></v:shape></w:pict>'
        )
        return (
            '<w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>'
            f"{drawing}</w:drawing></mc:Choice><mc:Fallback>{vml}</mc:Fallback></mc:AlternateContent></w:r>"
        )

    def vml_text_box(self, paras, w_pt, h_pt, x_pt, y_pt, fill="EAF1DD", extra=""):
        return (
            f'<w:r><w:pict><v:shape id="vt{self.next_id()}" type="#_x0000_t202" style="position:absolute;'
            f"margin-left:{x_pt}pt;margin-top:{y_pt}pt;width:{w_pt}pt;height:{h_pt}pt;z-index:{self.ids};"
            f'mso-position-horizontal-relative:margin;mso-position-vertical-relative:paragraph" fillcolor="#{fill}" '
            f'strokecolor="#4F6228">{extra}<v:textbox><w:txbxContent>{"".join(paras)}</w:txbxContent></v:textbox>'
            '<w10:wrap type="square"/></v:shape></w:pict></w:r>'
        )

    def vml_picture(self, rid, w_pt, h_pt):
        return (
            f'<w:r><w:pict><v:shape id="vp{self.next_id()}" type="#_x0000_t75" style="width:{w_pt}pt;'
            f'height:{h_pt}pt"><v:imagedata r:id="{rid}" o:title=""/></v:shape></w:pict></w:r>'
        )

    # Assembly.
    def sect(
        self,
        *,
        page=LETTER,
        margins=(1440, 1440, 1440, 1440),
        landscape=False,
        cols=1,
        col_space=720,
        kind=None,
        headers=(),
        footers=(),
        title_pg=False,
        pg_num=None,
        text_dir=None,
        grid=None,
        bidi=False,
        header_dist=720,
        sep=False,
        extra="",
    ):
        w, h = (page[1], page[0]) if landscape else page
        out = "".join(f'<w:headerReference w:type="{t}" r:id="{rid}"/>' for t, rid in headers)
        out += "".join(f'<w:footerReference w:type="{t}" r:id="{rid}"/>' for t, rid in footers)
        if kind:
            out += f'<w:type w:val="{kind}"/>'
        orient = ' w:orient="landscape"' if landscape else ""
        out += f'<w:pgSz w:w="{w}" w:h="{h}"{orient}/>'
        top, right, bottom, left = margins
        out += (
            f'<w:pgMar w:top="{top}" w:right="{right}" w:bottom="{bottom}" w:left="{left}" '
            f'w:header="{header_dist}" w:footer="{header_dist}" w:gutter="0"/>'
        )
        out += extra
        if pg_num:
            out += f"<w:pgNumType {pg_num}/>"
        separator = ' w:sep="1"' if sep else ""
        if cols > 1:
            out += f'<w:cols w:num="{cols}" w:space="{col_space}"{separator}/>'
        else:
            out += f'<w:cols w:space="{col_space}"/>'
        if title_pg:
            out += "<w:titlePg/>"
        if text_dir:
            out += f'<w:textDirection w:val="{text_dir}"/>'
        if bidi:
            out += "<w:bidi/>"
        out += grid or '<w:docGrid w:linePitch="360"/>'
        return f"<w:sectPr>{out}</w:sectPr>"

    def save(self, out_dir, final_sect):
        files = {}
        doc_xml = (
            f"{DECL}<w:document {ROOT_NS}{self.root_attrs}>{self.background}<w:body>{''.join(self.body)}"
            f"{final_sect}</w:body></w:document>"
        )
        files["word/document.xml"] = doc_xml
        self.overrides["/word/document.xml"] = CT + "document.main+xml"
        files["word/styles.xml"] = styles_xml(self.styles)
        self.overrides["/word/styles.xml"] = CT + "styles+xml"
        self.rel("styles", "styles.xml")
        notes = ""
        for kind in ("footnote", "endnote"):
            if self.notes[kind]:
                sep, cont = self.note_separators.get(kind, ("<w:separator/>", "<w:continuationSeparator/>"))
                body = (
                    f'<w:{kind} w:type="separator" w:id="-1"><w:p><w:pPr><w:spacing w:after="0"/></w:pPr>'
                    f'<w:r>{sep}</w:r></w:p></w:{kind}><w:{kind} w:type="continuationSeparator" w:id="0">'
                    f'<w:p><w:pPr><w:spacing w:after="0"/></w:pPr><w:r>{cont}</w:r></w:p></w:{kind}>'
                )
                files[f"word/{kind}s.xml"] = f"{DECL}<w:{kind}s {ROOT_NS}>{body}{''.join(self.notes[kind])}</w:{kind}s>"
                self.overrides[f"/word/{kind}s.xml"] = CT + f"{kind}s+xml"
                self.rel(f"{kind}s", f"{kind}s.xml")
                notes += f'<w:{kind}Pr><w:{kind} w:id="-1"/><w:{kind} w:id="0"/></w:{kind}Pr>'
        if self.abstract:
            nums = "".join(
                f'<w:num w:numId="{k + 1}"><w:abstractNumId w:val="{a}"/></w:num>' for k, a in enumerate(self.nums)
            )
            files["word/numbering.xml"] = f"{DECL}<w:numbering {ROOT_NS}>{''.join(self.abstract)}{nums}</w:numbering>"
            self.overrides["/word/numbering.xml"] = CT + "numbering+xml"
            self.rel("numbering", "numbering.xml")
        if self.comments:
            cs = "".join(
                f'<w:comment w:id="{cid}" w:author="{a}" w:date="{STAMP}" w:initials="RO"><w:p w14:paraId="{pid}" '
                f'w14:textId="77777777"><w:pPr><w:pStyle w:val="CommentText"/></w:pPr><w:r>'
                f"{rpr(style='CommentReference')}<w:annotationRef/></w:r>{R(t)}</w:p></w:comment>"
                for cid, a, t, pid in self.comments
            )
            files["word/comments.xml"] = f"{DECL}<w:comments {ROOT_NS}>{cs}</w:comments>"
            self.overrides["/word/comments.xml"] = CT + "comments+xml"
            self.rel("comments", "comments.xml")
            ex = "".join(
                f'<w15:commentEx w15:paraId="{pid}" w15:done="{int(k % 2)}"/>'
                for k, (_, _, _, pid) in enumerate(self.comments)
            )
            files["word/commentsExtended.xml"] = f"{DECL}<w15:commentsEx {ROOT_NS}>{ex}</w15:commentsEx>"
            self.overrides["/word/commentsExtended.xml"] = CT + "commentsExtended+xml"
            self.rel("http://schemas.microsoft.com/office/2011/relationships/commentsExtended", "commentsExtended.xml")
            people = "".join(
                f'<w15:person w15:author="{a}"><w15:presenceInfo w15:providerId="None" w15:userId="{a}"/></w15:person>'
                for a in sorted({c[1] for c in self.comments})
            )
            files["word/people.xml"] = f"{DECL}<w15:people {ROOT_NS}>{people}</w15:people>"
            self.overrides["/word/people.xml"] = CT + "people+xml"
            self.rel("http://schemas.microsoft.com/office/2011/relationships/people", "people.xml")
        files["word/settings.xml"] = (
            f'{DECL}<w:settings {ROOT_NS}><w:zoom w:percent="100"/>{self.settings}'
            f'<w:defaultTabStop w:val="720"/><w:characterSpacingControl w:val="doNotCompress"/>{notes}'
            '<w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" '
            'w:val="15"/></w:compat><w:themeFontLang w:val="en-US" w:eastAsia="zh-CN" w:bidi="ar-SA"/>'
            "</w:settings>"
        )
        self.overrides["/word/settings.xml"] = CT + "settings+xml"
        self.rel("settings", "settings.xml")
        files["word/theme/theme1.xml"] = THEME
        self.overrides["/word/theme/theme1.xml"] = "application/vnd.openxmlformats-officedocument.theme+xml"
        self.rel("theme", "theme/theme1.xml")
        for path, data in self.parts.items():
            files[path] = data
            # A header or footer resolves its pictures and links through relationships of its own.
            if re.fullmatch(r"word/(header|footer)\d+\.xml", path):
                used = set(re.findall(r'r:(?:id|embed)="(rId\d+)"', data))
                own = "".join(
                    f'<Relationship Id="{rid}" Type="{kind}" Target="{x(target)}"'
                    + (' TargetMode="External"' if ext else "")
                    + "/>"
                    for rid, kind, target, ext in self.rels
                    if rid in used
                )
                if own:
                    files[path.replace("word/", "word/_rels/") + ".rels"] = (
                        f'{DECL}<Relationships xmlns="{PKG_REL}">{own}</Relationships>'
                    )
        rels = "".join(
            f'<Relationship Id="{rid}" Type="{kind}" Target="{x(target)}"'
            + (' TargetMode="External"' if ext else "")
            + "/>"
            for rid, kind, target, ext in self.rels
        )
        files["word/_rels/document.xml.rels"] = f'{DECL}<Relationships xmlns="{PKG_REL}">{rels}</Relationships>'
        office = REL + "officeDocument"
        files["_rels/.rels"] = (
            f'{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{office}" '
            'Target="word/document.xml"/><Relationship Id="rId2" '
            'Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" '
            'Target="docProps/core.xml"/><Relationship Id="rId3" '
            f'Type="{REL}extended-properties" Target="docProps/app.xml"/></Relationships>'
        )
        files["docProps/core.xml"] = (
            f'{DECL}<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/'
            'core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" '
            'xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">'
            f"<dc:title>{x(self.title)}</dc:title><dc:creator>{AUTHOR}</dc:creator>"
            f'<dcterms:created xsi:type="dcterms:W3CDTF">{STAMP}</dcterms:created>'
            f'<dcterms:modified xsi:type="dcterms:W3CDTF">{STAMP}</dcterms:modified></cp:coreProperties>'
        )
        self.overrides["/docProps/core.xml"] = "application/vnd.openxmlformats-package.core-properties+xml"
        files["docProps/app.xml"] = (
            f'{DECL}<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties">'
            "<Application>docx-to-pdf corpus generator</Application></Properties>"
        )
        self.overrides["/docProps/app.xml"] = "application/vnd.openxmlformats-officedocument.extended-properties+xml"
        ct = "".join(f'<Default Extension="{e}" ContentType="{t}"/>' for e, t in sorted(self.defaults.items()))
        ct += "".join(f'<Override PartName="{p}" ContentType="{t}"/>' for p, t in sorted(self.overrides.items()))
        files["[Content_Types].xml"] = (
            f'{DECL}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">{ct}</Types>'
        )
        if self.strict:
            files = strictify(files)
        write_zip(os.path.join(out_dir, f"{self.name}.docx"), files)


def write_zip(path, files):
    order = ["[Content_Types].xml", "_rels/.rels", "word/document.xml"]
    names = order + sorted(n for n in files if n not in order)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for name in names:
            data = files[name]
            if isinstance(data, str):
                if name.endswith((".xml", ".rels")):
                    try:
                        minidom.parseString(data.encode())
                    except Exception as e:
                        raise SystemExit(f"{path}: {name} is not well-formed: {e}") from e
                data = data.encode()
            info = zipfile.ZipInfo(name, ZIP_DATE)
            info.compress_type = zipfile.ZIP_STORED if name.startswith("word/media/") else zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, data)


STRICT = [
    (
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
        "http://purl.oclc.org/ooxml/wordprocessingml/main",
    ),
    (
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/",
        "http://purl.oclc.org/ooxml/officeDocument/relationships/",
    ),
    (
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        "http://purl.oclc.org/ooxml/officeDocument/relationships",
    ),
    ("http://schemas.openxmlformats.org/drawingml/2006/main", "http://purl.oclc.org/ooxml/drawingml/main"),
    (
        "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
        "http://purl.oclc.org/ooxml/drawingml/wordprocessingDrawing",
    ),
    ("http://schemas.openxmlformats.org/drawingml/2006/picture", "http://purl.oclc.org/ooxml/drawingml/picture"),
    ("http://schemas.openxmlformats.org/drawingml/2006/chart", "http://purl.oclc.org/ooxml/drawingml/chart"),
    ("http://schemas.openxmlformats.org/drawingml/2006/diagram", "http://purl.oclc.org/ooxml/drawingml/diagram"),
    ("http://schemas.openxmlformats.org/officeDocument/2006/math", "http://purl.oclc.org/ooxml/officeDocument/math"),
]


def strictify(files):
    """ISO/IEC 29500 Strict: the same markup under the Strict namespaces and relationship types."""
    out = {}
    for name, data in files.items():
        if isinstance(data, str) and name != "[Content_Types].xml":
            for old, new in STRICT:
                data = data.replace(old, new)
            data = data.replace("<w:document ", '<w:document w:conformance="strict" ', 1)
        out[name] = data
    return out


# ---------------------------------------------------------------------------------------------------------------
# Styles and theme.

STYLE_BASE = [
    ("paragraph", "Normal", "Normal", "", '<w:spacing w:after="160" w:line="259" w:lineRule="auto"/>', ""),
    (
        "paragraph",
        "Heading1",
        "heading 1",
        "Normal",
        '<w:keepNext/><w:keepLines/><w:spacing w:before="240" w:after="120"/><w:outlineLvl w:val="0"/>',
        '<w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi" w:eastAsiaTheme="majorEastAsia" '
        'w:cstheme="majorBidi"/><w:b/><w:color w:val="1F4E79"/><w:sz w:val="32"/><w:szCs w:val="32"/>',
    ),
    (
        "paragraph",
        "Heading2",
        "heading 2",
        "Normal",
        '<w:keepNext/><w:spacing w:before="200" w:after="80"/><w:outlineLvl w:val="1"/>',
        '<w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi"/><w:b/><w:color w:val="2E74B5"/>'
        '<w:sz w:val="26"/>',
    ),
    (
        "paragraph",
        "Heading3",
        "heading 3",
        "Normal",
        '<w:keepNext/><w:spacing w:before="160" w:after="60"/><w:outlineLvl w:val="2"/>',
        '<w:b/><w:i/><w:color w:val="404040"/><w:sz w:val="24"/>',
    ),
    (
        "paragraph",
        "Title",
        "Title",
        "Normal",
        '<w:spacing w:after="240"/>',
        '<w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi"/><w:sz w:val="52"/><w:szCs w:val="52"/>',
    ),
    (
        "paragraph",
        "TOC1",
        "toc 1",
        "Normal",
        '<w:tabs><w:tab w:val="right" w:leader="dot" w:pos="9350"/></w:tabs><w:spacing w:after="80"/>',
        "<w:b/>",
    ),
    (
        "paragraph",
        "TOC2",
        "toc 2",
        "Normal",
        '<w:tabs><w:tab w:val="right" w:leader="dot" w:pos="9350"/></w:tabs>'
        '<w:spacing w:after="60"/><w:ind w:left="220"/>',
        "",
    ),
    (
        "paragraph",
        "TOC3",
        "toc 3",
        "Normal",
        '<w:tabs><w:tab w:val="right" w:leader="dot" w:pos="9350"/></w:tabs>'
        '<w:spacing w:after="60"/><w:ind w:left="440"/>',
        "<w:i/>",
    ),
    ("paragraph", "TOCHeading", "TOC Heading", "Heading1", '<w:outlineLvl w:val="9"/>', ""),
    (
        "paragraph",
        "Caption",
        "caption",
        "Normal",
        '<w:spacing w:after="200"/>',
        '<w:i/><w:color w:val="44546A"/><w:sz w:val="18"/>',
    ),
    (
        "paragraph",
        "FootnoteText",
        "footnote text",
        "Normal",
        '<w:spacing w:after="0" w:line="240" w:lineRule="auto"/>',
        '<w:sz w:val="20"/><w:szCs w:val="20"/>',
    ),
    (
        "paragraph",
        "EndnoteText",
        "endnote text",
        "Normal",
        '<w:spacing w:after="0" w:line="240" w:lineRule="auto"/>',
        '<w:sz w:val="20"/><w:szCs w:val="20"/>',
    ),
    (
        "paragraph",
        "Header",
        "header",
        "Normal",
        '<w:tabs><w:tab w:val="center" w:pos="4680"/>'
        '<w:tab w:val="right" w:pos="9360"/></w:tabs><w:spacing w:after="0" w:line="240" w:lineRule="auto"/>',
        "",
    ),
    (
        "paragraph",
        "Footer",
        "footer",
        "Normal",
        '<w:tabs><w:tab w:val="center" w:pos="4680"/>'
        '<w:tab w:val="right" w:pos="9360"/></w:tabs><w:spacing w:after="0" w:line="240" w:lineRule="auto"/>',
        "",
    ),
    ("paragraph", "ListParagraph", "List Paragraph", "Normal", '<w:ind w:left="720"/><w:contextualSpacing/>', ""),
    (
        "paragraph",
        "CommentText",
        "annotation text",
        "Normal",
        '<w:spacing w:line="240" w:lineRule="auto"/>',
        '<w:sz w:val="20"/>',
    ),
    ("paragraph", "Bibliography", "Bibliography", "Normal", '<w:ind w:left="720" w:hanging="720"/>', ""),
    ("character", "FootnoteReference", "footnote reference", "", "", '<w:vertAlign w:val="superscript"/>'),
    ("character", "EndnoteReference", "endnote reference", "", "", '<w:vertAlign w:val="superscript"/>'),
    ("character", "CommentReference", "annotation reference", "", "", '<w:sz w:val="16"/>'),
    ("character", "Hyperlink", "Hyperlink", "", "", '<w:color w:val="0563C1"/><w:u w:val="single"/>'),
    ("character", "PlaceholderText", "Placeholder Text", "", "", '<w:color w:val="808080"/>'),
]


def styles_xml(extra):
    out = []
    for kind, sid, name, based, ppr, rpr_ in STYLE_BASE + extra:
        default = ' w:default="1"' if sid == "Normal" else ""
        out.append(
            f'<w:style w:type="{kind}"{default} w:styleId="{sid}"><w:name w:val="{name}"/>'
            + (f'<w:basedOn w:val="{based}"/>' if based else "")
            + "<w:qFormat/>"
            + (f"<w:pPr>{ppr}</w:pPr>" if ppr else "")
            + (f"<w:rPr>{rpr_}</w:rPr>" if rpr_ else "")
            + "</w:style>"
        )
    out.append(
        '<w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/>'
        '<w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/>'
        '<w:left w:w="108" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="108" w:type="dxa"/>'
        "</w:tblCellMar></w:tblPr></w:style>"
    )
    defaults = (
        '<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia" '
        'w:hAnsiTheme="minorHAnsi" w:cstheme="minorBidi"/><w:sz w:val="22"/><w:szCs w:val="22"/>'
        '<w:lang w:val="en-US" w:eastAsia="zh-CN" w:bidi="ar-SA"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr>'
        '<w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>'
    )
    return f"{DECL}<w:styles {ROOT_NS}>{defaults}{''.join(out)}</w:styles>"


def _colors():
    names = [
        ("dk1", '<a:sysClr val="windowText" lastClr="000000"/>'),
        ("lt1", '<a:sysClr val="window" lastClr="FFFFFF"/>'),
        ("dk2", '<a:srgbClr val="44546A"/>'),
        ("lt2", '<a:srgbClr val="E7E6E6"/>'),
    ]
    accents = ["4472C4", "ED7D31", "A5A5A5", "FFC000", "5B9BD5", "70AD47"]
    names += [(f"accent{k + 1}", f'<a:srgbClr val="{c}"/>') for k, c in enumerate(accents)]
    names += [("hlink", '<a:srgbClr val="0563C1"/>'), ("folHlink", '<a:srgbClr val="954F72"/>')]
    return "".join(f"<a:{n}>{v}</a:{n}>" for n, v in names)


def _fills():
    solid = '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
    grad = (
        '<a:gradFill rotWithShape="1"><a:gsLst><a:gs pos="0"><a:schemeClr val="phClr"><a:tint val="67000"/>'
        '</a:schemeClr></a:gs><a:gs pos="100000"><a:schemeClr val="phClr"><a:shade val="94000"/></a:schemeClr>'
        '</a:gs></a:gsLst><a:lin ang="5400000" scaled="0"/></a:gradFill>'
    )
    return solid + grad + grad


THEME = (
    f'{DECL}<a:theme xmlns:a="{NS_A}" name="Corpus"><a:themeElements><a:clrScheme name="Corpus">{_colors()}'
    '</a:clrScheme><a:fontScheme name="Corpus"><a:majorFont><a:latin typeface="Cambria"/><a:ea typeface=""/>'
    '<a:cs typeface=""/><a:font script="Hans" typeface="SimSun"/><a:font script="Jpan" typeface="MS Mincho"/>'
    '<a:font script="Hebr" typeface="Times New Roman"/><a:font script="Arab" typeface="Times New Roman"/>'
    '</a:majorFont><a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/>'
    '<a:font script="Hans" typeface="SimSun"/><a:font script="Jpan" typeface="MS Mincho"/>'
    '<a:font script="Hebr" typeface="Arial"/><a:font script="Arab" typeface="Arial"/></a:minorFont></a:fontScheme>'
    f'<a:fmtScheme name="Corpus"><a:fillStyleLst>{_fills()}</a:fillStyleLst><a:lnStyleLst>'
    + '<a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>' * 3
    + "</a:lnStyleLst><a:effectStyleLst>"
    + "<a:effectStyle><a:effectLst/></a:effectStyle>" * 3
    + f"</a:effectStyleLst><a:bgFillStyleLst>{_fills()}</a:bgFillStyleLst></a:fmtScheme></a:themeElements>"
    "<a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"
)


# ---------------------------------------------------------------------------------------------------------------
# Pictures, drawn here.


def photo(name, w, h):
    """A photo-like picture: smooth gradients, soft shapes, a little seeded grain."""
    r = rng(name)
    base = Image.new("RGB", (w, h))
    c0 = [r.randrange(40, 200) for _ in range(3)]
    c1 = [r.randrange(40, 230) for _ in range(3)]
    px = base.load()
    for yy in range(h):
        for xx in range(w):
            t = (xx / max(w - 1, 1) + yy / max(h - 1, 1)) / 2
            px[xx, yy] = tuple(int(c0[k] + (c1[k] - c0[k]) * t) for k in range(3))
    draw = ImageDraw.Draw(base)
    for _ in range(6):
        x0, y0 = r.randrange(w), r.randrange(h)
        rad = r.randrange(max(3, min(w, h) // 8), max(4, min(w, h) // 2))
        col = tuple(r.randrange(256) for _ in range(3))
        draw.ellipse((x0 - rad, y0 - rad, x0 + rad, y0 + rad), fill=col)
    base = base.filter(ImageFilter.GaussianBlur(max(1, min(w, h) // 40)))
    px = base.load()
    for yy in range(0, h):
        for xx in range(0, w):
            n = r.randrange(-6, 7)
            p = px[xx, yy]
            px[xx, yy] = tuple(min(255, max(0, v + n)) for v in p)
    return base


def jpeg(name, w, h, quality, subsampling):
    out = io.BytesIO()
    photo(name, w, h).save(out, "JPEG", quality=quality, subsampling=subsampling, optimize=False)
    return out.getvalue()


def png(name, w, h, mode="RGB"):
    out = io.BytesIO()
    image = photo(name, w, h)
    if mode != "RGB":
        image = image.convert(mode)
    image.save(out, "PNG", compress_level=9)
    return out.getvalue()


def gif(name, w, h):
    out = io.BytesIO()
    image = Image.new("P", (w, h), 0)
    image.putpalette([255, 255, 255, 31, 78, 121, 237, 125, 49, 112, 173, 71] + [0] * (256 * 3 - 12))
    draw = ImageDraw.Draw(image)
    draw.rectangle((2, 2, w - 3, h - 3), outline=1, width=3)
    draw.ellipse((w // 4, h // 4, 3 * w // 4, 3 * h // 4), fill=2)
    draw.line((0, h - 1, w - 1, 0), fill=3, width=4)
    image.save(out, "GIF")
    return out.getvalue()


def wmf(w=400, h=240):
    """A placeable Windows Metafile: a filled rectangle, an ellipse and a line, written record by record."""

    def rec(func, *words):
        return struct.pack("<IH", 3 + len(words), func) + struct.pack(f"<{len(words)}h", *words)

    records = (
        rec(0x020B, 0, 0)  # SetWindowOrg
        + rec(0x020C, h, w)  # SetWindowExt
        + struct.pack("<IHHIh", 7, 0x02FC, 0, 0x00C47231, 0)  # CreateBrushIndirect: solid, colour
        + rec(0x012D, 0)  # SelectObject 0
        + rec(0x041B, h - 10, w - 10, 10, 10)  # Rectangle(bottom, right, top, left)
        + struct.pack("<IHHIh", 7, 0x02FC, 0, 0x0031ED7D, 0)
        + rec(0x012D, 1)
        + rec(0x0418, h - 40, w - 60, 40, 60)  # Ellipse
        + rec(0x0214, 20, 20)  # MoveTo(y, x)
        + rec(0x0213, h - 20, w - 20)  # LineTo
        + rec(0x0000)  # EOF
    )
    size_words = (18 + len(records)) // 2
    header = struct.pack("<HHHIHIH", 1, 9, 0x0300, size_words, 2, 7, 0)
    placeable = struct.pack("<IHhhhhHI", 0x9AC6CDD7, 0, 0, 0, w, h, 96, 0)
    checksum = 0
    for k in range(0, 20, 2):
        checksum ^= struct.unpack("<H", placeable[k : k + 2])[0]
    placeable += struct.pack("<H", checksum)
    return placeable + header + records


# ---------------------------------------------------------------------------------------------------------------
# Charts, SmartArt, workbooks.


def xlsx(rows):
    """A minimal workbook of `rows` (the first a header), inline strings and numbers."""

    def col(k):
        return chr(ord("A") + k)

    sheet = ""
    for i, row in enumerate(rows):
        cells = ""
        for k, v in enumerate(row):
            ref = f"{col(k)}{i + 1}"
            if isinstance(v, str):
                cells += f'<c r="{ref}" t="inlineStr"><is><t>{x(v)}</t></is></c>'
            else:
                cells += f'<c r="{ref}"><v>{v}</v></c>'
        sheet += f'<row r="{i + 1}">{cells}</row>'
    ns = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
    files = {
        "[Content_Types].xml": (
            f'{DECL}<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" '
            'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            '<Override PartName="/xl/worksheets/sheet1.xml" '
            'ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>'
        ),
        "_rels/.rels": (
            f'{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" '
            f'Type="{REL}officeDocument" Target="xl/workbook.xml"/></Relationships>'
        ),
        "xl/workbook.xml": (
            f'{DECL}<workbook xmlns="{ns}" xmlns:r="{NS_R}"><sheets><sheet name="Sheet1" '
            'sheetId="1" r:id="rId1"/></sheets></workbook>'
        ),
        "xl/_rels/workbook.xml.rels": (
            f'{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" '
            f'Type="{REL}worksheet" Target="worksheets/sheet1.xml"/></Relationships>'
        ),
        "xl/worksheets/sheet1.xml": f'{DECL}<worksheet xmlns="{ns}"><sheetData>{sheet}</sheetData></worksheet>',
    }
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        for name in [
            "[Content_Types].xml",
            "_rels/.rels",
            "xl/workbook.xml",
            "xl/_rels/workbook.xml.rels",
            "xl/worksheets/sheet1.xml",
        ]:
            info = zipfile.ZipInfo(name, ZIP_DATE)
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, files[name].encode())
    return out.getvalue()


def chart(doc, kind, categories, series, title, w_pt=360, h_pt=216):
    """A chart part with its embedded workbook; returns the run that places it inline."""
    n = sum(1 for p in doc.parts if p.startswith("word/charts/chart")) + 1
    rows = [[""] + [s[0] for s in series]] + [[c] + [s[1][k] for s in series] for k, c in enumerate(categories)]
    doc.part(f"word/embeddings/Workbook{n}.xlsx", xlsx(rows))
    doc.defaults["xlsx"] = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    sers = ""
    colors = ["4472C4", "ED7D31", "70AD47"]
    for k, (sname, values) in enumerate(series):
        col = chr(ord("B") + k)
        cats = "".join(f'<c:pt idx="{i}"><c:v>{x(c)}</c:v></c:pt>' for i, c in enumerate(categories))
        vals = "".join(f'<c:pt idx="{i}"><c:v>{v}</c:v></c:pt>' for i, v in enumerate(values))
        last = len(categories) + 1
        sers += (
            f'<c:ser><c:idx val="{k}"/><c:order val="{k}"/><c:tx><c:strRef><c:f>Sheet1!${col}$1</c:f><c:strCache>'
            f'<c:ptCount val="1"/><c:pt idx="0"><c:v>{x(sname)}</c:v></c:pt></c:strCache></c:strRef></c:tx>'
            f'<c:spPr><a:solidFill><a:srgbClr val="{colors[k % 3]}"/></a:solidFill></c:spPr>'
            + ('<c:marker><c:symbol val="circle"/><c:size val="6"/></c:marker>' if kind == "line" else "")
            + f'<c:cat><c:strRef><c:f>Sheet1!$A$2:$A${last}</c:f><c:strCache><c:ptCount val="{len(categories)}"/>'
            f"{cats}</c:strCache></c:strRef></c:cat><c:val><c:numRef><c:f>Sheet1!${col}$2:${col}${last}</c:f>"
            f'<c:numCache><c:formatCode>General</c:formatCode><c:ptCount val="{len(values)}"/>{vals}</c:numCache>'
            "</c:numRef></c:val>" + ('<c:smooth val="0"/>' if kind == "line" else "") + "</c:ser>"
        )
    if kind == "bar":
        plot = (
            f'<c:barChart><c:barDir val="col"/><c:grouping val="clustered"/><c:varyColors val="0"/>{sers}'
            '<c:gapWidth val="150"/><c:axId val="111"/><c:axId val="222"/></c:barChart>'
        )
    else:
        plot = (
            f'<c:lineChart><c:grouping val="standard"/><c:varyColors val="0"/>{sers}<c:marker val="1"/>'
            '<c:axId val="111"/><c:axId val="222"/></c:lineChart>'
        )
    axes = (
        '<c:catAx><c:axId val="111"/><c:scaling><c:orientation val="minMax"/></c:scaling><c:delete val="0"/>'
        '<c:axPos val="b"/><c:numFmt formatCode="General" sourceLinked="1"/><c:tickLblPos val="nextTo"/>'
        '<c:crossAx val="222"/><c:crosses val="autoZero"/><c:auto val="1"/><c:lblAlgn val="ctr"/>'
        '<c:lblOffset val="100"/></c:catAx><c:valAx><c:axId val="222"/><c:scaling><c:orientation val="minMax"/>'
        '</c:scaling><c:delete val="0"/><c:axPos val="l"/><c:majorGridlines/><c:numFmt formatCode="General" '
        'sourceLinked="1"/><c:tickLblPos val="nextTo"/><c:crossAx val="111"/><c:crosses val="autoZero"/>'
        '<c:crossBetween val="between"/></c:valAx>'
    )
    xml = (
        f'{DECL}<c:chartSpace xmlns:c="{NS_C}" xmlns:a="{NS_A}" xmlns:r="{NS_R}"><c:roundedCorners val="0"/>'
        f"<c:chart><c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>{x(title)}</a:t></a:r></a:p></c:rich></c:tx>"
        f'<c:overlay val="0"/></c:title><c:autoTitleDeleted val="0"/><c:plotArea><c:layout/>{plot}{axes}'
        '</c:plotArea><c:legend><c:legendPos val="r"/><c:overlay val="0"/></c:legend><c:plotVisOnly val="1"/>'
        '</c:chart><c:externalData r:id="rId1"><c:autoUpdate val="0"/></c:externalData></c:chartSpace>'
    )
    doc.part(f"word/charts/chart{n}.xml", xml, "application/vnd.openxmlformats-officedocument.drawingml.chart+xml")
    doc.part(
        f"word/charts/_rels/chart{n}.xml.rels",
        f'{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}package" '
        f'Target="../embeddings/Workbook{n}.xlsx"/></Relationships>',
    )
    rid = doc.rel("chart", f"charts/chart{n}.xml")
    did = doc.next_id()
    cx, cy = emu(w_pt), emu(h_pt)
    return (
        f'<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/>'
        f'<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="{did}" name="Chart {did}"/><wp:cNvGraphicFramePr/>'
        f'<a:graphic><a:graphicData uri="{NS_C}"><c:chart xmlns:c="{NS_C}" r:id="{rid}"/></a:graphicData>'
        "</a:graphic></wp:inline></w:drawing></w:r>"
    )


def guid(head, tail):
    """A SmartArt model id: `head` as the first digit of the first group, `tail` as the last group, zeros between."""
    zeros = "0" * 4
    return "{" + f"{head}{'0' * 7}-{zeros}-{zeros}-{zeros}-{tail:012d}" + "}"


def smartart(doc, labels, w_pt=400, h_pt=120, pictures=()):
    """A SmartArt graphic: its data model, layout, style and colours, and the drawing Word caches beside them,
    boxes in a row joined by arrows; `pictures` puts a picture into the first boxes."""
    n = sum(1 for p in doc.parts if p.startswith("word/diagrams/data")) + 1
    pts = (
        f'<dgm:pt modelId="{guid(0, 0)}" type="doc"><dgm:prSet '
        'loTypeId="urn:microsoft.com/office/officeart/2005/8/layout/process1"/><dgm:spPr/><dgm:t><a:bodyPr/><a:p>'
        '<a:endParaRPr lang="en-US"/></a:p></dgm:t></dgm:pt>'
    )
    cxns = ""
    for k, label in enumerate(labels):
        mid = guid(0, k + 10)
        pts += (
            f'<dgm:pt modelId="{mid}"><dgm:prSet phldrT="[Text]"/><dgm:spPr/><dgm:t><a:bodyPr/><a:p><a:r>'
            f'<a:rPr lang="en-US"/><a:t>{x(label)}</a:t></a:r></a:p></dgm:t></dgm:pt>'
        )
        cxns += f'<dgm:cxn modelId="{guid(0, k + 110)}" srcId="{guid(0, 0)}" destId="{mid}" srcOrd="{k}" destOrd="0"/>'
    drawing_rid_placeholder = "__DRAWING__"
    data = (
        f'{DECL}<dgm:dataModel xmlns:dgm="{NS_DGM}" xmlns:a="{NS_A}" xmlns:r="{NS_R}"><dgm:ptLst>{pts}</dgm:ptLst>'
        f"<dgm:cxnLst>{cxns}</dgm:cxnLst><dgm:bg/><dgm:whole/><dgm:extLst>"
        '<a:ext uri="http://schemas.microsoft.com/office/drawing/2008/diagram">'
        f'<dsp:dataModelExt xmlns:dsp="{NS_DSP}" relId="{drawing_rid_placeholder}" minVer="{NS_DGM}"/></a:ext>'
        "</dgm:extLst></dgm:dataModel>"
    )
    layout = (
        f'{DECL}<dgm:layoutDef xmlns:dgm="{NS_DGM}" xmlns:a="{NS_A}" '
        'uniqueId="urn:microsoft.com/office/officeart/2005/8/layout/process1"><dgm:title val=""/><dgm:desc val=""/>'
        '<dgm:catLst><dgm:cat type="process" pri="1000"/></dgm:catLst><dgm:layoutNode name="Name0">'
        '<dgm:alg type="lin"/><dgm:shape xmlns:r="' + NS_R + '" r:blip=""><dgm:adjLst/></dgm:shape>'
        '<dgm:presOf/><dgm:forEach name="Name1" axis="ch" ptType="node"><dgm:layoutNode name="node">'
        '<dgm:alg type="tx"/><dgm:shape type="roundRect" xmlns:r="' + NS_R + '" r:blip=""><dgm:adjLst/></dgm:shape>'
        '<dgm:presOf axis="desOrSelf" ptType="node"/></dgm:layoutNode></dgm:forEach></dgm:layoutNode>'
        "</dgm:layoutDef>"
    )
    style = (
        f'{DECL}<dgm:styleDef xmlns:dgm="{NS_DGM}" xmlns:a="{NS_A}" '
        'uniqueId="urn:microsoft.com/office/officeart/2005/8/quickstyle/simple1"><dgm:title val=""/>'
        '<dgm:desc val=""/><dgm:catLst><dgm:cat type="simple" pri="10100"/></dgm:catLst><dgm:scene3d>'
        '<a:camera prst="orthographicFront"/><a:lightRig rig="threePt" dir="t"/></dgm:scene3d>'
        '<dgm:styleLbl name="node1"><dgm:scene3d><a:camera prst="orthographicFront"/>'
        '<a:lightRig rig="threePt" dir="t"/></dgm:scene3d><dgm:sp3d/><dgm:txPr/>'
        '<dgm:style><a:lnRef idx="2"><a:scrgbClr r="0" g="0" b="0"/></a:lnRef><a:fillRef idx="1">'
        '<a:scrgbClr r="0" g="0" b="0"/></a:fillRef><a:effectRef idx="0"><a:scrgbClr r="0" g="0" b="0"/>'
        '</a:effectRef><a:fontRef idx="minor"><a:schemeClr val="lt1"/></a:fontRef></dgm:style>'
        "</dgm:styleLbl></dgm:styleDef>"
    )
    colors = (
        f'{DECL}<dgm:colorsDef xmlns:dgm="{NS_DGM}" xmlns:a="{NS_A}" '
        'uniqueId="urn:microsoft.com/office/officeart/2005/8/colors/accent1_2"><dgm:title val=""/>'
        '<dgm:desc val=""/><dgm:catLst><dgm:cat type="accent1" pri="11200"/></dgm:catLst>'
        '<dgm:styleLbl name="node1"><dgm:fillClrLst meth="repeat"><a:schemeClr val="accent1"/>'
        '</dgm:fillClrLst><dgm:linClrLst meth="repeat"><a:schemeClr val="lt1"/></dgm:linClrLst>'
        '<dgm:effectClrLst/><dgm:txLinClrLst/><dgm:txFillClrLst meth="repeat"><a:schemeClr val="lt1"/>'
        "</dgm:txFillClrLst><dgm:txEffectClrLst/></dgm:styleLbl></dgm:colorsDef>"
    )
    # The cached drawing: boxes and arrows in EMU.
    cx, cy = emu(w_pt), emu(h_pt)
    k_n = len(labels)
    gap = cx // (k_n * 5)
    box_w = (cx - gap * (k_n - 1)) // k_n
    box_h = cy * 2 // 3
    top = (cy - box_h) // 2
    shapes = ""
    drawing_rels = []
    for k, label in enumerate(labels):
        left = k * (box_w + gap)
        fill = '<a:solidFill><a:schemeClr val="accent1"><a:hueOff val="0"/></a:schemeClr></a:solidFill>'
        if k < len(pictures):
            rid = f"rId{len(drawing_rels) + 1}"
            drawing_rels.append((rid, pictures[k]))
            fill = f'<a:blipFill><a:blip r:embed="{rid}"/><a:srcRect/><a:stretch><a:fillRect/></a:stretch></a:blipFill>'
        shapes += (
            f'<dsp:sp modelId="{guid(1, k + 10)}"><dsp:nvSpPr><dsp:cNvPr id="0" '
            f'name=""/><dsp:cNvSpPr/></dsp:nvSpPr><dsp:spPr><a:xfrm><a:off x="{left}" y="{top}"/>'
            f'<a:ext cx="{box_w}" cy="{box_h}"/></a:xfrm><a:prstGeom prst="roundRect"><a:avLst/></a:prstGeom>'
            f'{fill}<a:ln w="12700"><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a:ln></dsp:spPr>'
            '<dsp:style><a:lnRef idx="2"><a:scrgbClr r="0" g="0" b="0"/></a:lnRef><a:fillRef idx="1">'
            '<a:scrgbClr r="0" g="0" b="0"/></a:fillRef><a:effectRef idx="0"><a:scrgbClr r="0" g="0" b="0"/>'
            '</a:effectRef><a:fontRef idx="minor"><a:schemeClr val="lt1"/></a:fontRef></dsp:style>'
            '<dsp:txBody><a:bodyPr anchor="b"/><a:lstStyle/><a:p><a:pPr algn="ctr"/><a:r><a:rPr lang="en-US" '
            f'sz="1400" b="1"><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a:rPr><a:t>{x(label)}</a:t>'
            "</a:r></a:p></dsp:txBody>"
            f'<dsp:txXfrm><a:off x="{left}" y="{top}"/><a:ext cx="{box_w}" cy="{box_h}"/></dsp:txXfrm></dsp:sp>'
        )
        if k + 1 < k_n:
            shapes += (
                f'<dsp:sp modelId="{guid(2, k + 10)}"><dsp:nvSpPr><dsp:cNvPr '
                'id="0" name=""/><dsp:cNvSpPr/></dsp:nvSpPr><dsp:spPr><a:xfrm><a:off '
                f'x="{left + box_w + gap // 6}" y="{top + box_h // 2 - gap // 3}"/><a:ext cx="{gap * 2 // 3}" '
                f'cy="{gap * 2 // 3}"/></a:xfrm><a:prstGeom prst="rightArrow"><a:avLst/></a:prstGeom><a:solidFill>'
                '<a:srgbClr val="9DC3E6"/></a:solidFill></dsp:spPr></dsp:sp>'
            )
    drawing = (
        f'{DECL}<dsp:drawing xmlns:dgm="{NS_DGM}" xmlns:dsp="{NS_DSP}" xmlns:a="{NS_A}" xmlns:r="{NS_R}">'
        f'<dsp:spTree><dsp:nvGrpSpPr><dsp:cNvPr id="0" name=""/><dsp:cNvGrpSpPr/></dsp:nvGrpSpPr>'
        f"<dsp:grpSpPr/>{shapes}</dsp:spTree></dsp:drawing>"
    )
    ct = "application/vnd.openxmlformats-officedocument.drawingml."
    rid_draw = doc.rel(
        "http://schemas.microsoft.com/office/2007/relationships/diagramDrawing", f"diagrams/drawing{n}.xml"
    )
    doc.part(f"word/diagrams/data{n}.xml", data.replace(drawing_rid_placeholder, rid_draw), ct + "diagramData+xml")
    doc.part(f"word/diagrams/layout{n}.xml", layout, ct + "diagramLayout+xml")
    doc.part(f"word/diagrams/quickStyle{n}.xml", style, ct + "diagramStyle+xml")
    doc.part(f"word/diagrams/colors{n}.xml", colors, ct + "diagramColors+xml")
    doc.part(f"word/diagrams/drawing{n}.xml", drawing, "application/vnd.ms-office.drawingml.diagramDrawing+xml")
    if drawing_rels:
        rels = "".join(
            f'<Relationship Id="{rid}" Type="{REL}image" Target="../media/{target}"/>' for rid, target in drawing_rels
        )
        doc.part(
            f"word/diagrams/_rels/drawing{n}.xml.rels", f'{DECL}<Relationships xmlns="{PKG_REL}">{rels}</Relationships>'
        )
    dm = doc.rel("diagramData", f"diagrams/data{n}.xml")
    lo = doc.rel("diagramLayout", f"diagrams/layout{n}.xml")
    qs = doc.rel("diagramQuickStyle", f"diagrams/quickStyle{n}.xml")
    cs = doc.rel("diagramColors", f"diagrams/colors{n}.xml")
    did = doc.next_id()
    return (
        f'<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/>'
        f'<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="{did}" name="Diagram {did}"/>'
        f'<wp:cNvGraphicFramePr/><a:graphic><a:graphicData uri="{NS_DGM}"><dgm:relIds xmlns:dgm="{NS_DGM}" '
        f'r:dm="{dm}" r:lo="{lo}" r:qs="{qs}" r:cs="{cs}"/></a:graphicData></a:graphic></wp:inline></w:drawing>'
        "</w:r>"
    )


# ---------------------------------------------------------------------------------------------------------------
# Office Math.


def m_r(text, sty=None):
    s = f'<m:rPr><m:sty m:val="{sty}"/></m:rPr>' if sty else ""
    return f"<m:r>{s}<m:t>{x(text)}</m:t></m:r>"


def m_frac(num, den):
    return f"<m:f><m:num>{num}</m:num><m:den>{den}</m:den></m:f>"


def m_sup(base, sup):
    return f"<m:sSup><m:e>{base}</m:e><m:sup>{sup}</m:sup></m:sSup>"


def m_sub(base, sub):
    return f"<m:sSub><m:e>{base}</m:e><m:sub>{sub}</m:sub></m:sSub>"


def m_nary(chr_, lower, upper, body):
    return (
        f'<m:nary><m:naryPr><m:chr m:val="{chr_}"/><m:limLoc m:val="undOvr"/></m:naryPr><m:sub>{lower}</m:sub>'
        f"<m:sup>{upper}</m:sup><m:e>{body}</m:e></m:nary>"
    )


def m_paren(inner):
    return f'<m:d><m:dPr><m:begChr m:val="("/><m:endChr m:val=")"/></m:dPr><m:e>{inner}</m:e></m:d>'


def m_rad(inner):
    return f'<m:rad><m:radPr><m:degHide m:val="1"/></m:radPr><m:deg/><m:e>{inner}</m:e></m:rad>'


def m_binom(top, bottom):
    return (
        f'<m:d><m:e><m:f><m:fPr><m:type m:val="noBar"/></m:fPr><m:num>{top}</m:num><m:den>{bottom}</m:den>'
        "</m:f></m:e></m:d>"
    )


def omath(inner):
    return f"<m:oMath>{inner}</m:oMath>"


def omath_para(inner, jc="center"):
    return f'<w:p><m:oMathPara><m:oMathParaPr><m:jc m:val="{jc}"/></m:oMathParaPr>{omath(inner)}</m:oMathPara></w:p>'


BINOMIAL = (
    m_sup(m_paren(m_r("x") + m_r("+") + m_r("a")), m_r("n"))
    + m_r("=")
    + m_nary(
        "∑", m_r("k=0"), m_r("n"), m_binom(m_r("n"), m_r("k")) + m_sup(m_r("x"), m_r("k")) + m_sup(m_r("a"), m_r("n−k"))
    )
)
TAYLOR = (
    m_sup(m_paren(m_r("1") + m_r("+") + m_r("x")), m_r("n"))
    + m_r("=1+")
    + m_frac(m_r("nx"), m_r("1!"))
    + m_r("+")
    + m_frac(m_r("n") + m_paren(m_r("n−1")) + m_sup(m_r("x"), m_r("2")), m_r("2!"))
    + m_r("+…")
)
QUADRATIC = m_r("x=") + m_frac(m_r("−b±") + m_rad(m_sup(m_r("b"), m_r("2")) + m_r("−4ac")), m_r("2a"))
VARIANCE = (
    m_sup(m_r("σ"), m_r("2"))
    + m_r("=")
    + m_frac(m_r("1"), m_r("N"))
    + m_nary("∑", m_r("i=1"), m_r("N"), m_sup(m_paren(m_sub(m_r("x"), m_r("i")) + m_r("−μ")), m_r("2")))
)
AREA = m_r("A=π") + m_sup(m_r("r"), m_r("2"))
INTEGRAL = (
    m_nary("∫", m_r("0"), m_r("∞"), m_sup(m_r("e"), m_r("−") + m_sup(m_r("x"), m_r("2"))) + m_r("dx"))
    + m_r("=")
    + m_frac(m_rad(m_r("π")), m_r("2"))
)


# ---------------------------------------------------------------------------------------------------------------
# Content controls, form fields, tables of contents.

SDT_IDS = iter(range(100000, 200000))


def sdt_text(alias, text=None, placeholder="Click here to enter text.", props=""):
    shown = text is None
    content = R(placeholder, rpr(style="PlaceholderText")) if shown else R(text, props)
    return (
        f'<w:sdt><w:sdtPr><w:alias w:val="{x(alias)}"/><w:tag w:val="{x(alias)}"/><w:id w:val="{next(SDT_IDS)}"/>'
        f"{'<w:showingPlcHdr/>' if shown else ''}<w:text/></w:sdtPr><w:sdtContent>{content}</w:sdtContent></w:sdt>"
    )


def sdt_date(alias):
    return (
        f'<w:sdt><w:sdtPr><w:alias w:val="{x(alias)}"/><w:id w:val="{next(SDT_IDS)}"/><w:showingPlcHdr/>'
        '<w:date><w:dateFormat w:val="M/d/yyyy"/><w:lid w:val="en-US"/><w:storeMappedDataAs w:val="dateTime"/>'
        '<w:calendar w:val="gregorian"/></w:date></w:sdtPr><w:sdtContent>'
        f"{R('Click here to enter a date.', rpr(style='PlaceholderText'))}</w:sdtContent></w:sdt>"
    )


def sdt_dropdown(alias, items, chosen):
    li = "".join(f'<w:listItem w:displayText="{x(i)}" w:value="{x(i)}"/>' for i in items)
    return (
        f'<w:sdt><w:sdtPr><w:alias w:val="{x(alias)}"/><w:id w:val="{next(SDT_IDS)}"/><w:dropDownList>{li}'
        f"</w:dropDownList></w:sdtPr><w:sdtContent>{R(chosen)}</w:sdtContent></w:sdt>"
    )


def sdt_checkbox(checked):
    mark = "☒" if checked else "☐"
    return (
        f'<w:sdt><w:sdtPr><w:id w:val="{next(SDT_IDS)}"/><w14:checkbox><w14:checked w14:val="{int(checked)}"/>'
        '<w14:checkedState w14:val="2612" w14:font="MS Gothic"/><w14:uncheckedState w14:val="2610" '
        f'w14:font="MS Gothic"/></w14:checkbox></w:sdtPr><w:sdtContent>{R(mark, rpr(font="MS Gothic"))}'
        "</w:sdtContent></w:sdt>"
    )


def form_field(kind, name, result, extra=""):
    """A legacy form field (FORMTEXT, FORMCHECKBOX, FORMDROPDOWN)."""
    inner = {
        "FORMTEXT": f'<w:textInput><w:default w:val="{x(result)}"/></w:textInput>',
        "FORMCHECKBOX": f'<w:checkBox><w:sizeAuto/><w:default w:val="{int(result == "1")}"/></w:checkBox>',
        "FORMDROPDOWN": extra,
    }[kind]
    ff = f'<w:ffData><w:name w:val="{x(name)}"/><w:enabled/><w:calcOnExit w:val="0"/>{inner}</w:ffData>'
    begin = f'<w:r><w:fldChar w:fldCharType="begin">{ff}</w:fldChar></w:r>'
    instr = f'<w:r><w:instrText xml:space="preserve"> {kind} </w:instrText></w:r>'
    if kind == "FORMCHECKBOX":
        return begin + instr + '<w:r><w:fldChar w:fldCharType="end"/></w:r>'
    return (
        begin
        + instr
        + '<w:r><w:fldChar w:fldCharType="separate"/></w:r>'
        + R(result)
        + '<w:r><w:fldChar w:fldCharType="end"/></w:r>'
    )


def toc(entries, switches='\\o "1-3" \\h \\z \\u', heading="Contents"):
    """A TOC field with its cached entries: (level, text, bookmark, page)."""
    out = [P(heading, style="TOCHeading")] if heading else []
    for k, (level, text, mark, page) in enumerate(entries):
        start = ""
        if k == 0:
            start = (
                f'<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> TOC '
                f'{x(switches)} </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r>'
            )
        pageref = field("PAGEREF " + mark + " \\h", str(page), "<w:rPr><w:webHidden/></w:rPr>")
        link = (
            f'<w:hyperlink w:anchor="{mark}" w:history="1">{R(text, rpr(style="Hyperlink"))}'
            f"<w:r><w:rPr><w:webHidden/></w:rPr><w:tab/></w:r>{pageref}</w:hyperlink>"
        )
        end = '<w:r><w:fldChar w:fldCharType="end"/></w:r>' if k == len(entries) - 1 else ""
        out.append(P(start + link + end, style=f"TOC{level}"))
    return out


def heading(level, text, mark=None, bid=None, num=None):
    run = R(text)
    if mark:
        run = bookmark(bid, mark, run)
    return P(run, style=f"Heading{level}", num=num)


def hr(pct, align="center", color="a0a0a0", height=1.5):
    """A VML horizontal rule of `pct` per cent of the column."""
    return (
        f'<w:r><w:pict><v:rect id="hr{pct}{align}" style="width:0;height:{height}pt" o:hralign="{align}" '
        f'o:hrstd="t" o:hrpct="{pct * 10}" o:hr="t" fillcolor="#{color}" stroked="f"/></w:pict></w:r>'
    )


# ---------------------------------------------------------------------------------------------------------------
# The documents. Each function builds one; DOCUMENTS lists them in the order of the corpus manifest.


def letterhead_sections(out):
    d = Doc("letterhead-sections", "Studienarbeit: Wärmeübertragung")
    d.styles.append(
        (
            "paragraph",
            "Institut",
            "Institut",
            "Normal",
            '<w:spacing w:after="0"/>',
            '<w:smallCaps/><w:color w:val="1F4E79"/><w:sz w:val="18"/>',
        )
    )
    logo = d.media(wmf(), "wmf")
    first_header = d.header(
        P(
            d.vml_text_box(
                [
                    P(R("Fakultät Maschinenwesen", rpr(b=True)), style="Institut"),
                    P("Institut für Energietechnik", style="Institut"),
                    P("Professur für Verbrennung, Wärme- und Stoffübertragung", style="Institut"),
                ],
                250,
                54,
                0,
                0,
                fill="FFFFFF",
            ),
            d.vml_picture(logo, 90, 54),
            jc="right",
            style="Header",
        ),
        P(R("Postadresse:\tBesucheradresse:", rpr(small_caps=True, sz=16)), style="Header"),
    )
    styleref = rpr(i=True, sz=18)
    default_header = d.header(
        P(
            field('STYLEREF "Heading 1" \\* MERGEFORMAT', "Einleitung", styleref),
            R("\tSeite ", styleref),
            field("PAGE", "2", styleref),
            style="Header",
        )
    )
    shapes_header = d.header(
        P(
            R("Technische Universität · Studienarbeit", rpr(small_caps=True, sz=18)),
            '<w:r><w:pict><v:rect id="band" style="position:absolute;margin-left:-20pt;margin-top:-8pt;width:500pt;'
            'height:4pt;z-index:-1" fillcolor="#1F4E79" stroked="f"/></w:pict></w:r>'
            '<w:r><w:pict><v:oval id="dot" style="position:absolute;margin-left:470pt;margin-top:-14pt;width:14pt;'
            'height:14pt" fillcolor="#ED7D31" stroked="f"/></w:pict></w:r>',
            style="Header",
        )
    )
    footer = d.footer(P(R("Studienarbeit\t\t"), field("PAGE", "1"), style="Footer"))
    first_footer = d.footer(P(R("Dresden, im Januar", rpr(sz=18)), style="Footer"))
    d.p(R("Studienarbeit", rpr(b=True, sz=48)), jc="center", ppr='<w:spacing w:before="2400"/>')
    d.p(R("Wärme- und Stoffübertragung an einer beheizten Platte", rpr(sz=32)), jc="center")
    d.p(
        R("Name:\tMax Beispiel\nMatrikel:\t1234567\nBetreuer:\tDr.-Ing. Erika Muster", rpr(sz=24)),
        ppr='<w:tabs><w:tab w:val="left" w:pos="2268"/></w:tabs><w:spacing w:before="1200"/>',
    )
    sections = [("Einleitung", 2), ("Grundlagen", 3), ("Versuchsaufbau", 3), ("Ergebnisse", 3), ("Zusammenfassung", 2)]
    headers = [("first", first_header), ("default", default_header)]
    d.add(
        P(sect=d.sect(page=A4, headers=headers, footers=[("first", first_footer), ("default", footer)], title_pg=True))
    )
    r = d.r
    for k, (title, paras) in enumerate(sections):
        d.add(heading(1, f"{k + 1} {title}"))
        for j in range(paras + 2):
            text = " ".join(DE[(k + j + n) % len(DE)] for n in range(4))
            if j == 1:
                cid = d.comment("Bitte die Quelle der Grenzwerte angeben.")
                d.p(
                    R("Die "),
                    R("Prüfvorschrift", rpr(small_caps=True)),
                    f'<w:commentRangeStart w:id="{cid}"/>',
                    R(" legt die Grenzwerte fest. "),
                    f'<w:commentRangeEnd w:id="{cid}"/>',
                    f'<w:r>{rpr(style="CommentReference")}<w:commentReference w:id="{cid}"/></w:r>',
                    R(text),
                )
            else:
                d.p(text)
        d.p(
            R("Größe\tWert\tEinheit\nTemperatur\t" + f"{20 + r.randrange(60)}" + "\t°C\nDruck\t1,013\tbar"),
            ppr='<w:tabs><w:tab w:val="left" w:pos="2835"/><w:tab w:val="decimal" w:pos="5103"/></w:tabs>',
        )
        hdrs = [("default", shapes_header if k % 2 else default_header)]
        pg = 'w:start="1"' if k == 0 else None
        if k < len(sections) - 1:
            d.add(P(sect=d.sect(page=A4, headers=hdrs, footers=[("default", footer)], pg_num=pg)))
    d.save(out, d.sect(page=A4, headers=[("default", default_header)], footers=[("default", footer)]))


def table_vertical_text(out):
    d = Doc("table-vertical-text", "A table with vertical text")
    color = "4BACC6"
    d.p(R("Before.", rpr(b=True)), ppr='<w:spacing w:after="0"/>', jc="both")
    tc_b = borders("tcBorders", color, 8)
    vert = (["ABCD"], f'<w:vMerge w:val="restart"/>{tc_b}<w:textDirection w:val="btLr"/>')
    vert_down = (["EFGH"], f'<w:vMerge w:val="restart"/>{tc_b}<w:textDirection w:val="tbRl"/>')
    cont = ([""], f"<w:vMerge/>{tc_b}")
    rows = [
        [([""], tc_b + shd("auto")), ([P(R("B1", rpr(b=True)), jc="center")], tc_b), ([P("C1")], tc_b)],
        [vert, ([P("1")], tc_b), vert_down],
        [cont, ([P("2")], tc_b + shd("DAEEF3")), cont],
        [cont, ([P("3")], tc_b), cont],
        [cont, ([P("4")], tc_b + shd("DAEEF3")), cont],
        [cont, ([P("5")], tc_b), cont],
    ]
    trpr = ['<w:cantSplit/><w:trHeight w:val="23"/>'] + ['<w:trHeight w:val="300"/>'] * 5
    d.add(table(rows, [960, 960, 960], border_color=color, trpr=trpr))
    d.p(R("After.", rpr(b=True)))
    d.save(out, d.sect(page=(11900, 16840)))


def endnote_continuation(out):
    d = Doc("endnote-continuation", "An endnote that continues")
    d.note_separators["endnote"] = (
        "<w:separator/>",
        "<w:continuationSeparator/>",
    )
    r = d.r
    note = d.note("endnote", *[prose(r, 7) for _ in range(4)])
    d.p(R("Body text"), note)
    d.p(prose(r, 3))
    d.save(out, d.sect(page=(12242, 5670), margins=(720, 1134, 720, 1134)))


def equation_captions(out):
    d = Doc("equation-captions", "Equations with captions")
    cap = rpr(style=None)
    d.p("Two display equations, one captioned above and one below.")
    d.p(R("Equation "), field("SEQ Equation \\* ARABIC", "1", cap), style="Caption", jc="center")
    d.add(omath_para(BINOMIAL))
    d.add(omath_para(TAYLOR))
    d.p(R("Equation "), field("SEQ Equation \\* ARABIC", "2", cap), style="Caption", jc="center")
    d.p(R("Inline, the roots are "), omath(QUADRATIC), R(" and the area of a circle is "), omath(AREA), R("."))
    d.save(out, d.sect())


def smartart_pictures(out):
    d = Doc("smartart-pictures", "SmartArt with pictures")
    sizes = [("image1", 61, 71, 75), ("image2", 195, 129, 75), ("image3", 129, 195, 80), ("image4", 97, 61, 75)]
    names = []
    for name, w, h, q in sizes:
        d.part(f"word/media/{name}.jpeg", jpeg(f"smartart-{name}", w, h, q, 2))
        names.append(f"{name}.jpeg")
    d.defaults["jpeg"] = "image/jpeg"
    d.p(
        R("Hayden Management", rpr(b=True, sz=36, color="1F4E79")), ppr=borders("pBdr", "1F4E79", 12, sides=("bottom",))
    )
    d.p(smartart(d, ["Plan", "Build", "Review"], 420, 140, pictures=names[:2]))
    pics = [d.rel("image", f"media/{n}") for n in names[2:]]
    bullets = d.abstract_num([("bullet", "•", 720)])
    for _k in range(3):
        d.p(R(sentence(d.r)), num=(bullets, 0), style="ListParagraph")
    rows = [
        [
            ([P(R("Quarter", rpr(b=True)))], shd("1F4E79")),
            ([P(R("Team", rpr(b=True)))], shd("1F4E79")),
            ([P(R("Picture", rpr(b=True)))], shd("1F4E79")),
        ],
        [([P("1")], shd("DEEAF6")), ([P("North")], shd("DEEAF6")), [P(d.inline_picture(pics[0], 43, 65))]],
        [([P("2")], shd("FFFFFF")), ([P("South")], shd("FBE4D5")), [P(d.inline_picture(pics[1], 73, 46))]],
    ]
    d.add(table(rows, [1500, 3000, 2500]))
    d.p("1", jc="right", ppr=borders("pBdr", "A5A5A5", 4, sides=("top",)))
    d.save(out, d.sect())


def bidi_worksheet(out):
    d = Doc("bidi-worksheet", "Worksheet with right-to-left instructions")
    picture = d.media(gif("bidi-worksheet", 120, 80), "gif")
    rtl = rpr(rtl=True, font="Arial")
    header = d.header(
        P(R("Revision sheet"), R("\tPage "), field("PAGE", "1"), R(" of "), field("NUMPAGES", "2"), style="Header")
    )
    footer = d.footer(P(R("Name: "), sdt_text("Student"), R("   Date: "), sdt_date("Date"), style="Footer"))
    first = d.footer(P("", style="Footer"))
    d.p(R("ENGLISH SHEET", rpr(b=True, sz=32)), jc="center")
    d.p(R(AR[0], rtl), bidi=True, jc="left")
    d.p(
        d.text_box(
            [P(R("Read each item, then circle the correct choice.", rpr(b=True))), P(R(AR[1], rtl), bidi=True)],
            200,
            60,
            280,
            0,
            rel_width=40,
        ),
        d.inline_picture(picture, 90, 60),
    )
    questions = d.abstract_num([("decimal", "%1.", 360), ("upperLetter", "%2.", 720)])
    for k in range(18):
        d.p(R(f"{'.' * 9} " + sentence(d.r, 9)), num=(questions, 0))
        d.p(R("A. every   B. each   C. most   D. all"), ppr='<w:ind w:left="720"/>')
        if k % 4 == 3:
            d.p(R(AR[k % len(AR)], rtl), bidi=True)
    d.save(
        out,
        d.sect(
            page=A4,
            headers=[("default", header)],
            footers=[("default", footer), ("first", first)],
            title_pg=True,
            pg_num='w:start="1"',
        ),
    )


def content_control_form(out):
    d = Doc("content-control-form", "Training equipment request")
    logo = d.media(png("form-logo", 160, 90), "png")
    stamp = d.media(gif("form-stamp", 80, 80), "gif")
    link = d.rel("hyperlink", "https://example.org/training", external=True)
    d.p(
        R("Training Equipment Request", rpr(b=True, sz=36)),
        d.anchored_picture(logo, 96, 54, 380, -6),
        ppr=borders("pBdr", "2E74B5", 12, sides=("bottom",)),
    )
    d.p(R("Contact Information", rpr(b=True, sz=26, color="2E74B5")))
    label = rpr(b=True)
    rows = [
        [[P(R("Requester/Trainer:", label))], [P(sdt_text("Requester"))]],
        [[P(R("Email:", label))], [P(sdt_text("Email"))]],
        [[P(R("Course Name:", label))], [P(sdt_text("Course"))]],
        [[P(R("Setup Date:", label))], [P(sdt_date("Setup date"))]],
        [[P(R("Class Start Time:", label))], [P(sdt_dropdown("Start", ["8:00", "9:00", "10:00"], "9:00"))]],
        [[P(R("Projector needed:", label))], [P(sdt_checkbox(True), R(" yes  "), sdt_checkbox(False), R(" no"))]],
    ]
    d.add(table(rows, [3000, 6000], grid=False))
    d.p(R("Shipping", rpr(b=True, sz=26, color="2E74B5")), d.anchored_picture(stamp, 48, 48, 420, 0))
    rows = [[[P(R("Item", label))], [P(R("Quantity", label))], [P(R("Notes", label))]]]
    for k in range(4):
        rows.append(
            [
                [P(sdt_text(f"Item {k + 1}"))],
                [P(sdt_text(f"Qty {k + 1}", placeholder="0"))],
                [P(sdt_text(f"Notes {k + 1}"))],
            ]
        )
    d.add(table(rows, [3500, 1500, 4000]))
    d.p(
        R("Send the form to the training office; the form's instructions are at "),
        f'<w:hyperlink r:id="{link}" w:history="1">{R("example.org/training", rpr(style="Hyperlink"))}</w:hyperlink>',
        R("."),
    )
    d.save(out, d.sect(footers=[("default", d.footer(P(R("Training office · form 12"), style="Footer")))]))


def page_background_gradient(out):
    d = Doc("page-background-gradient", "A page background")
    d.background = (
        '<w:background w:color="000082"><v:background id="bg" o:bwmode="white" fillcolor="#000082">'
        '<v:fill color2="#ff8200" angle="-135" colors="0 #000082;19661f #66008f;42598f #ba0066;58982f red;1 #ff8200" '
        'method="none" focus="100%" type="gradientRadial"><o:fill v:ext="view" type="gradientCenter"/></v:fill>'
        "</v:background></w:background>"
    )
    d.settings = "<w:displayBackgroundShape/>"
    d.p(R("Background", rpr(b=True, sz=48, color="FFFFFF")), jc="center", ppr='<w:spacing w:before="4800"/>')
    d.save(out, d.sect(margins=(1417, 1417, 1417, 1417)))


def floating_table_math(out):
    d = Doc("floating-table-math", "A floating table of formulas")
    footer = d.footer(P(field("PAGE", "1"), style="Footer", jc="center"))
    d.p(R("Polarimeter Measurement", rpr(b=True, sz=28)))
    d.p(prose(d.r, 4))
    var_x = omath(m_sup(m_r("σ"), m_r("2")) + m_sub(m_r("E"), m_r("x")))
    var_y = omath(m_sup(m_r("σ"), m_r("2")) + m_sub(m_r("E"), m_r("y")))
    hdr = rpr(b=True)
    rows = [[[P(R("Measurement", hdr))], [P(var_x, jc="center")], [P(var_y, jc="center")]]]
    for name in ["No Fiber Loop", "Access 10m Fiber", "Azimuth Move", "Elevation Move"]:
        rows.append(
            [
                [P(name)],
                [P(f"{d.r.uniform(0.000005, 0.0003):.7f}", jc="right")],
                [P(f"{d.r.uniform(0.000005, 0.0003):.7f}", jc="right")],
            ]
        )
    rows.append(
        [[P(R("Ratio", hdr))], [P(omath(m_frac(m_r("2") + m_sub(m_r("σ"), m_r("x")), m_r("N"))))], [P(omath(VARIANCE))]]
    )
    tblpr = (
        '<w:tblpPr w:leftFromText="180" w:rightFromText="180" w:vertAnchor="text" w:horzAnchor="margin" '
        'w:tblpXSpec="right" w:tblpY="200"/><w:tblOverlap w:val="never"/>'
    )
    d.add(table(rows, [2200, 1800, 2200], tblpr=tblpr))
    d.p(R("Table 2. Variance of the samples", rpr(i=True)), style="Caption")
    for _ in range(4):
        d.p(prose(d.r, 5))
    d.save(out, d.sect(footers=[("first", footer), ("default", footer)], title_pg=True, pg_num='w:start="1"'))


def tracked_changes_sections(out):
    d = Doc("tracked-changes-sections", "Tracked changes across sections")
    r = d.r
    header = d.header(P("Tracked changes", style="Header"))
    footer = d.footer(P(R("Page "), field("PAGE", "1"), style="Footer", jc="center"))
    nums = d.abstract_num([("decimal", "[%1.]", 360), ("decimal", "%1.%2", 720)])
    rid = 1
    for s in range(10):
        d.add(heading(2, f"Part {s + 1}"))
        for k in range(3 + s % 3):
            words = sentence(r, 12).split()
            runs = []
            for j, w in enumerate(words):
                if (j + k + s) % 5 == 1:
                    runs.append(tracked("ins", rid, w + " ", author="Reviewer One"))
                elif (j + k + s) % 5 == 3:
                    runs.append(tracked("del", rid, w + " ", author="Reviewer Two"))
                else:
                    runs.append(R(w + " "))
                rid += 1
            num = (nums, k % 2) if k < 2 else None
            d.p(*runs, num=num)
        d.p(
            R("BodyText\tBodySpacePreserve   with  spaces\tYYY"),
            ppr='<w:tabs><w:tab w:val="left" w:pos="2000"/><w:tab w:val="left" w:pos="6000"/></w:tabs>',
        )
        if s % 3 == 1:
            d.p(
                R("A frame beside the text: " + sentence(r, 6), rpr(i=True)),
                ppr='<w:framePr w:w="2600" w:hSpace="180" w:wrap="around" w:vAnchor="text" w:hAnchor="margin" '
                'w:xAlign="right" w:y="1"/>' + borders("pBdr", "7F7F7F", 4) + shd("F2F2F2"),
            )
        kind = "continuous" if s % 2 else "nextPage"
        cols = 2 if s in (3, 4) else 1
        pg = 'w:start="1"' if s in (2, 6) else None
        if s < 9:
            d.add(
                P(
                    sect=d.sect(
                        kind=kind, cols=cols, headers=[("default", header)], footers=[("default", footer)], pg_num=pg
                    )
                )
            )
    d.save(out, d.sect(kind="nextPage", headers=[("default", header)], footers=[("default", footer)]))


def strict_ooxml(out):
    d = Doc("strict-ooxml", "A Strict Open XML document")
    d.strict = True
    pic = d.media(gif("strict", 100, 60), "gif")
    header = d.header(P("This is a header.", style="Header"))
    d.p("Hello world!")
    d.p("This is a chart:")
    d.p(chart(d, "bar", ["1", "2", "3"], [("Series 1", [3.5, 2.5, 1.5]), ("Series 2", [2.0, 3.0, 1.0])], "Chart Title"))
    d.p("And this is a smart-art:")
    d.p(smartart(d, ["a", "b", "c"], 360, 100))
    d.p(R("This is math: "), omath(AREA))
    d.p(R("And a picture: "), d.inline_picture(pic, 75, 45))
    d.save(out, d.sect(headers=[("default", header)]))


def math_paper_damaged_table(out):
    d = Doc("math-paper-damaged-table", "Reduction of quadratic forms")
    d.settings = "<w:evenAndOddHeaders/>"
    first = d.header(P(R("Preprint", rpr(small_caps=True)), style="Header", jc="right"))
    odd = d.header(P(R("Reduction of quadratic forms\t\t"), field("PAGE", "1"), style="Header"))
    even = d.header(P(field("PAGE", "2"), R("\t\tA. Author"), style="Header"))
    d.p(R("Reduction of binary quadratic forms", rpr(b=True, sz=34)), jc="center")
    d.p(
        R("Abstract. ", rpr(b=True)),
        R(prose(d.r, 3)),
        ppr=borders("pBdr", "808080", 4) + '<w:ind w:left="567" w:right="567"/>',
    )
    sections = d.abstract_num([("decimal", "%1.", 360)])
    refs = d.abstract_num([("decimal", "[%1]", 567)])
    formulas = [BINOMIAL, QUADRATIC, VARIANCE, AREA, INTEGRAL, TAYLOR]
    k = 0
    for s in range(4):
        d.p(R(["Introduction", "The reduction", "An algorithm", "Results"][s], rpr(b=True)), num=(sections, 0))
        for _ in range(3):
            d.p(
                R(sentence(d.r) + " Let "),
                omath(m_sup(m_r("X"), m_r("2")) + m_r("+XY−") + m_sup(m_r("Y"), m_r("2"))),
                R(" be the form, where "),
                omath(m_sub(m_r("s"), m_r("5"))),
                R(" is the odd root of "),
                omath(m_sup(m_r("x"), m_r("2")) + m_r("=5 mod p")),
                R(". " + sentence(d.r)),
            )
            d.add(omath_para(formulas[k % len(formulas)]))
            k += 1
    # A damaged table: one grid column fewer than the cells, a cell without a paragraph, a span past the grid,
    # no widths.
    d.add(
        '<w:tbl><w:tblPr><w:tblW w:w="0" w:type="auto"/>'
        + borders("tblBorders", "000000", 4, sides=("top", "left", "bottom", "right", "insideH", "insideV"))
        + '</w:tblPr><w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="3000"/></w:tblGrid>'
        "<w:tr><w:tc><w:p><w:r><w:t>p</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>reduced form</w:t></w:r></w:p>"
        "</w:tc><w:tc><w:p><w:r><w:t>steps</w:t></w:r></w:p></w:tc></w:tr>"
        '<w:tr><w:tc><w:p><w:r><w:t>11</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:gridSpan w:val="4"/></w:tcPr>'
        "<w:p><w:r><w:t>X² + XY − Y²</w:t></w:r></w:p></w:tc></w:tr>"
        "<w:tr><w:tc><w:p><w:r><w:t>19</w:t></w:r></w:p></w:tc><w:tc></w:tc><w:tc><w:p><w:r><w:t>3</w:t></w:r>"
        "</w:p></w:tc></w:tr></w:tbl>"
    )
    d.p("References", style="Heading2")
    for k in range(5):
        d.p(R(f"A. Author{k}, " + sentence(d.r, 7), rpr(i=k % 2 == 0)), num=(refs, 0))
    d.save(
        out,
        d.sect(
            page=(11907, 16840),
            headers=[("first", first), ("default", odd), ("even", even)],
            title_pg=True,
            pg_num='w:start="1"',
        ),
    )


def floating_tables_columns(out):
    d = Doc("floating-tables-columns", "Floating tables in columns")
    pic = d.media(png("floating-tables", 180, 120), "png")
    headers = [
        ("default", d.header(P("Floating tables", style="Header"))),
        ("first", d.header(P(R("Report", rpr(b=True)), style="Header"))),
        ("even", d.header(P("Even page", style="Header"))),
    ]
    footers = [
        ("default", d.footer(P(field("PAGE", "1"), style="Footer", jc="right"))),
        ("first", d.footer(P("", style="Footer"))),
        ("even", d.footer(P(field("PAGE", "2"), style="Footer"))),
    ]
    nums = d.abstract_num([("decimal", "%1.", 360)])
    d.p(R("Xxxxxxxx Xxxxxxxx Report", rpr(b=True, sz=36)))
    d.p(
        d.text_box([P(R("Summary", rpr(b=True))), P(sentence(d.r, 12))], 160, 90, 300, 10, fill="DEEAF6"),
        R(prose(d.r, 4)),
    )
    d.add(P(sect=d.sect(kind="nextPage", headers=headers, footers=footers, title_pg=True)))

    def floating(x_tw, y_tw, h_anchor, title):
        rows = [[([P(R(title, rpr(b=True)))], shd("BDD7EE")), ([P(R("Value", rpr(b=True)))], shd("BDD7EE"))]]
        for k in range(4):
            rows.append([[P(f"Row {k + 1}")], [P(f"{d.r.randrange(100, 999)}", jc="right")]])
        pr = (
            f'<w:tblpPr w:leftFromText="141" w:rightFromText="141" w:vertAnchor="text" w:horzAnchor="{h_anchor}" '
            f'w:tblpX="{x_tw}" w:tblpY="{y_tw}"/><w:tblOverlap w:val="never"/>'
        )
        return table(rows, [1600, 1000], tblpr=pr)

    d.add(floating(5800, 300, "page", "Floating A"))
    for k in range(4):
        d.p(R(prose(d.r, 3)), num=(nums, 0) if k < 3 else None)
    d.p(d.anchored_picture(pic, 108, 72, 0, 0, wrap="square"), R(prose(d.r, 4)))
    d.add(floating(0, 200, "margin", "Floating B"))
    d.p(d.text_box([P("A second box beside the columns.")], 140, 40, 0, 30, fill="FFF2CC", line="BF9000"))
    for _ in range(6):
        d.p(prose(d.r, 4))
    d.save(out, d.sect(kind="continuous", cols=2, headers=headers, footers=footers))


def citations_comments(out):
    d = Doc("citations-comments", "Citations, a bibliography and comments")
    sources = (
        '<b:Sources xmlns:b="http://schemas.openxmlformats.org/officeDocument/2006/bibliography" '
        'xmlns="http://schemas.openxmlformats.org/officeDocument/2006/bibliography" SelectedStyle="\\APA.XSL" '
        'StyleName="APA"><b:Source><b:Tag>Ber20</b:Tag><b:SourceType>Book</b:SourceType><b:Author><b:Author>'
        "<b:NameList><b:Person><b:Last>Berg</b:Last><b:First>Anna</b:First></b:Person></b:NameList></b:Author>"
        "</b:Author><b:Title>Measuring Heat</b:Title><b:Year>2020</b:Year><b:City>Dresden</b:City>"
        "<b:Publisher>Example Press</b:Publisher></b:Source><b:Source><b:Tag>Nak18</b:Tag>"
        "<b:SourceType>JournalArticle</b:SourceType><b:Author><b:Author><b:NameList><b:Person><b:Last>Nakamura"
        "</b:Last><b:First>Ken</b:First></b:Person></b:NameList></b:Author></b:Author><b:Title>Thin plates"
        "</b:Title><b:JournalName>Journal of Examples</b:JournalName><b:Year>2018</b:Year></b:Source></b:Sources>"
    )
    d.part("customXml/item1.xml", DECL + sources)
    d.part(
        "customXml/itemProps1.xml",
        f'{DECL}<ds:datastoreItem ds:itemID="{{6F2E1B4A-0000-4000-8000-000000000001}}" '
        'xmlns:ds="http://schemas.openxmlformats.org/officeDocument/2006/customXml"><ds:schemaRefs><ds:schemaRef '
        'ds:uri="http://schemas.openxmlformats.org/officeDocument/2006/bibliography"/></ds:schemaRefs>'
        "</ds:datastoreItem>",
        "application/vnd.openxmlformats-officedocument.customXmlProperties+xml",
    )
    d.part(
        "customXml/_rels/item1.xml.rels",
        f'{DECL}<Relationships xmlns="{PKG_REL}"><Relationship Id="rId1" Type="{REL}customXmlProps" '
        'Target="itemProps1.xml"/></Relationships>',
    )
    d.rel("customXml", "../customXml/item1.xml")

    def citation(tag, text):
        cited = field("CITATION " + tag + " \\l 1033", text)
        return (
            f'<w:sdt><w:sdtPr><w:id w:val="{next(SDT_IDS)}"/><w:citation/></w:sdtPr><w:sdtContent>'
            f"{cited}</w:sdtContent></w:sdt>"
        )

    cid = d.comment("Is this the right edition?")
    cid2 = d.comment("Agreed, keep it.", author="Reviewer Two")
    d.p(
        R(sentence(d.r) + " Heat moves faster through a thin plate "),
        citation("Ber20", "(Berg, 2020)"),
        R(", as measured before "),
        citation("Nak18", "(Nakamura, 2018)"),
        R(". "),
        f'<w:commentRangeStart w:id="{cid}"/>',
        f'<w:commentRangeStart w:id="{cid2}"/>',
        R("The second source gives the method."),
        f'<w:commentRangeEnd w:id="{cid}"/>',
        f'<w:r>{rpr(style="CommentReference")}<w:commentReference w:id="{cid}"/></w:r>',
        f'<w:commentRangeEnd w:id="{cid2}"/>',
        f'<w:r>{rpr(style="CommentReference")}<w:commentReference w:id="{cid2}"/></w:r>',
    )
    rows = [
        [([P(R("Source", rpr(b=True)))], shd("D9D9D9")), ([P(R("Year", rpr(b=True)))], shd("D9D9D9"))],
        [([P("Berg")], shd("F2F2F2")), [P("2020")]],
        [[P("Nakamura")], ([P("2018")], shd("F2F2F2"))],
    ]
    d.add(table(rows, [4000, 2000]))
    entries = [
        P(
            R("Berg, A. (2020). "),
            R("Measuring Heat", rpr(i=True)),
            R(". Dresden: Example Press."),
            style="Bibliography",
        ),
        P(R("Nakamura, K. (2018). Thin plates. "), R("Journal of Examples", rpr(i=True)), R("."), style="Bibliography"),
    ]
    bib = (
        '<w:sdt><w:sdtPr><w:id w:val="77"/><w:docPartObj><w:docPartGallery w:val="Bibliographies"/>'
        "<w:docPartUnique/></w:docPartObj></w:sdtPr><w:sdtContent>"
        + P("Bibliography", style="Heading1")
        + '<w:p><w:pPr><w:pStyle w:val="Bibliography"/></w:pPr><w:r><w:fldChar w:fldCharType="begin"/></w:r>'
        '<w:r><w:instrText xml:space="preserve"> BIBLIOGRAPHY </w:instrText></w:r>'
        '<w:r><w:fldChar w:fldCharType="separate"/></w:r>'
        + R("Berg, A. (2020). Measuring Heat. Dresden: Example Press.")
        + "</w:p>"
        + entries[1].replace("</w:p>", '<w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>')
        + "</w:sdtContent></w:sdt>"
    )
    d.add(bib)
    d.save(out, d.sect())


def chart_ole_textbox(out):
    d = Doc("chart-ole-textbox", "A chart, an embedded spreadsheet and a text box")
    preview = d.media(png("ole-preview", 240, 120), "png")
    pic = d.media(png("chart-ole-picture", 120, 80), "png")
    d.part("word/embeddings/Worksheet1.xlsx", xlsx([["Month", "Units"], ["Jan", 12], ["Feb", 15], ["Mar", 9]]))
    d.defaults["xlsx"] = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    pkg = d.rel("package", "embeddings/Worksheet1.xlsx")
    d.p(R("asdf", rpr(b=True)))
    d.p(
        chart(
            d,
            "line",
            ["Jan", "Feb", "Mar", "Apr"],
            [("Units", [12, 15, 9, 14]), ("Target", [10, 12, 12, 13])],
            "Units by month",
            340,
            200,
        )
    )
    ole = (
        '<w:r><w:object w:dxaOrig="4800" w:dyaOrig="2400"><v:shapetype id="_x0000_t75" coordsize="21600,21600" '
        'o:spt="75" o:preferrelative="t" path="m@4@5l@4@11@9@11@9@5xe" filled="f" stroked="f"><v:stroke '
        'joinstyle="miter"/><v:path o:extrusionok="f" gradientshapeok="t" o:connecttype="rect"/><o:lock v:ext="edit" '
        'aspectratio="t"/></v:shapetype><v:shape id="_x0000_i1025" type="#_x0000_t75" style="width:240pt;'
        f'height:120pt" o:ole=""><v:imagedata r:id="{preview}" o:title=""/></v:shape><o:OLEObject Type="Embed" '
        f'ProgID="Excel.Sheet.12" ShapeID="_x0000_i1025" DrawAspect="Content" ObjectID="_1700000001" r:id="{pkg}"/>'
        "</w:object></w:r>"
    )
    d.p(ole)
    d.p(
        d.text_box(
            [P(R("A note in a text box", rpr(b=True))), P(sentence(d.r, 10))],
            180,
            70,
            280,
            0,
            fill="FCE4D6",
            line="C55A11",
        ),
        d.inline_picture(pic, 72, 48),
    )
    d.save(out, d.sect())


def toc_section_margins(out):
    d = Doc("toc-section-margins", "Sections with their own margins")
    footer = d.footer(P(field("PAGE", "1"), style="Footer", jc="center"))
    header = d.header(P("Section margins", style="Header"))
    hf = {"headers": [("default", header)], "footers": [("default", footer)]}
    nums = d.abstract_num([("decimal", "%1.", 432), ("decimal", "%1.%2.", 576)])
    titles = [f"Test{k}" for k in range(6)]
    pages = [1, 2, 3, 3, 4, 5]
    d.add(*toc([(1, t, f"_Toc{k:04d}", p) for k, (t, p) in enumerate(zip(titles, pages, strict=True))]))
    d.add(P(sect=d.sect(pg_num='w:fmt="upperRoman" w:start="1"', **hf)))
    margins = [
        (1440, 1440, 1440, 1440),
        (720, 2880, 720, 720),
        (2160, 1080, 2160, 1080),
        (1440, 720, 1440, 2880),
        (1080, 1080, 1080, 1080),
        (1440, 1440, 1440, 1440),
    ]
    for k, title in enumerate(titles):
        d.add(heading(1, title, f"_Toc{k:04d}", 100 + k, num=(nums, 0)))
        d.add(heading(2, "Detail", num=(nums, 1)))
        runs = [R(prose(d.r, 4))]
        if k == 1:
            runs.append(d.note("footnote", "A footnote in the narrow section: " + sentence(d.r)))
        d.p(*runs)
        d.p(prose(d.r, 4))
        if k == 3:
            d.p(column_break(), R(prose(d.r, 3)))
        if k < len(titles) - 1:
            cols = 2 if k == 3 else 1
            pg = 'w:fmt="decimal" w:start="1"' if k == 0 else None
            kind = "continuous" if k == 2 else "nextPage"
            d.add(P(sect=d.sect(margins=margins[k], cols=cols, pg_num=pg, kind=kind if k else None, **hf)))
    d.save(out, d.sect(margins=margins[-1], kind="nextPage", **hf))


def tracked_table_cells(out):
    d = Doc("tracked-table-cells", "A calendar with tracked cells")
    header = d.header(P(R("Beosztás", rpr(b=True)), style="Header"))
    footer = d.footer(P(R("Oldal "), field("PAGE", "1"), style="Footer", jc="right"))
    d.p(R("Something", rpr(b=True, sz=28)))
    d.p(HU[0])
    days = 24
    widths = [1700] + [560] * days
    cols_hdr = [[P(R("a hónap napjai", rpr(b=True, sz=16)))]] + [
        [P(R(f"{k + 1}.", rpr(sz=16)), jc="center")] for k in range(days)
    ]
    rows = [cols_hdr]
    rid = 500
    for name in ["Anna", "Béla", "Csilla", "Dénes", "Erzsébet", "Ferenc", "Gábor", "Hajnal"]:
        row = [[P(name)]]
        for k in range(days):
            mark = "X" if (k + len(name)) % 3 == 0 else ""
            tcpr = ""
            content = [P(R(mark, rpr(sz=16)), jc="center")]
            if (k + len(name)) % 7 == 0:
                tcpr = f'<w:cellIns w:id="{rid}" w:author="Reviewer One" w:date="{STAMP}"/>'
                content = [P(tracked("ins", rid + 1, mark or "+", rpr(sz=16)), jc="center")]
            elif (k + len(name)) % 11 == 0:
                tcpr = f'<w:cellDel w:id="{rid}" w:author="Reviewer Two" w:date="{STAMP}"/>'
            rid += 2
            row.append((content, tcpr))
        rows.append(row)
    d.add(table(rows, widths, trpr='<w:trHeight w:val="400"/>'))
    d.p(tracked("ins", rid, "Módosítva: " + HU[1]), R(" " + HU[2]))
    hf = {
        "headers": [("default", header)],
        "footers": [("default", footer)],
        "landscape": True,
        "page": A4,
        "margins": (720, 720, 720, 720),
    }
    d.add(P(sect=d.sect(title_pg=True, **hf)))
    d.p(R("Second month", rpr(b=True, sz=28)))
    d.add(table(rows[:5], widths))
    d.save(out, d.sect(title_pg=True, kind="nextPage", **hf))


def table_formulas_unicode(out):
    d = Doc("table-formulas-unicode", "Table formulas naming bookmarks in several scripts")
    hf = {}
    for kind in ("default", "first", "even"):
        hf.setdefault("headers", []).append((kind, d.header(P(f"Formulas ({kind})", style="Header"))))
        hf.setdefault("footers", []).append((kind, d.footer(P(f"Footer ({kind})", style="Footer"))))
    marks = [("預期結果", "預期結果"), ("نتيجةمتوقعة", "نتيجة متوقعة"), ("ExpectedResult", "Expected Result")]
    d.p(R("MS Word Formula", rpr(b=True, sz=28)))
    rows = [[[P(R("Formula", rpr(b=True)))], [P(R("Bookmark", rpr(b=True)))], [P(R("Result", rpr(b=True)))]]]
    for k, (mark, label) in enumerate(marks):
        right = k == 1
        rows.append(
            [
                [P(R(f"=defined({mark})", rpr(rtl=right)), bidi=right)],
                [P(bookmark(10 + k, mark, R(label, rpr(rtl=right))), bidi=right)],
                [P(field(f"= defined({mark})", "1"), jc="right")],
            ]
        )
    rows.append([[P("=SUM(ABOVE)")], [P("")], [P(field("=SUM(ABOVE)", "3"), jc="right")]])
    rows.append([[P("=1/0")], [P("")], [P(field("=1/0", "** Expression is faulty **"), jc="right")]])
    d.add(table(rows, [3300, 3300, 2700]))
    d.p(
        R("References: "),
        field("REF 預期結果 \\h", "預期結果"),
        R(" · "),
        field("REF ExpectedResult \\h", "Expected Result"),
    )
    d.p(field("REF نتيجةمتوقعة \\h", "نتيجة متوقعة", rpr(rtl=True)), bidi=True)
    d.p(R(ZH[0] + " " + AR[0] + " " + sentence(d.r)))
    d.settings = "<w:evenAndOddHeaders/>"
    d.save(out, d.sect(title_pg=True, **hf))


def column_section_breaks(out):
    d = Doc("column-section-breaks", "Column section breaks")
    page = A6
    bdr = borders("pBdr", "4472C4", 6)
    d.p(R("Starting the document with a section column break!", rpr(b=True)), ppr=bdr)
    for _ in range(2):
        d.p(R("Two lovely columns. " + sentence(d.r, 10)))
    d.add(P(sect=d.sect(page=page, landscape=True, cols=2, kind="continuous", margins=(567, 567, 567, 567))))
    d.p(R("They balance if there is a continuous break. " + sentence(d.r, 8)), ppr=shd("E2EFDA"))
    d.add(P(sect=d.sect(page=page, landscape=True, cols=1, kind="continuous", margins=(567, 567, 567, 567))))
    for _ in range(3):
        d.p(R("Otherwise the first fills the page before the next column. " + sentence(d.r, 12)))
    d.p(page_break(), R("Any page break stops the balancing act."))
    d.add(P(sect=d.sect(page=page, landscape=True, cols=2, kind="continuous", margins=(567, 567, 567, 567), sep=True)))
    d.p(R(AR[2], rpr(rtl=True)), bidi=True, ppr=bdr)
    d.p(R(AR[3], rpr(rtl=True)), bidi=True)
    d.add(P(sect=d.sect(page=page, landscape=True, cols=2, kind="continuous", margins=(567, 567, 567, 567), bidi=True)))
    d.p(R("Single column, now that the columns are done. " + sentence(d.r, 8)))
    d.save(out, d.sect(page=page, landscape=True, kind="continuous", margins=(567, 567, 567, 567)))


def smartart_sections(out):
    d = Doc("smartart-sections", "SmartArt in two sections")
    logo = d.media(png("smartart-sections", 120, 40), "png")
    header = d.header(P(d.inline_picture(logo, 72, 24), style="Header", jc="right"))
    footer = d.footer(P(field("PAGE", "1"), style="Footer", jc="center"))
    hf = {"page": A4, "headers": [("default", header)], "footers": [("default", footer)]}
    nums = d.abstract_num([("decimal", "%1)", 360)])
    d.add(heading(1, "Process"))
    d.p(smartart(d, ["Collect", "Sort", "Measure", "Report"], 440, 120))
    for _k in range(4):
        d.p(sentence(d.r), num=(nums, 0))
    d.add(P(sect=d.sect(**hf)))
    d.add(heading(1, "Review"))
    d.p(smartart(d, ["Draft", "Check", "Publish"], 400, 140))
    d.p(prose(d.r, 3), ppr=borders("pBdr", "A5A5A5", 4, sides=("top",)))
    d.save(out, d.sect(kind="nextPage", **hf))


def japanese_vertical(out):
    d = Doc("japanese-vertical", "縦書きの文書")
    jp = rpr(extra='<w:rFonts w:hint="eastAsia"/><w:lang w:eastAsia="ja-JP"/>')
    for k in range(10):
        d.p(R("".join(JA[(k + n) % len(JA)] for n in range(3)), jp), ppr='<w:ind w:firstLine="220"/>')
    grid = '<w:docGrid w:type="lines" w:linePitch="360"/>'
    d.save(out, d.sect(page=A4, landscape=True, text_dir="tbRl", grid=grid, margins=(1701, 1418, 1701, 1418)))


def chinese_spec(out):
    d = Doc("chinese-spec", "测试文档")
    zh = rpr(extra='<w:rFonts w:hint="eastAsia"/><w:lang w:eastAsia="zh-CN"/>')
    pics = [d.media(png(f"chinese-spec-{k}", 160, 90), "png") for k in range(4)]
    header = d.header(P(field('STYLEREF "Heading 1" \\* MERGEFORMAT', "范围"), R("\t测试文档"), style="Header"))
    footer = d.footer(P(R("第 "), field("PAGE", "1"), R(" 页"), style="Footer", jc="center"))
    nums = d.abstract_num([("decimal", "%1", 432), ("decimal", "%1.%2", 576), ("decimal", "%1.%2.%3", 720)])
    d.p(R("测试文档", rpr(b=True, sz=44)), jc="center")
    d.p(R("文档属性：测试测试", zh), jc="center")
    rows = [[([P(R(h, zh))], shd("D9E2F3")) for h in ["日期", "修改人", "修改描述"]]]
    people = ["李四", "张三", "王五", "赵六"]
    for k in range(6):
        rows.append([[P(f"2024-0{k + 1}-1{k}")], [P(R(people[k % 4], zh))], [P(R(ZH[k % len(ZH)][:10], zh))]])
    d.p(R("修订记录", rpr(b=True)))
    d.add(table(rows, [2000, 2000, 5000]))
    titles = ["范围", "术语", "设计", "测试", "结论"]
    picture = 0
    for k, title in enumerate(titles):
        d.p(R(title, zh), style="Heading1", num=(nums, 0))
        for j in range(3):
            d.p(R(f"{title}{j + 1}", zh), style="Heading2", num=(nums, 1))
            d.p(R(ZH[(k + j) % len(ZH)] + ZH[(k + j + 1) % len(ZH)], zh))
            if j == 1 and picture < 4:
                d.p(d.vml_picture(pics[picture], 120, 68), jc="center")
                picture += 1
        if k == 1:
            rows2 = [[[P("ID")], [P(R("名称", zh))]], [[P("1")], [P(R("模块", zh))]], [[P("2")], [P(R("接口", zh))]]]
            pr = (
                '<w:tblpPr w:leftFromText="180" w:rightFromText="180" w:vertAnchor="text" w:horzAnchor="margin" '
                'w:tblpXSpec="right" w:tblpY="1"/>'
            )
            d.add(table(rows2, [800, 1600], tblpr=pr))
        if k in (2, 3, 4):
            d.add(table([[[P("A")], [P("B")]], [[P(R(ZH[k][:6], zh))], [P(str(k))]]], [3000, 3000]))
    d.p(R("English summary: " + sentence(d.r)), bidi=False)
    grid = '<w:docGrid w:type="lines" w:linePitch="312"/>'
    d.save(
        out,
        d.sect(
            page=(12242, 15842),
            headers=[("default", header)],
            footers=[("default", footer)],
            grid=grid,
            pg_num='w:start="1"',
        ),
    )


def drop_cap_dashes(out):
    d = Doc("drop-cap-dashes", "Drop caps that are dashes")
    frame = (
        '<w:keepNext/><w:framePr w:dropCap="drop" w:lines="3" w:wrap="around" w:vAnchor="text" '
        'w:hAnchor="text"/><w:spacing w:after="0" w:line="798" w:lineRule="exact"/>'
    )
    for ch, label in [("-", "dash"), ("–", "en-dash"), ("—", "em-dash"), ("‒", "figure dash")]:
        d.p(R(ch, rpr(extra='<w:position w:val="-11"/><w:sz w:val="96"/>')), ppr=frame)
        d.p(f"({label})" + prose(d.r, 6))
    d.save(out, d.sect(margins=(1134, 1134, 1134, 1134)))


def footnotes_per_page(out):
    d = Doc("footnotes-per-page", "A footnote on every page")
    for k in range(6):
        runs = [R(f"This is page {k + 1}."), d.note("footnote", f"Footnote for page {k + 1}.")]
        d.p(*runs)
        if k % 2:
            d.p(R(AR[k % len(AR)], rpr(rtl=True)), d.note("footnote", AR[(k + 1) % len(AR)]), bidi=True)
        if k < 5:
            d.p(page_break())
    footnote_pr = '<w:footnotePr><w:numRestart w:val="eachPage"/></w:footnotePr>'
    d.save(out, d.sect(page=A4, pg_num='w:start="1"').replace("<w:pgSz", footnote_pr + "<w:pgSz"))


def page_x_of_y(out):
    d = Doc("page-x-of-y", "Page X of Y")
    footer = d.footer(P(R("Page "), field("PAGE", "1"), R(" of "), field("NUMPAGES", "2"), style="Footer", jc="center"))
    first_hdr = d.header(P("", style="Header"))
    nums = d.abstract_num([("decimal", "%1.", 360)])
    d.add(*toc([(1, "Introduction", "_Toc1001", 1), (1, "Second section", "_Toc1002", 2)]))
    d.add(heading(1, "Introduction", "_Toc1001", 1))
    d.p(R("Overview", rpr(caps=True, b=True)))
    for _k in range(3):
        d.p(sentence(d.r), num=(nums, 0))
    d.add(P(sect=d.sect(footers=[("default", footer)], headers=[("default", first_hdr)])))
    d.add(heading(1, "Second section", "_Toc1002", 2))
    d.p(R("Details", rpr(caps=True)))
    d.p(prose(d.r, 5))
    d.save(out, d.sect(kind="nextPage", footers=[("default", footer)], headers=[("default", first_hdr)]))


def tracked_table_columns(out):
    d = Doc("tracked-table-columns", "Inserted and deleted empty table columns")
    ins_mark = f'<w:rPr><w:ins w:id="900" w:author="Reviewer One" w:date="{STAMP}"/></w:rPr>'
    del_mark = f'<w:rPr><w:del w:id="901" w:author="Reviewer Two" w:date="{STAMP}"/></w:rPr>'
    d.p(R("X", rpr(b=True)))

    def tracked_cell(width, kind):
        rid = 950 + width
        tag = "cellIns" if kind == "ins" else "cellDel"
        mark = ins_mark if kind == "ins" else del_mark
        return (
            f'<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/><w:{tag} w:id="{rid}" w:author="Reviewer '
            f'{"One" if kind == "ins" else "Two"}" w:date="{STAMP}"/></w:tcPr><w:p><w:pPr>{mark}</w:pPr>'
            "</w:p></w:tc>"
        )

    widths = [1500, 900, 900, 1500, 900, 900, 1500]
    rows = []
    for label in ["A", "B", "C", "D"]:
        rows.append(
            [
                [P(label)],
                tracked_cell(widths[1], "ins"),
                tracked_cell(widths[2], "ins"),
                [P(R("‍" + label.lower()))],
                tracked_cell(widths[4], "del"),
                tracked_cell(widths[5], "del"),
                ([P(tracked("ins", 960, "new"))], shd("FFF2CC")),
            ]
        )
    d.add(table(rows, widths))
    d.p(R(AR[4], rpr(rtl=True)), bidi=True)
    d.p(R("Status: "), sdt_dropdown("Status", ["Draft", "Final"], "Draft"))
    d.p(tracked("del", 970, "A deleted sentence. "), tracked("ins", 971, "An inserted sentence."))
    d.save(out, d.sect(page=A4))


def long_toc_form(out):
    d = Doc("long-toc-form", "Patienteninformation")
    placeholder = sdt_text("Kopf", placeholder="[Hier eingeben]")
    header = d.header(
        P(
            placeholder,
            R("\t"),
            sdt_text("Kopf2", placeholder="[Hier eingeben]"),
            R("\t"),
            sdt_text("Kopf3", placeholder="[Hier eingeben]"),
            style="Header",
        )
    )
    footer = d.footer(
        P(R("Seite "), field("PAGE", "1"), R(" von "), field("NUMPAGES", "3"), style="Footer", jc="right")
    )
    nums = d.abstract_num([("decimal", "%1.", 360), ("decimal", "%1.%2", 720)])
    d.p(R("Patienteninformation", rpr(caps=True, b=True, sz=32)), jc="center")
    d.p(
        R(
            "Informationsblatt und Einwilligungserklärung zur Teilnahme an einer klinischen Studie",
            rpr(caps=True, sz=22),
        ),
        jc="center",
        ppr=borders("pBdr", "000000", 8, sides=("bottom",)),
    )
    d.p(
        d.text_box([P(R("Studiencode", rpr(b=True))), P("xx-0000")], 120, 40, 360, 0, fill="F2F2F2", line="7F7F7F"),
        d.text_box([P(R("Version", rpr(b=True))), P("1.0")], 80, 40, 0, 0, fill="F2F2F2", line="7F7F7F"),
        R("_" * 70),
    )
    sections = [
        "Organisation",
        "Ziel der Studie",
        "Ablauf",
        "Risiken",
        "Nutzen",
        "Datenschutz",
        "Versicherung",
        "Kosten",
        "Rücktritt",
        "Kontakt",
    ]
    entries = []
    for k, title in enumerate(sections):
        entries.append((1, f"{k + 1}. {title}".upper(), f"_Toc2{k:03d}", 2 + k // 5))
        for j in range(3):
            entries.append((2, f"{k + 1}.{j + 1} Abschnitt", f"_Toc3{k:02d}{j}", 2 + k // 5))
    d.add(*toc(entries, heading="INHALT"))
    d.p(page_break())
    bid = 300
    for k, title in enumerate(sections):
        d.add(P(bookmark(bid, f"_Toc2{k:03d}", R(title, rpr(caps=True))), style="Heading1", num=(nums, 0)))
        bid += 1
        for j in range(3):
            d.add(P(bookmark(bid, f"_Toc3{k:02d}{j}", R("Abschnitt")), style="Heading2", num=(nums, 1)))
            bid += 1
            d.p(DE[(k + j) % len(DE)] + " " + DE[(k + j + 2) % len(DE)], ppr=shd("F2F2F2") if j == 1 else "")
    d.save(out, d.sect(page=A4, headers=[("default", header)], footers=[("default", footer)]))


def toc_styleref(out):
    d = Doc("toc-styleref", "A running head from STYLEREF")
    header = d.header(
        P(field('STYLEREF "Heading 1" \\* MERGEFORMAT', "Chapter"), R("\t\t"), field("PAGE", "2"), style="Header")
    )
    first = d.header(P(R("Title page", rpr(i=True)), style="Header"))
    header2 = d.header(P(R("Part two: "), field('STYLEREF "Heading 1"', "Chapter"), style="Header"))
    first2 = d.header(P("", style="Header"))
    footer = d.footer(P(field("PAGE", "1"), style="Footer", jc="center"))
    chapters = ["Steps behind him", "The street", "The money", "Morning"]
    d.p(R("He heard quiet steps", rpr(b=True, sz=44)), jc="center", ppr='<w:spacing w:before="2000"/>')
    d.add(*toc([(1, c, f"_Toc4{k:03d}", 2 + k // 2) for k, c in enumerate(chapters)]))
    d.p(page_break())
    for k, c in enumerate(chapters):
        d.add(heading(1, c, f"_Toc4{k:03d}", 400 + k))
        for _ in range(5):
            d.p(prose(d.r, 5))
        if k == 1:
            d.add(
                P(
                    sect=d.sect(
                        page=A4,
                        headers=[("first", first), ("default", header)],
                        footers=[("default", footer)],
                        title_pg=True,
                    )
                )
            )
    d.save(
        out,
        d.sect(
            page=A4,
            kind="nextPage",
            headers=[("first", first2), ("default", header2)],
            footers=[("default", footer)],
            title_pg=True,
        ),
    )


def full_width_line(out):
    d = Doc("full-width-line", "Una línea a todo el ancho")
    d.styles.append(
        (
            "paragraph",
            "Estilo",
            "Estilo",
            "Normal",
            '<w:widowControl w:val="0"/><w:ind w:left="720" w:hanging="720"/>',
            '<w:sz w:val="24"/><w:szCs w:val="24"/>',
        )
    )
    # A strip down the page's left edge, anchored in the header and wrapped through: it reaches into the text area,
    # so the line beside it is narrower still than the rule after the title.
    strip = d.media(png("full-width-line", 40, 400), "png")
    edge = P(d.anchored_picture(strip, 83.85, 848.2, -74.85, -36, wrap="through"), style="Header")
    headers = [("even", d.header(P("", style="Header"))), ("default", d.header(edge)), ("first", d.header(edge))]
    tabs = "<w:tabs>" + "".join(f'<w:tab w:val="left" w:pos="{p}"/>' for p in range(0, 9361, 720)) + "</w:tabs>"
    width, height = 6053328, 18288  # 476.65 pt by 1.44 pt: wider than the text area of 425.2 pt
    line = (
        '<w:r><w:rPr><w:noProof/></w:rPr><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>'
        f'<wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{width}" cy="{height}"/>'
        '<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Rectangle 1"/><wp:cNvGraphicFramePr/>'
        '<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">'
        f'<wps:wsp><wps:cNvSpPr/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{width}" cy="{height}"/></a:xfrm>'
        '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="000000"/></a:solidFill>'
        '<a:ln w="0"><a:noFill/></a:ln></wps:spPr><wps:bodyPr/></wps:wsp></a:graphicData></a:graphic></wp:inline>'
        '</w:drawing></mc:Choice><mc:Fallback><w:pict><v:rect id="Rectangle 1" style="width:476.65pt;height:1.45pt;'
        "visibility:visible;mso-wrap-style:square;mso-position-horizontal-relative:char;"
        'mso-position-vertical-relative:line" fillcolor="black" stroked="f" strokeweight="0"><w10:anchorlock/>'
        "</v:rect></w:pict></mc:Fallback></mc:AlternateContent></w:r>"
    )
    ppr = tabs + '<w:spacing w:line="0" w:lineRule="atLeast"/><w:ind w:left="0" w:firstLine="0"/>'
    d.p(R(ES[0], rpr(b=True)), line, style="Estilo", ppr=ppr)
    d.p("", style="Estilo", ppr=tabs + '<w:ind w:left="0" w:firstLine="0"/>', jc="both")
    d.p(
        R("Concierto en la plaza", rpr(sz=44)),
        style="Estilo",
        jc="center",
        ppr=tabs + '<w:ind w:left="0" w:firstLine="0"/>',
    )
    nums = d.abstract_num([("bullet", "•", 720)])
    for s in ES:
        d.p(R(s), style="Estilo", num=(nums, 0))
    link = d.rel("hyperlink", "https://example.org/agenda", external=True)
    rows = [[[P(R("Hora", rpr(b=True)))], [P(R("Lugar", rpr(b=True)))]], [[P("20:00")], [P("Plaza Mayor")]]]
    pr = (
        '<w:tblpPr w:leftFromText="141" w:rightFromText="141" w:vertAnchor="text" w:horzAnchor="page" '
        'w:tblpX="2352" w:tblpY="335"/>'
    )
    d.add(table(rows, [4000, 4008], tblpr=pr))
    d.p(
        R("Más información: "),
        f'<w:hyperlink r:id="{link}">{R("example.org/agenda", rpr(style="Hyperlink"))}</w:hyperlink>',
        R(" " + ES[1].upper(), rpr(caps=True)),
    )
    d.save(out, d.sect(page=A4, margins=(1417, 1701, 1276, 1701), headers=headers))


def legacy_form_fields(out):
    d = Doc("legacy-form-fields", "Software schedule")
    header = d.header(P(R("SOFTWARE SCHEDULE", rpr(caps=True, b=True)), style="Header", jc="center"))
    footer = d.footer(P(R("Page "), field("PAGE", "1"), style="Footer", jc="center"))
    clauses = d.abstract_num(
        [("decimal", "%1.", 360, shd("D9D9D9")), ("decimal", "%1.%2", 792), ("lowerLetter", "(%3)", 1224)]
    )
    d.p(
        R("Software Schedule #", rpr(b=True, caps=True, sz=32)),
        form_field("FORMTEXT", "Number", "[NUMBER]"),
        jc="center",
    )
    d.p(
        R("to the agreement between "),
        form_field("FORMTEXT", "Parties", "[parties]"),
        R(" with an Agreement Effective Date of "),
        bookmark(1, "SSEDate", form_field("FORMTEXT", "Date", "[Agreement Effective Date]")),
        R(' ("Agreement").'),
    )
    d.p(field('ASK Schedule-Effective-Date "Schedule Effective Date?" \\d DATE \\* MERGEFORMAT', ""))
    d.p(R("Schedule number: "), bookmark(2, "SSNumber", R("S-0001")))
    d.p(
        R("Accepted: "),
        form_field("FORMCHECKBOX", "Accept", "1"),
        R("  Region: "),
        form_field(
            "FORMDROPDOWN",
            "Region",
            "North",
            '<w:ddList><w:result w:val="0"/><w:listEntry w:val="North"/><w:listEntry w:val="South"/></w:ddList>',
        ),
    )
    d.p(
        R("BETWEEN PARTY AND " + "_" * 40, rpr(caps=True)),
        ppr='<w:framePr w:w="4000" w:hSpace="180" w:wrap="around" w:vAnchor="text" w:hAnchor="margin" '
        'w:xAlign="right" w:y="1"/>' + borders("pBdr", "000000", 4) + shd("F2F2F2"),
    )
    comments = [
        d.comment("Check the effective date."),
        d.comment("Region list to be confirmed."),
        d.comment("Number format agreed.", author="Reviewer Two"),
    ]
    r = d.r
    c = 0
    for k in range(9):
        d.p(
            R(
                ["Definitions", "Licence", "Delivery", "Fees", "Support", "Term", "Warranty", "Liability", "General"][
                    k
                ],
                rpr(b=True, caps=True),
            ),
            num=(clauses, 0),
            ppr=shd("D9D9D9"),
        )
        for j in range(4):
            runs = [R(sentence(r, 14) + " ")]
            if j == 1:
                runs += [
                    R("As of "),
                    field("REF SSEDate \\h \\* MERGEFORMAT", "[Agreement Effective Date]"),
                    R(", schedule "),
                    field("REF SSNumber \\h", "S-0001"),
                    R(" applies. "),
                ]
            if j == 2 and c < len(comments):
                runs = (
                    [f'<w:commentRangeStart w:id="{comments[c]}"/>']
                    + runs
                    + [
                        f'<w:commentRangeEnd w:id="{comments[c]}"/>',
                        f'<w:r>{rpr(style="CommentReference")}<w:commentReference w:id="{comments[c]}"/></w:r>',
                    ]
                )
                c += 1
            d.p(*runs, R(prose(r, 3)), num=(clauses, 1), ppr=shd("F2F2F2") if j == 3 else "")
            if j == 3:
                for _n in range(3):
                    d.p(R(sentence(r, 10)), num=(clauses, 2))
    d.save(out, d.sect(page=A4, headers=[("default", header)], footers=[("default", footer)]))


def case_study_pictures(out):
    d = Doc("case-study-pictures", "Customer solution case study")
    d.part("word/media/image1.jpeg", jpeg("case-study-small", 320, 200, 97, 0))
    d.part("word/media/image2.jpeg", jpeg("case-study-large", 888, 552, 94, 0))
    d.part("word/media/image3.png", png("case-study-chart", 320, 180))
    d.part("word/media/image4.png", png("case-study-logo", 200, 200))
    d.defaults.update({"jpeg": "image/jpeg", "png": "image/png"})
    small, large, chart_png, logo = (
        d.rel("image", f"media/{n}") for n in ["image1.jpeg", "image2.jpeg", "image3.png", "image4.png"]
    )
    header = d.header(P(d.inline_picture(logo, 36, 36), R("\tCustomer Solution Case Study"), style="Header"))
    header2 = d.header(P("Case study", style="Header", jc="right"))
    footer = d.footer(P(R("Page "), field("PAGE", "1"), R(" of "), field("NUMPAGES", "5"), style="Footer", jc="center"))
    d.p(R("Race Cars and Video", rpr(b=True, sz=48, color="1F4E79")))
    d.p(d.inline_picture(large, 444, 276))
    patt = (
        '<a:pattFill prst="ltUpDiag"><a:fgClr><a:srgbClr val="BDD7EE"/></a:fgClr><a:bgClr>'
        '<a:srgbClr val="FFFFFF"/></a:bgClr></a:pattFill>'
    )
    box = d.text_box(
        [P(R("Overview", rpr(b=True))), P("Country or Region: Australia"), P("Industry: Digital video design")],
        170,
        110,
        300,
        10,
        fill="FFFFFF",
    )
    d.p(box.replace('<a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill>', patt, 1), R(prose(d.r, 5)))
    d.p(
        d.vml_text_box(
            [P(R("Customer Profile", rpr(b=True))), P(sentence(d.r, 14))],
            180,
            80,
            0,
            0,
            extra='<v:fill type="pattern" color2="#DDEBF7"/>',
        )
    )
    for _ in range(4):
        d.p(prose(d.r, 5))
    d.add(P(sect=d.sect(headers=[("default", header)], footers=[("default", footer)])))
    d.add(heading(1, "Situation"))
    for k in range(6):
        d.p(prose(d.r, 5))
        if k == 1:
            d.p(d.inline_picture(small, 160, 100))
        if k == 3:
            d.p(
                R("“" + sentence(d.r, 12) + "”", rpr(i=True, sz=28, color="2E74B5")),
                ppr='<w:framePr w:w="3000" w:hSpace="180" w:wrap="around" w:vAnchor="text" w:hAnchor="margin" '
                'w:xAlign="center" w:y="1"/>' + borders("pBdr", "2E74B5", 8, sides=("top", "bottom")),
            )
    d.p(column_break(), R(prose(d.r, 3)))
    d.p(d.inline_picture(chart_png, 160, 90))
    for _t in range(6):
        rows = [[([P(R("Item", rpr(b=True)))], shd("DEEAF6")), ([P(R("Value", rpr(b=True)))], shd("DEEAF6"))]]
        rows += [[[P(f"Measure {k + 1}")], [P(str(d.r.randrange(10, 99)), jc="right")]] for k in range(3)]
        d.add(table(rows, [2400, 1600]))
        d.p(sentence(d.r, 12))
    d.save(
        out,
        d.sect(
            kind="nextPage", cols=2, headers=[("default", header2)], footers=[("default", footer)], page=(12242, 15842)
        ),
    )


def arabic_lists(out):
    d = Doc("arabic-lists", "قائمة بالعربية")
    nums = d.abstract_num([("decimal", "%1-", 720, "<w:bidi/>"), ("arabicAlpha", "%2.", 1080, "<w:bidi/>")])
    ar = rpr(rtl=True, font="Arial")
    for k in range(12):
        runs = [R(AR[k % len(AR)] + " ", ar)]
        if k % 3 == 0:
            runs += [R("(Reboot) ", rpr(font="Arial")), R(AR[(k + 2) % len(AR)], ar)]
        if k % 4 == 1:
            runs += [R(f" {k + 10}%", rpr(font="Arial")), R(" " + AR[(k + 1) % len(AR)], ar)]
        d.p(*runs, bidi=True, num=(nums, k % 2), jc="left")
    d.save(out, d.sect(page=A4, bidi=True))


def long_footnotes(out):
    d = Doc("long-footnotes", "An essay with long footnotes")
    nums = d.abstract_num([("upperRoman", "%1", 360), ("decimal", "%2.", 720)])
    r = d.r
    d.p(R("I", rpr(b=True)), num=None, jc="center")
    d.p(R("The definition of the term", rpr(b=True)), num=(nums, 1))
    for k in range(8):
        note_paras = [prose(r, 6 if k % 3 == 0 else 2) for _ in range(3 if k % 3 == 0 else 1)]
        d.p(
            R(prose(r, 4) + " "),
            d.note("footnote", *note_paras),
            R(" " + prose(r, 3)),
            ppr=shd("F2F2F2") if k % 4 == 2 else "",
        )
        if k == 3:
            d.p(R("Second part", rpr(b=True)), num=(nums, 1))
    d.save(out, d.sect(page=A4))


def multilevel_numbering(out):
    d = Doc("multilevel-numbering", "Multilevel numbering")
    header = d.header(P(R("NUMBERING", rpr(caps=True)), style="Header", jc="right"))
    footer = d.footer(P(field("PAGE", "1"), style="Footer", jc="center"))
    first = d.footer(P("", style="Footer"))
    legal = d.abstract_num([("decimal", "%1.", 360), ("decimal", "%1.%2.", 792), ("decimal", "%1.%2.%3.", 1224)])
    letters = d.abstract_num([("upperLetter", "%1", 360)])
    for a in range(2):
        d.p(
            R(f"Level one {a + 1}", rpr(b=True, caps=True)),
            num=(legal, 0),
            ppr=borders("pBdr", "4472C4", 6, sides=("bottom",)),
        )
        for b in range(2):
            d.p(R(f"Level two {b + 1}"), num=(legal, 1))
            for c in range(3):
                d.p(R(f"Level three {c + 1}: " + sentence(d.r, 6), rpr(small_caps=c == 1)), num=(legal, 2))
    for _k in range(3):
        d.p(sentence(d.r, 8), num=(letters, 0))
    d.save(
        out,
        d.sect(page=A4, headers=[("default", header)], footers=[("default", footer), ("first", first)], title_pg=True),
    )


def relative_width_lines(out):
    d = Doc("relative-width-lines", "Horizontal lines of relative width")
    page = (11907, 8391)
    hf = {
        "headers": [
            ("default", d.header(P("Lines", style="Header"))),
            ("first", d.header(P("", style="Header"))),
            ("even", d.header(P("Even", style="Header"))),
        ],
        "footers": [
            ("default", d.footer(P("", style="Footer"))),
            ("first", d.footer(P("", style="Footer"))),
            ("even", d.footer(P("", style="Footer"))),
        ],
    }
    d.p("100% of column")
    d.p(hr(100))
    d.p("50% centered")
    d.p(hr(50, "center", "2E74B5"))
    d.p("25% right-aligned while the text is left aligned and above the line")
    d.p(hr(25, "right", "C00000"))
    d.p("10% left-aligned while the paragraph text is right aligned and below the line", jc="right")
    d.p(hr(10, "left", "70AD47"), jc="right")
    d.add(P(sect=d.sect(page=page, **hf)))
    d.p("100% of custom sized column")
    d.p(hr(100, color="7030A0"))
    d.p(prose(d.r, 3))
    d.add(P(sect=d.sect(page=page, cols=2, col_space=1440, kind="continuous", **hf)))
    d.add(table([[[P("75% inside table"), P(hr(75))], [P(sentence(d.r, 6))]]], [5000, 3000]))
    d.add(P(sect=d.sect(page=page, kind="nextPage", **hf)))
    d.p(d.vml_text_box([P("Textbox with horizontal line"), P(hr(80, color="000000"))], 240, 80, 0, 0, fill="FFFFFF"))
    d.p(prose(d.r, 2))
    d.save(out, d.sect(page=page, kind="continuous", **hf))


def theme_fonts(out):
    d = Doc("theme-fonts", "Theme fonts")
    d.styles.append(
        (
            "paragraph",
            "Custom1",
            "Custom",
            "Normal",
            '<w:spacing w:before="120"/>',
            '<w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi"/><w:color w:val="C45911" '
            'w:themeColor="accent2" w:themeShade="BF"/><w:sz w:val="28"/>',
        )
    )
    d.p(R("汉语漢語", rpr(extra='<w:rFonts w:hint="eastAsia"/>')))
    d.p(R(HE[0], rpr(rtl=True, extra='<w:rFonts w:hint="cs"/>')), bidi=True)
    d.p("Default style theme font")
    d.p(R("Direct format font", rpr(font="Arial Black")))
    d.p(R("Major theme font", rpr(extra='<w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi"/>')))
    d.p(
        "Paragraph shadow",
        ppr='<w:shd w:val="thinHorzStripe" w:color="7B4A08" w:themeColor="accent1" '
        'w:themeShade="80" w:fill="F9D0C0" w:themeFill="text2" w:themeFillTint="33"/>',
    )
    d.p("Custom style", style="Custom1")
    d.save(out, d.sect())


def toc_field_f(out):
    d = Doc("toc-field-f", "A table of contents from TC fields")
    entries = [(1, "Heading 1", "_Toc5001", 2), (2, "Heading2", "_Toc5002", 2), (3, "Heading3", "_Toc5003", 2)]
    paras = toc(entries, switches='\\o "1-3" \\h \\z \\u \\f', heading=None)
    paras[-1] = paras[-1].replace("</w:p>", page_break() + "</w:p>")
    d.add(*paras)
    for k, (level, text, mark, _) in enumerate(entries):
        d.add(heading(level, text, mark, 600 + k))
        d.p(R(sentence(d.r)), field(f'TC "Entry {k + 1}" \\f \\l {level}', ""))
    d.save(out, d.sect(page=A4))


# The manifest's order: (builder, the LibreOffice document it replaces).
DOCUMENTS = [
    (letterhead_sections, "090716_Studentische_Arbeit_VWS"),
    (table_vertical_text, "bnc865381"),
    (endnote_continuation, "endnote-cont-separator"),
    (equation_captions, "EquationWithAboveAndBelowCaption"),
    (smartart_pictures, "fdo66474"),
    (bidi_worksheet, "FDO76312"),
    (content_control_form, "FDO78292"),
    (page_background_gradient, "fill"),
    (floating_table_math, "floatingtbl_with_formula"),
    (tracked_changes_sections, "n830205"),
    (strict_ooxml, "strict"),
    (math_paper_damaged_table, "tableCurrupt"),
    (floating_tables_columns, "tdf105688"),
    (citations_comments, "tdf106724"),
    (chart_ole_textbox, "tdf124398"),
    (toc_section_margins, "tdf124637_sectionMargin"),
    (tracked_table_cells, "tdf127814"),
    (table_formulas_unicode, "tdf133647_unicode"),
    (column_section_breaks, "tdf135343_columnSectionBreak_c14"),
    (smartart_sections, "tdf135906"),
    (japanese_vertical, "tdf142407"),
    (chinese_spec, "tdf149089"),
    (drop_cap_dashes, "tdf150200"),
    (footnotes_per_page, "tdf152203"),
    (page_x_of_y, "tdf155736_PageNumbers_footer"),
    (tracked_table_columns, "tdf157011_ins_del_empty_cols"),
    (long_toc_form, "tdf164905"),
    (toc_styleref, "tdf166850"),
    (full_width_line, "tdf38575_fullWidthLine"),
    (legacy_form_fields, "tdf61000"),
    (case_study_pictures, "tdf81345"),
    (arabic_lists, "tdf87533_bidi"),
    (long_footnotes, "tdf90810short"),
    (multilevel_numbering, "tdf95848"),
    (relative_width_lines, "tdf97648_relativeWidth"),
    (theme_fonts, "theme-preservation"),
    (toc_field_f, "TOC_field_f"),
]


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.dirname(os.path.abspath(__file__))
    os.makedirs(out, exist_ok=True)
    for build, _ in DOCUMENTS:
        build(out)
        name = build.__name__.replace("_", "-")
        with open(os.path.join(out, f"{name}.docx"), "rb") as f:
            print(f"{name}.docx\t{hashlib.sha256(f.read()).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
