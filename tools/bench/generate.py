"""The bench's documents: DOCX only, each a fixed, seeded file.

    python generate.py OUT [--features DIR] [--only NAME,...] [--hostile] [--heavy]

Writes OUT/<class>/<name>.docx and OUT/manifest.tsv. The same arguments give the same bytes: images come from a
seeded generator and the ZIP entries carry a fixed date. Classes, each varying one thing:

- report  what installations send: text reports with tables (2, 5, 20 pages), and the same
          with 30, 45 or 60 pictures (10, 12, 15, 20 MB); the workload the sizing answers are for
- text    text only, by pages (1, 10, 100, 500)
- tables  tables of 20 rows by 6 columns, by count (10, 100, 1000)
- px      one flat PNG, by decoded megapixels (4 to 256): many pixels in few bytes
- bytes   16-megapixel noise JPEGs, by file size (1 to 150 MB): the most bytes per pixel
- mix     photo-like JPEGs of 2-12 megapixels placed at 150-300 DPI, by file size (5 to 150 MB)
- objects 36 of the corpus's 37 feature documents (charts, SmartArt, Office Math, tables, sections, fields,
          notes, tracked changes, right-to-left and vertical text, ...), copied from
          tests/corpus/documents/features/, each with a rendering approved by eye; not full-width-line, which
          hangs moalhaddar/docx-to-pdf
- hostile (with --hostile) a 1-gigapixel PNG and full-width-line, the feature document that hangs
          moalhaddar/docx-to-pdf; never in throughput runs
- heavy   (with --heavy; several GB) the `bytes` and `mix` classes from 300 MB to 1.1 GB, `text` to
          250 000 pages, and the content bombs: `tables` to 10 000, `px` to 4 gigapixels and eight 1-gigapixel PNGs
          in one document, a 512-megapixel RGB PNG, `pages` (one paragraph repeated for 500 000 pages in under 2 MB);
          the streamed ones are written by heavy.py; never in throughput runs
"""

import argparse
import hashlib
import io
import os
import shutil
import sys
import zipfile
from datetime import UTC, datetime

import heavy
import numpy as np
from docx import Document
from docx.shared import Inches
from PIL import Image

FIXED_DATE = datetime(2026, 1, 1, tzinfo=UTC)
ZIP_DATE = (2026, 1, 1, 0, 0, 0)
PAGE_WIDTH_IN = 6.5
WORDS = (
    "the of and to in report patient result value test sample analysis normal range date clinic laboratory "
    "method reference unit measured within above below level control protocol observation summary"
).split()


def rng(name):
    seed = int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "little")
    return np.random.Generator(np.random.PCG64(seed))


def new_document():
    doc = Document()
    props = doc.core_properties
    props.created = props.modified = FIXED_DATE
    props.author = props.last_modified_by = "docx-to-pdf bench"
    props.revision = 1
    return doc


def save(doc, path):
    """Saves `doc` with every ZIP entry dated ZIP_DATE, so the bytes depend on the content only."""
    buffer = io.BytesIO()
    doc.save(buffer)
    buffer.seek(0)
    with zipfile.ZipFile(buffer) as src, zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as dst:
        for item in src.infolist():
            info = zipfile.ZipInfo(item.filename, ZIP_DATE)
            info.compress_type = zipfile.ZIP_STORED if item.filename.startswith("word/media/") else zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            dst.writestr(info, src.read(item.filename))


def sentence(r, words):
    return " ".join(WORDS[i] for i in r.integers(0, len(WORDS), words)).capitalize() + "."


def text(name, pages):
    r = rng(name)
    doc = new_document()
    doc.add_heading(f"Text, {pages} pages", 1)
    # About 450 words a page at the default style: 5 paragraphs of 90 words.
    for _ in range(pages * 5):
        doc.add_paragraph(" ".join(sentence(r, 15) for _ in range(6)))
    return doc, {"pages_hint": pages}


def tables(name, count):
    r = rng(name)
    doc = new_document()
    doc.add_heading(f"Tables, {count}", 1)
    for t in range(count):
        doc.add_paragraph(f"Table {t + 1}. " + sentence(r, 8))
        table = doc.add_table(rows=20, cols=6)
        table.style = "Table Grid"
        values = r.integers(0, 100000, (20, 6))
        for i, row in enumerate(table.rows):
            for j, cell in enumerate(row.cells):
                cell.text = f"{values[i, j] / 100:.2f}" if i else f"Column {j + 1}"
    return doc, {"tables": count}


