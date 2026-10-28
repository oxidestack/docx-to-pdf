#!/usr/bin/env python3
"""Writes the named seeds of fuzz/seeds/, which `make lint ONLY=fuzz` replays on every push: small PNGs and JPEGs of
every layout the downscales branch on, each behind the first byte that picks the bound (fuzz_targets/*.rs: 1 asks
for a quarter of the bitmap, 3 for a sixty-fourth), plus a few malformed ones. Pillow, run by hand once (`python3
fuzz/make-seeds.py`); the files are committed, so CI needs neither. Deterministic: the same Pillow writes the same
bytes. Rewrites only what it names.
"""

import io
import struct
import zlib
from pathlib import Path

from PIL import Image

SEEDS = Path(__file__).resolve().parent / "seeds"
PICKS = (1, 3)
W, H = 64, 48


def gradient(mode):
    """A picture with structure in both axes, so a downscale has something to average."""
    rgba = Image.new("RGBA", (W, H))
    rgba.putdata([(x * 4, y * 5, (x ^ y) * 3 % 256, 255 - x * 2) for y in range(H) for x in range(W)])
    if mode == "P":
        return rgba.convert("RGB").quantize(16)
    if mode == "1":
        return rgba.convert("L").point(lambda v: 255 if v > 96 else 0).convert("1")
    return rgba.convert(mode)


def png(mode, **kw):
    out = io.BytesIO()
    image = gradient(mode)
    if kw.pop("sixteen", False):
        image = image.convert("I;16")
    image.save(out, "PNG", **kw)
    return out.getvalue()


def jpeg(mode, **kw):
    out = io.BytesIO()
    gradient(mode).save(out, "JPEG", quality=75, **kw)
    return out.getvalue()


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def png_adam7_flag():
    """The RGB seed with IHDR's interlace method set to Adam7 and its CRC mended (Pillow writes no interlaced PNG)."""
    body = bytearray(png("RGB"))
    body[28] = 1
    body[29:33] = struct.pack(">I", zlib.crc32(bytes(body[12:29])))
    return bytes(body)


def png_huge_header():
    """A valid PNG whose IHDR claims 65535 x 65535 and whose IDAT is one row: the bound is the claimed bitmap."""
    ihdr = struct.pack(">IIBBBBB", 65535, 65535, 8, 0, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(b"\0" * 65536)) + chunk(b"IEND", b"")
    )


PNGS = {
    "gray8": lambda: png("L"),
    "gray-alpha": lambda: png("LA"),
    "gray16": lambda: png("L", sixteen=True),
    "bilevel": lambda: png("1"),
    "rgb": lambda: png("RGB"),
    "rgba": lambda: png("RGBA"),
    "palette16": lambda: png("P"),
    "rgb-adam7-flag": png_adam7_flag,
    "rgb-truncated-idat": lambda: png("RGB")[:200],
    "huge-header-one-row": png_huge_header,
}

JPEGS = {
    "gray": lambda: jpeg("L"),
    "rgb-420": lambda: jpeg("RGB", subsampling=2),
    "rgb-422": lambda: jpeg("RGB", subsampling=1),
    "rgb-444": lambda: jpeg("RGB", subsampling=0),
    "rgb-progressive": lambda: jpeg("RGB", progressive=True),
    "cmyk": lambda: jpeg("CMYK"),
    "rgb-truncated-scan": lambda: jpeg("RGB")[:300],
}


def main():
    for target, seeds in (("png_downscale", PNGS), ("jpeg_downscale", JPEGS)):
        directory = SEEDS / target
        directory.mkdir(parents=True, exist_ok=True)
        for name, make in seeds.items():
            body = make()
            for pick in PICKS:
                (directory / f"{name}-pick{pick}").write_bytes(bytes([pick]) + body)
        (directory / "empty-after-pick").write_bytes(b"\x01")


if __name__ == "__main__":
    main()
