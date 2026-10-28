#!/usr/bin/env python3
"""Writes the JPEG fixtures of src/pictures/jpeg/fixtures/ that `tests_progressive.rs` reads, from pictures this script
draws itself: each `pattern-*.jpg` is a baseline JPEG libjpeg-turbo's `cjpeg` encodes from one, and each variant
beside it is that baseline transcoded without loss by its `jpegtran` (progressive; with restart markers; sequential
with each component in a scan of its own; made gray). Every output's markers are checked against the variant it
stands for, and a dump of them printed. `thumbnail.progressive.jpg`, the corpus's text-1p thumbnail made
progressive, is not this script's.

Run by hand once, in a container (the files are committed, so neither CI nor the tests need it):

    docker run --rm -v "$PWD/src/pictures/jpeg/fixtures:/out" \
        alpine@sha256:ff7f0c727dd796aa36787bfcd9a64bf35a6ce72a73451b3359fde849b6703d63 \
        sh -c 'apk add -q libjpeg-turbo-utils python3 && python3 /out/make-fixtures.py /out'

The fixtures were made with alpine 3.23's libjpeg-turbo 3.1.2.
"""

import struct
import subprocess
import sys
import tempfile
from pathlib import Path

# name, width, height, cjpeg sampling, quality: the sizes and samplings the fixtures this replaces had (odd sizes, so
# the pictures end in partial MCUs).
SOURCES = [
    ("pattern-61x71", 61, 71, "2x2", 75),
    ("pattern-130x194", 130, 194, "2x2", 95),
    ("pattern-195x129", 195, 129, "2x2", 95),
    ("pattern-257x131", 257, 131, "1x1", 98),
]

# jpegtran's default progression for three components (one for a single component: without the chroma scans).
PROGRESSION_3 = [
    ([1, 2, 3], 0, 0, 0, 1),
    ([1], 1, 5, 0, 2),
    ([3], 1, 63, 0, 1),
    ([2], 1, 63, 0, 1),
    ([1], 6, 63, 0, 2),
    ([1], 1, 63, 2, 1),
    ([1, 2, 3], 0, 0, 1, 0),
    ([3], 1, 63, 1, 0),
    ([2], 1, 63, 1, 0),
    ([1], 1, 63, 1, 0),
]
PROGRESSION_1 = [
    ([1], 0, 0, 0, 1),
    ([1], 1, 5, 0, 2),
    ([1], 6, 63, 0, 2),
    ([1], 1, 63, 2, 1),
    ([1], 0, 0, 1, 0),
    ([1], 1, 63, 1, 0),
]
SEQUENTIAL_3 = [([1, 2, 3], 0, 63, 0, 0)]
SEPARATE_3 = [([1], 0, 63, 0, 0), ([2], 0, 63, 0, 0), ([3], 0, 63, 0, 0)]

ADOBE = b"Adobe\0\x64\0\0\0\0\x01"  # version 100, no flags, colour transform 1 (YCbCr)
COMMENT = b"A picture drawn by make-fixtures.py"