def picture(doc, data, px_width, dpi):
    width_in = min(px_width / dpi, PAGE_WIDTH_IN)
    doc.add_picture(io.BytesIO(data), width=Inches(width_in))
    return px_width / width_in


def flat_png(side, mode="RGB"):
    """A flat image with a frame and a diagonal, so it is not blank, in very few bytes."""
    image = Image.new(mode, (side, side), "white" if mode == "RGB" else 255)
    pixels = image.load()
    for i in range(0, side, max(1, side // 4096)):
        for p in ((i, 0), (i, side - 1), (0, i), (side - 1, i), (i, i)):
            pixels[p] = (0, 0, 0) if mode == "RGB" else 0
    out = io.BytesIO()
    image.save(out, "PNG", optimize=False, compress_level=9)
    return out.getvalue()


def px(name, mpx, mode="RGB"):
    side = int(round((mpx * 1_000_000) ** 0.5))
    doc = new_document()
    doc.add_heading(f"One flat PNG, {mpx} megapixels", 1)
    dpi = picture(doc, flat_png(side, mode), side, 300)
    return doc, {"images": 1, "decoded_mpx": side * side / 1e6, "placed_dpi": round(dpi)}


def jpeg(array, quality):
    out = io.BytesIO()
    Image.fromarray(array, "RGB").save(out, "JPEG", quality=quality)
    return out.getvalue()


def noise(r, w, h):
    return r.integers(0, 256, (h, w, 3), dtype=np.uint8)


def photo_like(r, w, h):
    """Smooth random colour fields with fine grain: about the bytes per pixel of a photograph at quality 85."""
    small = r.integers(0, 256, (max(2, h // 64), max(2, w // 64), 3), dtype=np.uint8)
    base = np.asarray(Image.fromarray(small, "RGB").resize((w, h), Image.BICUBIC), dtype=np.int16)
    grain = r.normal(0, 12, (h, w, 3)).astype(np.int16)
    return np.clip(base + grain, 0, 255).astype(np.uint8)


def bytes_class(name, mb):
    """16-megapixel noise JPEGs: one at the quality that hits `mb`, or as many at quality 90 as reach it."""
    r = rng(name)
    target = mb * 1_000_000
    side = 4000
    doc = new_document()
    doc.add_heading(f"Noise JPEGs, about {mb} MB", 1)
    images, total, dpis = 0, 0, []
    one = jpeg(noise(r, side, side), 90)
    if len(one) >= target:
        lo, hi = 1, 90
        while lo < hi:
            q = (lo + hi + 1) // 2
            if len(jpeg(noise(rng(f"{name}-q"), side, side), q)) <= target:
                lo = q
            else:
                hi = q - 1
        one = jpeg(noise(rng(f"{name}-q"), side, side), lo)
        dpis.append(picture(doc, one, side, 300))
        images, total = 1, len(one)
    else:
        while total < target:
            data = one if images == 0 else jpeg(noise(r, side, side), 90)
            dpis.append(picture(doc, data, side, 300))
            images += 1
            total += len(data)
    return doc, {"images": images, "decoded_mpx": images * side * side / 1e6, "placed_dpi": round(min(dpis))}


def mix(name, mb):
    r = rng(name)
    target = mb * 1_000_000
    doc = new_document()
    doc.add_heading(f"Photo-like JPEGs, about {mb} MB", 1)
    images, total, pixels, dpis = 0, 0, 0, []
    while total < target:
        mpx = r.uniform(2, 12)
        aspect = r.choice([4 / 3, 3 / 2, 16 / 9])
        w = int((mpx * 1e6 * aspect) ** 0.5)
        h = int(w / aspect)
        data = jpeg(photo_like(r, w, h), 85)
        doc.add_paragraph(f"Figure {images + 1}. " + sentence(r, 10))
        dpis.append(picture(doc, data, w, r.uniform(150, 300)))
        images += 1
        total += len(data)
        pixels += w * h
    return doc, {"images": images, "decoded_mpx": pixels / 1e6, "placed_dpi": round(min(dpis))}


def report(name, pages, images, mb=0):
    """What the installations send: mostly text reports with tables; in some, the same
    with 30-60 pictures, 10-15 MB in all. Pictures are photo-like JPEGs at quality 85 (about 0.28 bytes a pixel),
    sized at random around the average that makes `images` of them come to `mb`."""
    r = rng(name)
    doc = new_document()
    doc.add_heading(f"Report, {pages} pages of text" + (f", {images} pictures" if images else ""), 1)
    pixels, dpis = 0, []
    for page in range(pages):
        doc.add_heading(f"Section {page + 1}", 2)
        for _ in range(3):
            doc.add_paragraph(" ".join(sentence(r, 15) for _ in range(5)))
        table = doc.add_table(rows=8, cols=4)
        table.style = "Table Grid"
        for _i, row in enumerate(table.rows):
            for j, cell in enumerate(row.cells):
                cell.text = sentence(r, 2) if j == 0 else f"{r.integers(0, 100000) / 100:.2f}"
    for i in range(images):
        average = mb * 1e6 / images / 0.28e6
        mpx = r.uniform(0.6 * average, 1.4 * average)
        w = int((mpx * 1e6 * 4 / 3) ** 0.5)
        h = int(w * 3 / 4)
        doc.add_paragraph(f"Picture {i + 1}. " + sentence(r, 8))
        dpis.append(picture(doc, jpeg(photo_like(r, w, h), 85), w, r.uniform(150, 300)))
        pixels += w * h
    meta = {"pages_hint": pages + images // 2, "tables": pages}
    if images:
        meta.update(images=images, decoded_mpx=pixels / 1e6, placed_dpi=round(min(dpis)))
    return doc, meta


GENERATED = [
    *[("report", f"report-text-{n}p", lambda name, n: report(name, n, 0), n) for n in (2, 5, 20)],
    *[
        ("report", f"report-img{k}-{mb}mb", lambda name, km: report(name, 5, *km), (k, mb))
        for k, mb in ((30, 10), (45, 12), (60, 15), (60, 20))
    ],
    *[("text", f"text-{n}p", text, n) for n in (1, 10, 100, 500)],
    *[("tables", f"tables-{n}", tables, n) for n in (10, 100, 1000)],
    *[("px", f"px-{n}mpx", px, n) for n in (4, 16, 64, 256)],
    *[("bytes", f"bytes-{n}mb", bytes_class, n) for n in (1, 10, 50, 100, 150)],
    *[("mix", f"mix-{n}mb", mix, n) for n in (5, 20, 50, 100, 150)],
]
HOSTILE = [("hostile", "hostile-px-1024mpx", lambda name, n: px(name, n, "L"), 1024)]
# The feature document that hangs moalhaddar/docx-to-pdf's LibreOffice: hostile, so never in `objects`.
HOSTILE_FEATURES = ["full-width-line.docx"]


def feature_documents(directory):
    """The feature documents in `directory`, sorted, but the hostile ones."""
    return sorted(f for f in os.listdir(directory) if f.endswith(".docx") and f not in HOSTILE_FEATURES)


def feature_descriptions(directory):
    """What each feature document exercises, from the table in the directory's README (none if it has none)."""
    readme = os.path.join(directory, "README.md")
    if not os.path.exists(readme):
        return {}
    descriptions = {}
    with open(readme) as f:
        for line in f:
            cols = [c.strip() for c in line.strip().strip("|").split("|")]
            if len(cols) == 2 and cols[0].startswith("`") and cols[0].endswith("`"):
                descriptions[f"{cols[0].strip('`')}.docx"] = " ".join(cols[1].split())
    return descriptions


def streamed(write):
    """Marks a maker that writes its document itself (heavy.py): `write(name, level, path)` returns the meta."""
    write.streamed = True
    return write


@streamed
def heavy_text(name, pages, path):
    heavy.write_docx(path, heavy.text_body(name, pages), large=pages * 4000 > 2**31 - 1)
    return {"pages_hint": pages}


@streamed
def heavy_pages(name, pages, path):
    heavy.write_docx(path, heavy.pages_bomb_body(name, pages), large=pages * 4000 > 2**31 - 1)
    return {"pages_hint": pages}


@streamed
def heavy_tables(name, count, path):
    heavy.write_docx(path, heavy.tables_body(name, count))
    return {"tables": count}


@streamed
def heavy_px(name, spec, path):
    mpx, count, channels = (*spec, 1)[:3]
    images, body, meta = heavy.pixels(name, mpx, count, channels)
    heavy.write_docx(path, body, images)
    return meta


HEAVY = [
    *[("bytes", f"bytes-{n}mb", bytes_class, n) for n in (300, 500, 750, 1100)],
    *[("mix", f"mix-{n}mb", mix, n) for n in (300, 500, 750, 1100)],
    *[("text", f"text-{n}p", heavy_text, n) for n in (2000, 10000, 50000, 250000)],
    *[("tables", f"tables-{n}", heavy_tables, n) for n in (2000, 4000, 10000)],
    *[("px", f"px-{n}mpx", heavy_px, (n, 1)) for n in (2048, 4096)],
    ("px", "px-8x1024mpx", heavy_px, (1024, 8)),
    ("px", "px-rgb-512mpx", heavy_px, (512, 1, 3)),
    ("pages", "pages-500000p", heavy_pages, 500000),
]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("out")
    parser.add_argument(
        "--features", default="/repo/tests/corpus/documents/features", help="the corpus's feature documents"
    )
    parser.add_argument("--only", help="comma-separated names to (re)generate")
    parser.add_argument("--hostile", action="store_true")
    parser.add_argument("--heavy", action="store_true")
    args = parser.parse_args()
    only = set(args.only.split(",")) if args.only else None

    rows = {}
    manifest = os.path.join(args.out, "manifest.tsv")
    if os.path.exists(manifest):
        with open(manifest) as f:
            header = f.readline().rstrip("\n").split("\t")
            for line in f:
                row = dict(zip(header, line.rstrip("\n").split("\t"), strict=False))
                rows[row["name"]] = row

    def record(cls, name, path, meta):
        rows[name] = {
            "class": cls,
            "name": name,
            "path": os.path.relpath(path, args.out),
            "bytes": str(os.path.getsize(path)),
            "sha256": sha256(path),
            "pages_hint": str(meta.get("pages_hint", "")),
            "tables": str(meta.get("tables", "")),
            "images": str(meta.get("images", "")),
            "decoded_mpx": f"{meta['decoded_mpx']:.1f}" if "decoded_mpx" in meta else "",
            "placed_dpi": str(meta.get("placed_dpi", "")),
            "feature": meta.get("feature", ""),
        }
        print(f"{name}\t{rows[name]['bytes']} bytes", flush=True)

    for cls, name, make, level in GENERATED + (HOSTILE if args.hostile else []) + (HEAVY if args.heavy else []):
        if only and name not in only:
            continue
        os.makedirs(os.path.join(args.out, cls), exist_ok=True)
        path = os.path.join(args.out, cls, f"{name}.docx")
        if getattr(make, "streamed", False):
            meta = make(name, level, path)
        else:
            doc, meta = make(name, level)
            save(doc, path)
        record(cls, name, path, meta)

    features = feature_descriptions(args.features)
    copied = [("objects", feature_documents(args.features))] + ([("hostile", HOSTILE_FEATURES)] if args.hostile else [])
    for cls, names in copied:
        os.makedirs(os.path.join(args.out, cls), exist_ok=True)
        for file in names:
            name = f"{cls}-{file.removesuffix('.docx')}"
            if only and name not in only:
                continue
            path = os.path.join(args.out, cls, file)
            shutil.copyfile(os.path.join(args.features, file), path)
            record(cls, name, path, {"feature": features.get(file, "")})

    header = [
        "class",
        "name",
        "path",
        "bytes",
        "sha256",
        "pages_hint",
        "tables",
        "images",
        "decoded_mpx",
        "placed_dpi",
        "feature",
    ]
    with open(manifest, "w") as f:
        f.write("\t".join(header) + "\n")
        for name in sorted(rows, key=lambda n: (rows[n]["class"], n)):
            f.write("\t".join(rows[name][h] for h in header) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