def pixel(x, y, w, h):
    """Gradients, a disc of deterministic noise, diagonal stripes and a fine checker: edges and texture in both axes for
    a downscale to average. The disc and the checker are at most some 50 pixels across, to keep the files small."""
    u, v = x / w, y / h
    r, g, b = 40 + 180 * u, 30 + 190 * v, 200 - 150 * u * v
    noise = 0
    if (x - 0.62 * w) ** 2 + (y - 0.4 * h) ** 2 < min(w, h, 100) ** 2 / 16:
        r, g, b = 230 - 60 * v, 70 + 40 * u, 60
        noise = ((x * 7919 + y * 104_729) ^ (x * y * 31)) % 9 - 4
    if (x + 2 * y) % 23 < 4:
        r, g, b = r * 0.55, g * 0.55, b * 0.7
    if x < min(w // 4, 48) and y > max(3 * h // 4, h - 48) and (x // 2 + y // 2) % 2:
        r, g, b = 250, 250, 245
    return bytes(max(0, min(255, int(c + noise))) for c in (r, g, b))


def ppm(w, h):
    return b"P6\n%d %d\n255\n" % (w, h) + b"".join(pixel(x, y, w, h) for y in range(h) for x in range(w))


def run(*args, data=None):
    return subprocess.run(args, input=data, capture_output=True, check=True).stdout


def with_segments(jpeg, segments):
    """`jpeg` with `segments` (marker, payload) inserted after its JFIF APP0."""
    assert jpeg[2:4] == b"\xff\xe0"
    at = 4 + struct.unpack(">H", jpeg[4:6])[0]
    extra = b"".join(b"\xff" + bytes([m]) + struct.pack(">H", len(p) + 2) + p for m, p in segments)
    return jpeg[:at] + extra + jpeg[at:]


def markers(jpeg):
    """The frame, the restart interval, the APPn/COM markers, and each scan with its restart-marker count."""
    frame, dri, extra, scans = None, 0, [], []
    i = 2
    while i < len(jpeg):
        marker = jpeg[i + 1]
        if marker == 0xD9:
            break
        length = struct.unpack(">H", jpeg[i + 2 : i + 4])[0]
        seg = jpeg[i + 4 : i + 2 + length]
        if marker in (0xC0, 0xC1, 0xC2):
            h, w = struct.unpack(">HH", seg[1:5])
            comps = [f"{seg[7 + 3 * k] >> 4}x{seg[7 + 3 * k] & 15}" for k in range(seg[5])]
            frame = (f"SOF{marker - 0xC0}", w, h, comps)
        elif marker == 0xDD:
            dri = struct.unpack(">H", seg[:2])[0]
        elif 0xE0 <= marker <= 0xEF or marker == 0xFE:
            extra.append(f"{marker:02X}")
        if marker != 0xDA:
            i += 2 + length
            continue
        n = seg[0]
        comps = [seg[1 + 2 * k] for k in range(n)]
        ss, se, a = seg[1 + 2 * n], seg[2 + 2 * n], seg[3 + 2 * n]
        j, restarts = i + 2 + length, 0
        while not (jpeg[j] == 0xFF and jpeg[j + 1] not in range(0xD0, 0xD8) and jpeg[j + 1] != 0):
            restarts += jpeg[j] == 0xFF and jpeg[j + 1] in range(0xD0, 0xD8)
            j += 1
        scans.append(((comps, ss, se, a >> 4, a & 15), restarts))
        i = j
    return frame, dri, extra, scans


def check(name, jpeg, frame, scripts, dri=0, extra=("E0",)):
    got_frame, got_dri, got_extra, scans = markers(jpeg)
    assert got_frame == frame, (name, got_frame)
    assert got_dri == dri, (name, got_dri)
    assert got_extra == list(extra), (name, got_extra)
    assert [s for s, _ in scans] == scripts, (name, scans)
    assert all(r > 0 for _, r in scans) == (dri > 0), (name, scans)
    print(f"{name}: {got_frame[0]} {got_frame[1]}x{got_frame[2]} {' '.join(got_frame[3])}, DRI {got_dri}, {got_extra}")
    for (comps, ss, se, ah, al), restarts in scans:
        print(f"  SOS {comps} Ss={ss} Se={se} Ah={ah} Al={al}" + (f" RST={restarts}" if restarts else ""))


def main(out):
    out = Path(out)
    made = {}
    for name, w, h, sampling, quality in SOURCES:
        jpeg = run("cjpeg", "-quality", str(quality), "-sample", sampling, "-optimize", data=ppm(w, h))
        if sampling == "1x1":
            jpeg = with_segments(jpeg, [(0xFE, COMMENT), (0xEE, ADOBE)])
        made[name] = jpeg

    def tran(source, *args):
        return run("jpegtran", "-copy", "all", *args, data=made[source])

    with tempfile.NamedTemporaryFile("w", suffix=".txt") as scans:
        scans.write("0;\n1;\n2;\n")
        scans.flush()
        made["pattern-195x129.separate"] = tran("pattern-195x129", "-scans", scans.name)
    made["pattern-61x71.progressive"] = tran("pattern-61x71", "-progressive")
    made["pattern-130x194.progressive"] = tran("pattern-130x194", "-progressive")
    made["pattern-195x129.progressive-restart"] = tran("pattern-195x129", "-progressive", "-restart", "3B")
    made["pattern-257x131.progressive"] = tran("pattern-257x131", "-progressive")
    made["pattern-130x194.gray"] = tran("pattern-130x194", "-grayscale")
    made["pattern-130x194.gray.progressive"] = run(
        "jpegtran", "-copy", "all", "-progressive", data=made["pattern-130x194.gray"]
    )

    rgb420, rgb444 = ["2x2", "1x1", "1x1"], ["1x1", "1x1", "1x1"]
    expect = {
        "pattern-61x71": (("SOF0", 61, 71, rgb420), SEQUENTIAL_3, 0),
        "pattern-61x71.progressive": (("SOF2", 61, 71, rgb420), PROGRESSION_3, 0),
        "pattern-130x194": (("SOF0", 130, 194, rgb420), SEQUENTIAL_3, 0),
        "pattern-130x194.progressive": (("SOF2", 130, 194, rgb420), PROGRESSION_3, 0),
        "pattern-130x194.gray": (("SOF0", 130, 194, ["1x1"]), [([1], 0, 63, 0, 0)], 0),
        "pattern-130x194.gray.progressive": (("SOF2", 130, 194, ["1x1"]), PROGRESSION_1, 0),
        "pattern-195x129": (("SOF0", 195, 129, rgb420), SEQUENTIAL_3, 0),
        "pattern-195x129.progressive-restart": (("SOF2", 195, 129, rgb420), PROGRESSION_3, 3),
        "pattern-195x129.separate": (("SOF0", 195, 129, rgb420), SEPARATE_3, 0),
        "pattern-257x131": (("SOF0", 257, 131, rgb444), SEQUENTIAL_3, 0),
        "pattern-257x131.progressive": (("SOF2", 257, 131, rgb444), PROGRESSION_3, 0),
    }
    assert sorted(expect) == sorted(made)
    for name, (frame, scripts, dri) in expect.items():
        extra = ("E0", "FE", "EE") if name.startswith("pattern-257x131") else ("E0",)
        check(name, made[name], frame, scripts, dri, extra)
        (out / f"{name}.jpg").write_bytes(made[name])


if __name__ == "__main__":
    main(sys.argv[1])
