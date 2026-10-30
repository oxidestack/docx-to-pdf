#!/usr/bin/env python3
"""oxidestack/docx-to-pdf, Gotenberg and moalhaddar/docx-to-pdf compared by a command anyone can rerun.

    tools/compare/compare.sh [--quick] [--blocks N] [--soak N] [--only speed|torture] [--subjects a,b]
    (`make compare [QUICK=1]`; this file runs inside the runner container that compare.sh starts)

It repeats two measurements on record, with their own drivers:

- **speed and memory**: the speed cell on record (`tools/bench/matrices/compare-1cpu.tsv`):
  one pinned vCPU, pool 1, the typical report mix, a closed load of 3 clients, through `tools/bench/bench.py cell`
  (fresh containers, k6, the cgroup sampler) and `analyze.py`. The cells run in blocks, each block every cell once
  in a shuffled order, so a drift of the host moves every service alike.
- **the torture test**: the hostile documents on record through `tools/bench/torture.py`: each service alone in a
  fresh container, the answer, its time, the container's memory peak, a report sent alongside, the first report that
  converts afterwards.

`--quick` (minutes on a laptop): one block of 12 conversions; the deflate bomb, the gigapixel picture at
LibreOffice's bound, an empty file and 1 MiB of random bytes, each service under a 4 GiB limit. The full mode (a VM:
hours): three blocks of 52 conversions, the torture test's full sequence (the three bombs, four gigapixel pictures,
`tdf38575`, random bytes, the paused and the stalled upload, linked pictures against a listener) at 12 GiB, and a
soak of `--soak` conversions per service (moalhaddar/docx-to-pdf: at most 1000, the length on record; on a host
with a vCPU free for each soak they run alongside the rest, as the record's run did). moalhaddar/docx-to-pdf has
only a linux/amd64 image: on another architecture the run says so and skips it.

Writes `.bench/compare/<stamp>/`: `results.json` (every measurement, raw, and beside it the Docker host's CPU, caches,
memory, OS, kernel and hypervisor as tools/bench/provenance.py reads them, the machine compare.sh ran on, the command,
the commit, every image's digest, the revision oxidestack/docx-to-pdf's image labels, the documents' SHA-256, the seed
and the settings), `tables.md` (the tables printed at the end; `--tables <results.json>` prints them again from a
recorded run), and the drivers' own raw data. Every speed
figure is the mean ± σ (n) over the blocks, and a figure agrees with the record when the two means are within
2·√(σ₁²/n₁ + σ₂²/n₂). The figures on record are comparable only on the record's kind of host (an amd64 Xeon Gold
6248R VM). Gotenberg's rendering of the corpus has never been measured, so nothing here says anything about it.
"""

import argparse
import csv
import datetime
import hashlib
import json
import os
import random
import re
import statistics
import subprocess
import sys
import threading
import time
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(ROOT, "tools", "bench"))

import bench  # noqa: E402
import heavy  # noqa: E402
import provenance  # noqa: E402
import torture  # noqa: E402
from figures import agreement, fmt, record, sigma, spread, stats  # noqa: E402

MATRIX = os.path.join(ROOT, "tools", "bench", "matrices", "compare-1cpu.tsv")
OUT_ROOT = os.path.join(bench.DATA, "compare")
DOCS = os.path.join(OUT_ROOT, "docs")
REPORT = os.path.join(bench.DOCS, "report", "report-img30-10mb.docx")
SPEED_DOCS = bench.PROBE_DOCS.split(",")
SOAK_DOCS = ["report-text-2p", "report-text-5p", "report-text-20p"] + SPEED_DOCS
NAMES = {
    "ours": "oxidestack/docx-to-pdf",
    "gotenberg": "Gotenberg 8.38.1",
    "moalhaddar": "moalhaddar/docx-to-pdf",
}
# the speed cells by the subject torture.py calls them
CELL_SUBJECT = {"ours": "ours", "gotenberg": "gotenberg", "g-norestart": "gotenberg", "oracle": "moalhaddar"}
CELL_LABEL = {
    "ours": NAMES["ours"],
    "gotenberg": "Gotenberg, as shipped",
    "g-norestart": "Gotenberg, no restarts",
    "oracle": NAMES["moalhaddar"],
}

# LibreOffice's DOCX test document that hangs LibreOffice 7.3 (moalhaddar/docx-to-pdf's), MPL-2.0, fetched by commit.
TDF38575 = (
    "https://raw.githubusercontent.com/LibreOffice/core/0025f2b2608cf347a81380b1f44a6658e94bc2cd/"
    "sw/qa/extras/ooxmlexport/data/tdf38575_fullWidthLine.docx",
    "c6d022b650e67cb5281cfc71083dab16930d5515ef9ff78fe54d7c04ed9debaf",
)

# The speed cell on record: amd64, 2026-10-23, 3 replicates of 52 conversions (the host: HOST_ON_RECORD). Each figure
# is (mean, σ, n) over the replicates, σ the sample standard deviation, computed from the replicates' raw summaries.
RECORD_SPEED = {
    "ours": {
        "throughput": (0.998, 0.043, 3),
        "cpu": (0.9333, 0.0264, 3),
        "p50": (2.8536, 0.0649, 3),
        "p95": (3.3425, 0.2223, 3),
        "p99": (3.6168, 0.1733, 3),
        "peak": (411.0, 0.4539, 3),
        "anon": (322.85, 0.7178, 3),
        "written": (28.074, 0.003, 3),
        "ready": (0.51, 0.0173, 3),
    },
    "gotenberg": {
        "throughput": (0.7621, 0.0259, 3),
        "cpu": (1.1908, 0.0327, 3),
        "p50": (3.5837, 0.1134, 3),
        "p95": (4.6126, 0.1977, 3),
        "p99": (4.927, 0.4167, 3),
        "peak": (725.36, 3.1241, 3),
        "anon": (628.91, 13.3324, 3),
        "written": (27.851, 0.0982, 3),
        "ready": (1.0067, 0.0058, 3),
    },
    "g-norestart": {
        "throughput": (0.8542, 0.0251, 3),
        "cpu": (1.1139, 0.0217, 3),
        "p50": (3.4692, 0.0634, 3),
        "p95": (3.898, 0.1943, 3),
        "p99": (3.9951, 0.2391, 3),
        "peak": (770.56, 63.065, 3),
        "anon": (654.74, 83.0744, 3),
        "written": (28.146, 0.2818, 3),
        "ready": (1.01, 0.0173, 3),
    },
    "oracle": {
        "throughput": (0.2992, 0.004, 3),
        "cpu": (3.2051, 0.0021, 3),
        "p50": (9.7932, 0.0774, 3),
        "p95": (10.8818, 0.0728, 3),
        "p99": (11.18, 0.0892, 3),
        "peak": (1062.66, 110.3282, 3),
        "anon": (924.88, 111.2794, 3),
        "written": (29.808, 0.0004, 3),
        "ready": (13.28, 0.5976, 3),
    },
}
# The host of the record: what its runs recorded (the guest's view and the VM's definition) and, marked as such, the
# CPU vendor's specification of the model.
HOST_ON_RECORD = (
    "an Intel Xeon Gold 6248R VM (Cascade Lake; the guest sees 8 vCPUs as 1 socket × 8 cores, 1 thread a core, "
    "@ 3.00 GHz; 32 GiB; Ubuntu 24.04, kernel 6.8, Docker 29.9, cgroup v2; VMware vCPUs, pinning not "
    "visible from the guest. The model's specification: 24 cores / 48 threads, 3.0 GHz base, 4.0 GHz turbo, "
    "35.75 MB L3)"
)
# The torture test on record (amd64, the same VM type, 2026-10-26, one run each) and, for a few cases, the earlier
# single-document measurement it quotes.
RECORD_TORTURE = {
    ("pages-500000p", "ours"): "413 in 0.01 s; peak 237 MiB",
    ("pages-500000p", "gotenberg"): "503 in 30.0 s; peak 7 600 MiB; first 200 220 s after",
    ("pages-500000p", "moalhaddar"): "no answer in 300 s; peak 9 233 MiB",
    ("px-8x1024mpx", "ours"): "413 in 71.4 s; peak 1 827 MiB; 76 × 200 alongside",
    ("px-8x1024mpx", "gotenberg"): "503 in 30.0 s; peak 8 085 MiB; first 200 33 s after",
    ("px-8x1024mpx", "moalhaddar"): "no answer in 300 s; peak 2 564 MiB",
    ("tables-10000", "ours"): "504 in 180.1 s; peak 1 289 MiB",
    ("tables-10000", "gotenberg"): "503 in 30.0 s; peak 1 931 MiB; first 200 218 s after",
    ("tables-10000", "moalhaddar"): "no answer in 300 s; 3 501 MiB at 10 min",
    ("png-g-32767", "ours"): "200, downscaled ÷2, 10.0 s",
    ("png-g-32767", "gotenberg"): "200 in 9.2 s, picture kept",
    ("png-g-32767", "moalhaddar"): "200, picture kept",
    ("png-g-32768", "ours"): "200, downscaled ÷2, 10.0 s",
    ("png-g-32768", "gotenberg"): "200 in 0.8 s, picture dropped",
    ("png-g-32768", "moalhaddar"): "200, picture dropped",
    ("png-rgb-18918", "ours"): "200, downscaled ÷4, 4.2 s",
    ("png-rgb-18918", "gotenberg"): "200 in 10.5 s, picture kept",
    ("png-rgb-18918", "moalhaddar"): "200, picture kept",
    ("png-rgb-18919", "ours"): "200, downscaled ÷4, 4.2 s",
    ("png-rgb-18919", "gotenberg"): "200 in 0.7 s, picture dropped",
    ("png-rgb-18919", "moalhaddar"): "200, picture dropped",
    ("empty", "ours"): "415",
    ("empty", "gotenberg"): "200 in 0.9 s",
    ("empty", "moalhaddar"): "200",
    ("random-1mb", "ours"): "415",
    ("random-1mb", "gotenberg"): "200 in 20.3 s",
    ("random-10mb", "ours"): "415",
    ("random-10mb", "gotenberg"): "503 in 30.0 s",
    ("random-10mb", "moalhaddar"): "9 MB: no answer after 15 min",
    ("tdf38575", "ours"): "200 in about 0.3 s",
    ("tdf38575", "gotenberg"): "200 in 0.9 s",
    ("tdf38575", "moalhaddar"): "no answer in 180-300 s",
    ("pause", "ours"): "converts; a stalled upload 408 at 120 s",
    ("pause", "gotenberg"): "200 in 7.1-7.8 s; a stalled upload 500 at 30.0 s",
    ("pause", "moalhaddar"): "500 after Tomcat's timeout",
    ("egress", "ours"): "no connection (make image-egress)",
    ("egress", "gotenberg"): "68 of 68 200; no connection",
    ("egress", "moalhaddar"): "33 connections, public and private",
    ("soak", "ours"): "3000, 0 errors; 465-480 MiB; +0.8 MiB per 1000",
    ("soak", "gotenberg"): "3000, 0 errors; median 246-247 MiB",
    ("soak", "g-norestart"): "3000, 0 errors; median 444-456 MiB; +1.4 MiB per 1000",
    ("soak", "moalhaddar"): "1000; 213 → 471 MiB; +152 MiB per 1000",
}
OURS_SOAK_ENV = ["DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS=0", f"DOCX_TO_PDF_MAX_DOCUMENT_SIZE={300 << 20}"]
# How long a torture case waits for an answer, as on record.
CAP = 300
# moalhaddar/docx-to-pdf's linked-picture run waits less a document. After its ODT OLE link it answers nothing more, so
# at CAP the 17 documents after it took 85 minutes of the full mode; its slowest answer before that, ten pictures
# linked to an address that never answers (10 s each), took 100 s. At 150 s every document answers or not as at CAP.
LINKED_CAP_MOALHADDAR = 150
GIGAPIXEL = {
    "png-g-32767": (32767, 1),
    "png-g-32768": (32768, 1),
    "png-rgb-18918": (18918, 3),
    "png-rgb-18919": (18919, 3),
}


def say(*parts):
    print("==", *parts, file=sys.stderr, flush=True)


def docker_arch():
    return bench.sh("docker", "info", "--format", "{{.Architecture}}", quiet=True)


def pull(subjects):
    """Every image a run starts, pulled first: bench.py only inspects them (a first run of the speed cell failed on
    images never pulled), and a registry hiccup is retried rather than lost as a failed cell."""
    images = [bench.K6_IMAGE, torture.PYTHON]
    images += [torture.GOTENBERG] if "gotenberg" in subjects else []
    images += [torture.ORACLE, bench.oracle_image()] if "moalhaddar" in subjects else []
    for image in dict.fromkeys(images):
        if subprocess.run(["docker", "image", "inspect", image], capture_output=True).returncode == 0:
            continue
        for attempt in range(1, 4):
            say(f"pulling {image} ({attempt}/3)")
            if subprocess.run(["docker", "pull", "--quiet", image]).returncode == 0:
                break
            time.sleep(10 * attempt)
        else:
            sys.exit(f"could not pull {image}")


def patch_torture(image, memory, cpus):
    """torture.py as the record's run used it, with two local facts: the image under test, and the memory and cpus
    the host can give. (torture.py finds the container's cgroup itself, through bench.cgroup_dir.)"""
    torture.SUBJECTS["ours"]["image"] = image
    init = torture.Service.__init__

    def service(self, subject, name, host_port, **kw):
        kw.setdefault("memory", memory)
        kw.setdefault("cpus", cpus)
        init(self, subject, name, host_port, **kw)

    torture.Service.__init__ = service


# ---- documents ---------------------------------------------------------------------------------------------------


def bench_docs(names):
    """The bench's seeded documents (generate.py, in the tool image), the ones not in .bench/docs yet."""
    have = set()
    manifest = os.path.join(bench.DOCS, "manifest.tsv")
    if os.path.exists(manifest):
        with open(manifest) as f:
            have = {line.split("\t")[1] for line in f.readlines()[1:]}
    missing = [n for n in names if n not in have]
    if missing:
        say("generating", ",".join(missing))
        if bench.tool("generate.py", "/data/docs", "--only", ",".join(missing)):
            sys.exit("generate.py failed")


def torture_doc(name):
    """A torture document under .bench/compare/docs/, written once: heavy.py's bombs, flat
    gigapixel PNGs at LibreOffice's 1 GiB bitmap bound, an empty file, random bytes, and tdf38575 fetched by
    commit and checked by SHA-256."""
    path = os.path.join(DOCS, f"{name}.docx")
    if os.path.exists(path):
        return path
    os.makedirs(DOCS, exist_ok=True)
    tmp = path + ".part"
    say("writing", name)
    if name == "pages-500000p":
        heavy.write_docx(tmp, heavy.pages_bomb_body(name, 500000), large=True)
    elif name == "tables-10000":
        heavy.write_docx(tmp, heavy.tables_body(name, 10000))
    elif name == "px-8x1024mpx":
        images, body, _ = heavy.pixels(name, 1024, 8, 1)
        heavy.write_docx(tmp, body, images)
    elif name in GIGAPIXEL:
        side, channels = GIGAPIXEL[name]
        body = [heavy.heading(f"One flat PNG, {side}² × {channels} channels"), heavy.picture(0, side, side)]
        heavy.write_docx(tmp, body, [(heavy.flat_png(side, channels=channels), "png")])
    elif name == "empty":
        open(tmp, "wb").close()
    elif name.startswith("random-"):
        size = int(name.split("-")[1].removesuffix("mb")) << 20
        with open(tmp, "wb") as f:
            f.write(random.Random(name).randbytes(size))
    elif name == "tdf38575":
        with urllib.request.urlopen(TDF38575[0], timeout=60) as r:
            data = r.read()
        if hashlib.sha256(data).hexdigest() != TDF38575[1]:
            sys.exit(f"tdf38575: unexpected SHA-256 from {TDF38575[0]}")
        open(tmp, "wb").write(data)
    else:
        raise ValueError(name)
    os.replace(tmp, path)
    return path


# ---- speed -------------------------------------------------------------------------------------------------------


def speed(out, cells, blocks, conversions, image, seed):
    """The speed cells, in blocks: each block runs every cell once, shuffled."""
    with open(MATRIX) as f:
        rows = {r["cell"]: r for r in csv.DictReader((x for x in f if not x.startswith("#")), delimiter="\t")}
    rng = random.Random(seed)
    order = []
    for block in range(1, blocks + 1):
        names = list(cells)
        rng.shuffle(names)
        order += [(block, n) for n in names]
    failures = {}
    for i, (block, name) in enumerate(order, 1):
        row = dict(rows[name])
        if conversions:
            row["conversions"] = str(conversions)
            row["cooldown"] = "5"
        argv = [name]
        for key, value in row.items():
            if key != "cell" and value:
                for v in value.split(";") if key == "env" else [value]:
                    argv += [f"--{key.replace('_', '-')}", v]
        parser = argparse.ArgumentParser()
        parser.add_argument("name")
        bench.cell_options(parser)
        args = parser.parse_args(argv)
        args.out = os.path.join(out, "speed", name, f"r{block}")
        if name == "ours":
            args.image = image
        say(f"speed [{i}/{len(order)}] block {block}: {CELL_LABEL[name]}")
        try:
            bench.cell(args)
        except (RuntimeError, SystemExit) as error:
            failures[f"{name}/r{block}"] = str(error).strip().splitlines()[0][:200]
            say(f"{name} r{block} failed: {error}")
    rel = os.path.relpath(os.path.join(out, "speed"), bench.DATA)
    if bench.tool("analyze.py", f"/data/{rel}", "--out", f"/data/{rel}-report"):
        say("analyze.py failed")
    results = {}
    for name in cells:
        reps = []
        for block in range(1, blocks + 1):
            run = os.path.join(out, "speed", name, f"r{block}")
            summary, meta = os.path.join(run, "summary.json"), os.path.join(run, "run.json")
            if os.path.exists(summary):
                s = json.load(open(summary))
                s["ready_seconds"] = json.load(open(meta)).get("ready_seconds")
                reps.append(s)
        results[name] = reps
    return results, failures


def metric(s, key):
    mib, mb = 2**20, 1e6
    return {
        "throughput": lambda: s["throughput"],
        "cpu": lambda: s["cpu_seconds_per_conversion"],
        "p50": lambda: s["latency"]["p50"],
        "p95": lambda: s["latency"]["p95"],
        "p99": lambda: s["latency"]["p99"],
        "peak": lambda: s["memory_peak"] / mib,
        "anon": lambda: s["anon_max"] / mib,
        "written": lambda: s["write_bytes_per_conversion"] / mb,
        "ready": lambda: s["ready_seconds"],
    }[key]()


ROWS = [
    ("conversions/s", "throughput", 3),
    ("CPU-s a conversion", "cpu", 3),
    ("p50 s", "p50", 2),
    ("p95 s", "p95", 2),
    ("p99 s", "p99", 2),
    ("`memory.peak` MiB", "peak", 0),
    ("anonymous peak MiB", "anon", 0),
    ("written a conversion, MB", "written", 1),
    ("`docker run` to ready, s", "ready", 2),
]


def speed_table(results, failures, comparable):
    cells = [c for c in results if results[c] or any(k.startswith(c + "/") for k in failures)]
    lines = ["| | " + " | ".join(CELL_LABEL[c] for c in cells) + " |", "|---" * (len(cells) + 1) + "|"]
    lines.append(
        "| replicates (errors) | "
        + " | ".join(f"{len(results[c])} ({sum(s['errors'] for s in results[c])})" for c in cells)
        + " |"
    )
    verdicts = []
    for label, key, digits in ROWS:
        row = []
        for c in cells:
            values = [metric(s, key) for s in results[c]]
            row.append(spread(values, digits))
            new, rec = stats(values), RECORD_SPEED[c][key]
            if comparable and new:
                delta, bound = agreement(new, rec)
                if delta is None:
                    continue
                verdict = "agrees" if abs(delta) <= bound else "differs"
                verdicts.append(
                    (
                        CELL_LABEL[c],
                        label,
                        record(new, digits),
                        record(rec, digits),
                        f"{verdict}: Δ {delta / rec[0] * 100:+.1f} % ({fmt(delta, digits + 1)}; "
                        f"2σ {fmt(bound, digits + 1)})",
                    )
                )
        lines.append(f"| {label} | " + " | ".join(row) + " |")
    lines.append("| *on record (amd64, 2026-10-23)* | " + " | ".join("" for _ in cells) + " |")
    for label, key, digits in ROWS:
        lines.append(f"| *{label}* | " + " | ".join(record(RECORD_SPEED[c][key], digits) for c in cells) + " |")
    return lines, verdicts


# ---- torture -----------------------------------------------------------------------------------------------------


def statuses(reqs):
    count = {}
    for r in reqs:
        key = str(r["status"])[:40]
        count[key] = count.get(key, 0) + 1
    return ", ".join(f"{n} × {k}" for k, n in count.items()) or "none"


def answer(rec):
    status = rec.get("status")
    if isinstance(status, str):
        status = "no answer" if "timed out" in status or "Timeout" in status else status
    return f"{status} in {rec.get('seconds')} s"


def mib(v):
    return f"{v / 2**20:,.0f} MiB".replace(",", " ") if v else "—"


def hostile(out, subject, name, recover, cap):
    run = os.path.join(out, "torture", f"hostile-{subject}-{name}")
    torture.hostile(
        argparse.Namespace(subject=subject, doc=torture_doc(name), alongside=REPORT, cap=cap, recover=recover, out=run)
    )
    res = json.load(open(os.path.join(run, "result.json")))
    side = res.get("alongside_requests", [])
    during = [r for r in side if r.get("during")]
    after = res.get("first_200_after_answer_s")
    peak = (res.get("state_after") or {}).get("memory_peak")
    events = (res.get("state_after") or {}).get("memory_events", {})
    cell = (
        f"{answer(res.get('hostile', {}))}; peak {mib(peak)}; alongside {statuses(during)}; "
        f"first 200 after the answer: {f'{after} s' if after is not None else 'none'}"
    )
    if events.get("oom_kill"):
        cell += f"; OOM kills {events['oom_kill']}"
    return {"case": name, "subject": subject, "kind": "hostile", "result": res, "cell": cell}


def one(out, subject, name, cap):
    """torture.py's `one`, with the PDF's pictures counted in the runner (image XObjects) instead of pdfimages."""
    doc = torture_doc(name)
    svc = torture.Service(subject, f"docx-to-pdf-compare-{subject}-one", 18125)
    try:
        ready = svc.wait_ready()
        rec, pdf = torture.post(svc.url + svc.s["path"], doc, svc.s["field"], cap)
        rec["ready_s"] = ready
        rec["state"] = svc.state()
        if rec["status"] == 200:
            rec["pdf_images"] = len(re.findall(rb"/Subtype\s*/Image\b", pdf))
    finally:
        svc.remove()
    cell = f"{answer(rec)}; peak {mib(rec['state'].get('memory_peak'))}"
    if "pdf_images" in rec:
        cell += f"; pictures in the PDF {rec['pdf_images']}"
    pictures = {k: v for k, v in rec.get("headers", {}).items() if k.lower().startswith("docx-to-pdf")}
    if pictures:
        cell += "; " + "; ".join(f"{k}: {v}" for k, v in pictures.items())
    if rec.get("status") not in (200,) and rec.get("body"):
        cell += f"; “{rec['body'][:80].strip()}”"
    return {"case": name, "subject": subject, "kind": "one", "result": rec, "cell": cell}


def pause(out, subject):
    run = os.path.join(out, "torture", f"pause-{subject}")
    torture.pause(argparse.Namespace(subject=subject, doc=REPORT, pauses="2,2,2", stall=200, out=run))
    res = json.load(open(os.path.join(run, "result.json")))
    cell = (
        "pauses: "
        + ", ".join(f"{p['answer'].split(' (')[0]} in {p['seconds']} s" for p in res["pauses"])
        + f"; stalled: {res['stall']['answer'].split(' (')[0]} in {res['stall']['seconds']} s"
    )
    return {"case": "pause", "subject": subject, "kind": "pause", "result": res, "cell": cell}


def listener_connections(path):
    """The connections in the listener's log by the address they reached: the stand-in internet (198.18.N.10,
    public) or the private network (10.231.N.10). How many there are grows with the run's length (moalhaddar/
    docx-to-pdf's LibreOffice reconnects to the private address every 300 s); which addresses, does not."""
    split = {"public": 0, "private": 0}
    with open(path) as f:
        for line in f:
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if isinstance(event, dict) and event.get("event") == "connect":
                split["public" if str(event.get("local", "")).startswith("198.18.") else "private"] += 1
    return split


def egress(out, subject, cap=CAP):
    run = os.path.join(out, "torture", f"egress-{subject}")
    torture.egress(argparse.Namespace(subject=subject, egress=os.path.join(ROOT, "image", "egress"), cap=cap, out=run))
    split = listener_connections(os.path.join(run, "listener.jsonl"))
    connections = split["public"] + split["private"]
    path = os.path.join(run, "pdf", "results.jsonl")
    recs = []
    if os.path.exists(path):
        with open(path) as f:
            recs = [json.loads(line) for line in f]
    cell = f"{len(recs)} documents: {statuses(recs)}; connections the listener logged: {connections}"
    if connections:
        cell += f" ({split['public']} public, {split['private']} private)"
    if cap != CAP:
        cell += f"; {cap} s a document"
    return {
        "case": "egress",
        "subject": subject,
        "kind": "egress",
        "result": {"connections": connections, "connections_by_address": split, "cap_s": cap, "requests": recs},
        "cell": cell,
    }


def soak(out, subject, n, env, label, cpus="0", port=18127):
    run = os.path.join(out, "torture", f"soak-{label}")
    torture.soak(
        argparse.Namespace(
            subject=subject,
            doc_dir=bench.DOCS,
            manifest=os.path.join(bench.DOCS, "manifest.tsv"),
            classes="report",
            n=n,
            name=label,
            port=port,
            cpus=cpus,
            memory="4g",
            env=env,
            out=run,
        )
    )
    summary = soak_summary(os.path.join(run, "soak.tsv"))
    return {"case": "soak", "subject": label, "kind": "soak", "result": summary["result"], "cell": summary["cell"]}


def rss_cell(s):
    mean, sd, n = s
    return f"{fmt(mean, 0)} ± {sigma(mean, sd, 0)} MiB ({n} requests)" if n else "—"


def fit(points):
    """The least-squares slope of (i, MiB) points in MiB per 1000 requests ± its standard error; (None, None) when
    there are too few points to fit."""
    if len(points) <= 2:
        return None, None
    mx, my = statistics.mean(i for i, _ in points), statistics.mean(v for _, v in points)
    den = sum((i - mx) ** 2 for i, _ in points)
    if not den:
        return None, None
    b = sum((i - mx) * (v - my) for i, v in points) / den
    residual = sum((v - my - b * (i - mx)) ** 2 for i, v in points)
    return b * 1000, (residual / (len(points) - 2) / den) ** 0.5 * 1000


# The smallest rise a soak calls growth, as a fraction of its level per 1000 conversions. At a 1 MiB spread a
# 3000-conversion soak resolves a fraction of a MiB per 1000 from zero; a rise that small is no leak an operator could
# act on. 1 % per 1000 is 4 MiB on a 400 MiB LibreOffice, 0.8 MiB over the 200 conversions an instance lives by default
# and 10 % over 10 000 without recycling, while the climbs of Gotenberg and moalhaddar/docx-to-pdf on record ran at
# 7 % or more.
GROWTH_FLOOR = 0.01


def growth_floor(level):
    """The floor in MiB per 1000 conversions for a soak whose LibreOffice sits at `level` MiB."""
    return GROWTH_FLOOR * level


def growth(slope, se, half, half_se, floor=0.0):
    """Whether a soak's memory grows. The nine tenths' slope says whether it rose; the second half's says whether it
    still rises at the end. A climb that stops early and a plateau fit as a rise over nine tenths, but the second half
    is flat, below that rise at 2σ: it levels off. The second half alone does not judge: a slow climb under a wide
    spread (a 1000-request soak) is not resolved from zero over 500 requests, and over 1500 a fraction of a MiB per
    1000 already is. A rise resolved but no steeper than `floor` MiB per 1000 does not grow either: at a tight spread
    the fit resolves rises too small to matter."""
    if slope is None or slope <= 2 * se or slope <= floor:
        return "does not grow"
    if half is not None and slope - half > 2 * (se**2 + half_se**2) ** 0.5:
        return "levels off"
    return "grows"


def soak_summary(path):
    """A soak's soak.tsv: LibreOffice's RSS after each request as mean ± σ (n) over the first tenth and the second
    half; the least-squares slope over the last nine tenths and over the second half, each ± its standard error; and
    whether the memory grows, judged by both against a floor of 1 % of the second half's level (growth())."""
    rows = []
    with open(path) as f:
        f.readline()
        for line in f:
            i, _doc, status, _s, _cur, _anon, rss, _t = line.rstrip("\n").split("\t")
            procs = json.loads(rss)
            key = "instance" if "instance" in procs else "soffice.bin"
            rows.append((int(i), status, procs.get(key, 0) / 1024))
    errors = sum(1 for r in rows if r[1] != "200")
    live = [(i, v) for i, _, v in rows if v > 50]  # a restarted LibreOffice is ~11 MiB until it converts
    tenth, half = len(rows) // 10, len(rows) // 2
    first = stats([v for i, v in live if i < tenth]) or (0, None, 0)
    second = stats([v for i, v in live if i >= half]) or (0, None, 0)
    slope, se = fit([(i, v) for i, v in live if i >= tenth])
    half_slope, half_se = fit([(i, v) for i, v in live if i >= half])
    floor = growth_floor(second[0])
    verdict = growth(slope, se, half_slope, half_se, floor)
    cell = (
        f"{len(rows)}, {errors} errors; LibreOffice RSS {rss_cell(first)} over the first tenth / "
        f"{rss_cell(second)} over the second half; "
        + (f"{slope:+.1f} ± {se:.1f} MiB per 1000" if slope is not None else "— MiB per 1000")
        + " over the last nine tenths, "
        + (f"{half_slope:+.1f} ± {half_se:.1f}" if half_slope is not None else "—")
        + f" over the second half; floor {floor:.1f} MiB per 1000 ({GROWTH_FLOOR * 100:g} % of the level): {verdict}"
    )
    result = {
        "rows": len(rows),
        "errors": errors,
        "slope_per_1000": slope,
        "slope_se": se,
        "slope_second_half_per_1000": half_slope,
        "slope_second_half_se": half_se,
        "growth_floor_per_1000": floor,
        "growth": verdict,
    }
    return {"result": result, "cell": cell}


def torture_table(rows):
    cases = []
    for r in rows:
        if r["case"] not in cases:
            cases.append(r["case"])
    subjects = []
    for r in rows:
        if r["subject"] not in subjects:
            subjects.append(r["subject"])
    lines = ["| Case | Service | This run | On record (amd64, 2026-10-26) |", "|---|---|---|---|"]
    for case in cases:
        for subject in subjects:
            hit = [r for r in rows if r["case"] == case and r["subject"] == subject]
            if hit:
                label = CELL_LABEL.get(subject, NAMES.get(subject, subject)) if case == "soak" else NAMES[subject]
                lines.append(f"| `{case}` | {label} | {hit[0]['cell']} | {RECORD_TORTURE.get((case, subject), '—')} |")
    return lines


# ---- the host and the provenance ------------------------------------------------------------------------------


def docker_host():
    """The Docker host as the runner sees it: the runner shares the host kernel, so /proc and /sys are the host's (a
    Docker VM's on a laptop), read by the bench's provenance.linux_host() as bench.py reads them in its tool container,
    and completed by `docker info`. A guest cannot see how its vCPUs map to physical cores (pinning)."""
    info = json.loads(bench.sh("docker", "info", "--format", "{{json .}}", quiet=True) or "{}")
    return provenance.with_docker_info(provenance.linux_host(), info)


def machine():
    """The machine compare.sh ran on, as `provenance.py machine` printed it there (COMPARE_MACHINE): on a Mac the
    chip, which the runner inside the Docker VM cannot see. None when compare.sh could not read it."""
    try:
        return json.loads(os.environ.get("COMPARE_MACHINE") or "null")
    except ValueError:
        return None


def host_line(meta):
    """The run's machine and Docker host as the bench's reports print them. A run recorded before compare read its
    host through provenance.py says only whether the CPU reported a hypervisor."""
    h = meta.get("host")
    if h and isinstance(h.get("hypervisor"), bool):
        h = {**h, "hypervisor": "a hypervisor" if h["hypervisor"] else None}
    return provenance.host_line(h, meta.get("machine"))


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def documents(names, torture_names):
    """Each document's SHA-256: the bench's from its manifest (generated with a seed fixed by the name), the torture
    documents from their files."""
    out = {}
    manifest = os.path.join(bench.DOCS, "manifest.tsv")
    if os.path.exists(manifest):
        with open(manifest) as f:
            rows = {r["name"]: r["sha256"] for r in csv.DictReader(f, delimiter="\t")}
        out.update({n: rows[n] for n in names if n in rows})
    for n in torture_names:
        path = os.path.join(DOCS, f"{n}.docx")
        if os.path.exists(path):
            out[n] = sha256(path)
    return out


def revision_line(meta):
    """The tables' word on the image's source: the commit it labels, beside the checkout's. A run recorded before the
    revision was recorded says nothing."""
    if "image_revision" not in meta:
        return ""
    rev = meta["image_revision"]
    return f"; the image labels revision `{rev}`" if rev else "; the image carries no revision label"


def pinned(ref, digest):
    """`ref` pinned to `digest`, unless it is pinned already (`IMAGE=<name>:<tag>@sha256:…`): one digest, never two."""
    return ref if "@" in ref else f"{ref}@{digest}"


def image_id(ref):
    return bench.sh("docker", "image", "inspect", "--format", "{{.Id}}", ref, check=False, quiet=True) or None


# ---- the tables --------------------------------------------------------------------------------------------------


def render(results, base=None):
    """tables.md from results.json: what the run printed, re-made from the raw results (`--tables`). Given the run's
    directory, a soak's cell is computed again from its soak.tsv, so a run recorded before a change of the summary
    prints as a run today would."""
    meta = results["meta"]
    for row in results.get("torture", []):
        path = os.path.join(base or "", "torture", f"soak-{row['subject']}", "soak.tsv")
        if base and row["kind"] == "soak" and os.path.exists(path):
            row["cell"] = soak_summary(path)["cell"]
    quick = meta["mode"] == "quick"
    comparable = meta["docker_arch"] in ("x86_64", "amd64") and not quick
    lines = [
        f"# docx-to-pdf, Gotenberg and moalhaddar/docx-to-pdf — {meta['mode']} run, {meta['stamp']}",
        "",
        f"Host: {host_line(meta)}.",
        "",
        f"Command: `{meta.get('command', 'not recorded')}` at commit `{meta.get('commit', 'not recorded')}`"
        f"{revision_line(meta)}; "
        f"speed seed {meta.get('seed', results.get('speed', {}).get('seed', '—'))}.",
        "",
        "Images: "
        + "; ".join(
            f"{k} `{v}`"
            for k, v in (meta.get("images") or {NAMES["ours"]: f"{meta['image']} {meta['image_id']}"}).items()
        )
        + ".",
        "",
    ]
    if meta.get("documents"):
        lines += ["Documents (SHA-256): " + "; ".join(f"{k} `{v[:16]}`" for k, v in meta["documents"].items()), ""]
    lines += [f"- {n}" for n in meta.get("notes", [])] + ([""] if meta.get("notes") else [])

    if "speed" in results:
        sp = results["speed"]
        table, verdicts = speed_table(sp["cells"], sp["failures"], comparable)
        lines += (
            [
                "## Speed and memory: the cell on record",
                "",
                f"One pinned vCPU, pool 1, a closed load of 3 clients, the typical report mix "
                f"({', '.join(SPEED_DOCS)}), {meta['conversions']} conversions a cell after one warm-up per document, "
                f"16 GiB, `/tmp` on the Docker host's disk, tracing off; {meta['blocks']} block(s), each running every "
                "cell once in a shuffled order. Each figure is the mean ± σ (n) over the blocks, σ the sample "
                "standard deviation. Gotenberg's API timeout is raised to 900 s here, as the record's run had it. "
                f"The record's host: {HOST_ON_RECORD}.",
                "",
            ]
            + table
            + [""]
        )
        if sp["failures"]:
            lines += ["Failed cells: " + "; ".join(f"{k}: {v}" for k, v in sp["failures"].items()), ""]
        if verdicts:
            lines += [
                "Against the record (two means agree when the difference Δ is within 2σ = 2·√(σ₁²/n₁ + σ₂²/n₂); a "
                "difference is a reason for "
                "an interleaved, paired measurement, not a result):",
                "",
                "| Service | Figure | This run | On record | Verdict |",
                "|---|---|---|---|---|",
            ]
            lines += [f"| {c} | {m} | {v} | {r} | {w} |" for c, m, v, r, w in verdicts] + [""]

    if "torture" in results:
        soak_n = meta.get("soak", 0)
        soak_m = meta.get("soak_moalhaddar", soak_n)
        lines += (
            [
                "## The torture test: the documents on record",
                "",
                f"Each service alone in a fresh container, as shipped (Gotenberg's API timeout 30 s; "
                f"oxidestack/docx-to-pdf's defaults, pool 1), on cpus {meta['torture_cpus']} with "
                f"{meta['torture_memory']} of memory. One run a case (n = 1): the outcomes are set by limits, so no σ. "
                "A bomb is sent with `report-img30-10mb` resent alongside from 2 s in; then the report again "
                + ("once." if quick else "until one converts (at most 600 s).")
                + " Peak is the container's `memory.peak`, one warm-up report included."
                + (
                    f" Each case waits at most {meta.get('cap', CAP)} s for an answer; moalhaddar/docx-to-pdf's "
                    f"linked pictures {meta['linked_cap_moalhaddar']} s a document, since after its ODT OLE link it "
                    "answers nothing more (its slowest answer before that takes about 100 s). Its connection count "
                    "grows with the run's length, as its LibreOffice reconnects every 300 s; the addresses do not."
                    if not quick
                    and "moalhaddar" in meta.get("subjects", [])
                    and meta.get("linked_cap_moalhaddar", CAP) != meta.get("cap", CAP)
                    else ""
                )
                + (
                    f" The soaks ({soak_n} conversions a service, {soak_m} for moalhaddar/docx-to-pdf, cycling the "
                    "seven reports, 4 GiB; oxidestack/docx-to-pdf on one instance, recycling off, with a "
                    "300 MiB document limit, as its soak on record ran) ran "
                    + (
                        "alongside the rest, each on a vCPU of its own, as the record's run did: their memory is "
                        "the measurement, not their time."
                        if meta.get("soak_alongside")
                        else "one after another at the end."
                    )
                    + " A soak's memory is the mean ± σ (n) of LibreOffice's RSS after each request, and its slopes "
                    "the least-squares fits over the last nine tenths and over the second half ± their standard "
                    "errors. The second half judges growth: a soak grows when its nine tenths rise at 2σ and its "
                    "second half is not below that rise at 2σ; when it is, the memory climbed and levelled off. A "
                    "rise no steeper than 1 % of the second half's level per 1000 conversions, the row's floor, does "
                    "not grow."
                    if soak_n and not quick
                    else ""
                ),
                "",
            ]
            + torture_table(results["torture"])
            + [""]
        )

    if "elapsed_s" in meta:
        lines += [f"The run took {meta['elapsed_s'] // 60} min {meta['elapsed_s'] % 60} s."]
    return "\n".join(lines) + "\n"


# ---- main --------------------------------------------------------------------------------------------------------


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--quick", action="store_true", help="minutes on a laptop: one block of 12, a torture subset")
    p.add_argument("--blocks", type=int, help="speed blocks (default: 1 quick, 3 full)")
    p.add_argument("--conversions", type=int, help="speed conversions a cell (default: 12 quick, 52 full)")
    p.add_argument("--soak", type=int, help="soak conversions a service, full mode (default 3000; 0 skips)")
    p.add_argument(
        "--soak-moalhaddar",
        type=int,
        help="moalhaddar/docx-to-pdf's soak (default: --soak, at most 1000, the length on record)",
    )
    p.add_argument(
        "--soak-alone",
        action="store_true",
        help="run the soaks one after another after the torture, not alongside it on their own vCPUs",
    )
    p.add_argument(
        "--linked-cap-moalhaddar",
        type=int,
        default=LINKED_CAP_MOALHADDAR,
        metavar="S",
        help=f"seconds moalhaddar/docx-to-pdf's linked-picture run waits a document, full mode "
        f"(default {LINKED_CAP_MOALHADDAR}; every other case waits {CAP})",
    )
    p.add_argument(
        "--only",
        choices=("speed", "torture", "soak"),
        help="one of the two measurements, or the torture test's soaks alone",
    )
    p.add_argument("--subjects", default="ours,gotenberg,moalhaddar")
    p.add_argument(
        "--image",
        default=os.environ.get("COMPARE_IMAGE", "docx-to-pdf:compare"),
        help="oxidestack/docx-to-pdf's image",
    )
    p.add_argument("--seed", type=int, default=None)
    p.add_argument("--tables", metavar="RESULTS_JSON", help="print the tables of a recorded run; measure nothing")
    a = p.parse_args()

    if a.tables:
        with open(a.tables) as f:
            print(render(json.load(f), os.path.dirname(os.path.abspath(a.tables))), end="")
        return 0

    began = time.time()
    stamp = datetime.datetime.now(datetime.UTC).strftime("%Y%m%dT%H%M%SZ")
    out = os.path.join(OUT_ROOT, stamp)
    os.makedirs(out, exist_ok=True)
    arch = docker_arch()
    subjects = [s for s in a.subjects.split(",") if s]
    notes = []
    if "moalhaddar" in subjects and arch not in ("x86_64", "amd64"):
        subjects.remove("moalhaddar")
        notes.append(
            f"moalhaddar/docx-to-pdf skipped: its only image is linux/amd64, and this Docker host is {arch}. "
            "Emulation is not a measurement; run the full mode on an amd64 host for it."
        )
        say(notes[-1])
    comparable = arch in ("x86_64", "amd64") and not a.quick
    if not comparable:
        notes.append(
            "The figures on record are from an amd64 Xeon Gold 6248R VM in the full mode; "
            + ("this run is the quick mode" if a.quick else f"this Docker host is {arch}")
            + ", so the two are shown side by side, not compared."
        )
    vm_cpus, vm_mem = bench.vm_cpus(), int(bench.sh("docker", "info", "--format", "{{.MemTotal}}", quiet=True))
    blocks = a.blocks or (1 if a.quick else 3)
    conversions = a.conversions or (12 if a.quick else None)  # None: the matrix's 52
    memory = "4g" if a.quick else "12g"
    cpus = "2-3" if vm_cpus >= 4 else "0"
    soak_n = 0 if a.quick else (3000 if a.soak is None else a.soak)
    soak_m = soak_n if a.soak_moalhaddar is None else a.soak_moalhaddar
    soak_m = min(soak_m, 1000) if a.soak_moalhaddar is None else soak_m
    seed = a.seed if a.seed is not None else int(time.time())
    meta = {
        "stamp": stamp,
        "mode": "quick" if a.quick else "full",
        # compare.sh passes the command it was given and the checkout's commit (`git describe --always --dirty`).
        "command": os.environ.get("COMPARE_COMMAND") or "compare.py " + " ".join(sys.argv[1:]),
        "commit": os.environ.get("COMPARE_COMMIT", "unknown"),
        "host": docker_host(),
        "machine": machine(),
        "docker_arch": arch,
        "vm_cpus": vm_cpus,
        "vm_memory": vm_mem,
        "image": a.image,
        "subjects": subjects,
        "blocks": blocks,
        "conversions": conversions or 52,
        "seed": seed,
        "torture_memory": memory,
        "torture_cpus": cpus,
        "soak": soak_n,
        "soak_moalhaddar": soak_m,
        "cap": CAP,
        "linked_cap_moalhaddar": a.linked_cap_moalhaddar,
        "notes": notes,
    }
    # The image's id and the commit it labels (org.opencontainers.image.revision; compare.sh builds it with the
    # Makefile's IMAGE_REVISION), read as the bench reads its service's: a reused or given image names its own source.
    ours = provenance.image(bench.sh, a.image)
    meta["image_id"] = ours["id"]
    meta["image_revision"] = ours.get("revision") or None
    say(json.dumps({k: v for k, v in meta.items() if k not in ("notes", "host", "machine")}))
    say("host:", host_line(meta))
    pull(subjects)
    meta["images"] = {
        NAMES["ours"]: pinned(a.image, meta["image_id"]),
        "k6": bench.K6_IMAGE,
        "bench tools": f"{bench.TOOLS_IMAGE}@{image_id(bench.TOOLS_IMAGE)}",
        "torture driver": torture.PYTHON,
    }
    if "gotenberg" in subjects:
        meta["images"]["Gotenberg"] = torture.GOTENBERG
    if "moalhaddar" in subjects:
        meta["images"]["moalhaddar/docx-to-pdf"] = bench.oracle_image()
    meta["images"] = {k: v for k, v in meta["images"].items() if v}
    patch_torture(a.image, memory, cpus)

    results = {"meta": meta}
    used_docs, used_torture = [], []
    if a.only in (None, "speed"):
        cells = [c for c in ("ours", "gotenberg", "g-norestart", "oracle") if CELL_SUBJECT[c] in subjects]
        bench_docs(SPEED_DOCS)
        used_docs += SPEED_DOCS
        speed_results, failures = speed(out, cells, blocks, conversions, a.image, seed)
        _, verdicts = speed_table(speed_results, failures, comparable)
        results["speed"] = {"cells": speed_results, "failures": failures, "seed": seed, "against_record": verdicts}

    if a.only in (None, "torture", "soak"):
        cases = a.only != "soak"
        rows = []
        cap = CAP
        bench_docs(["report-img30-10mb"] + (SOAK_DOCS if soak_n else []))
        used_docs += ["report-img30-10mb"] + (SOAK_DOCS if soak_n else [])
        bombs = [] if not cases else ["pages-500000p"] + ([] if a.quick else ["px-8x1024mpx", "tables-10000"])
        singles = (
            []
            if not cases
            else (
                ["png-g-32768", "empty", "random-1mb"]
                if a.quick
                else [
                    "png-g-32767",
                    "png-g-32768",
                    "png-rgb-18918",
                    "png-rgb-18919",
                    "empty",
                    "random-1mb",
                    "random-10mb",
                    "tdf38575",
                ]
            )
        )
        for name in bombs + singles:
            torture_doc(name)
        used_torture += bombs + singles
        # The soaks measure memory, not time: on a host with a vCPU free for each, they run alongside the rest on
        # their own vCPUs, as the record's run did; otherwise one after another at the end.
        soaks, threads, soak_rows = [], [], {}
        if soak_n and not a.quick:
            # oxidestack/docx-to-pdf as its soak on record ran: one instance (recycling off) and the
            # document limit raised as bench.py raises it, since three of the seven reports pass the shipped 10 MiB.
            soaks = [(s, OURS_SOAK_ENV if s == "ours" else [], s) for s in subjects]
            if "gotenberg" in subjects:
                soaks.insert(
                    soaks.index(("gotenberg", [], "gotenberg")) + 1,
                    ("gotenberg", ["LIBREOFFICE_RESTART_AFTER=0"], "g-norestart"),
                )
            lo, _, hi = cpus.partition("-")
            busy = set(range(int(lo), int(hi or lo) + 1))
            free = [c for c in range(vm_cpus) if c not in busy]
            meta["soak_alongside"] = cases and not a.soak_alone and len(free) >= len(soaks)
            if meta["soak_alongside"]:
                for k, (s, env, label) in enumerate(soaks):
                    n = soak_m if s == "moalhaddar" else soak_n
                    say(f"torture: soak of {n} → {CELL_LABEL.get(label, label)} alongside, on vCPU {free[k]}")

                    def run_soak(s=s, n=n, env=env, label=label, cpu=str(free[k]), port=18127 + k):
                        try:
                            soak_rows[label] = soak(out, s, n, env, label, cpu, port)
                        except Exception as error:  # noqa: BLE001
                            soak_rows[label] = {
                                "case": "soak",
                                "subject": label,
                                "kind": "soak",
                                "result": {"error": str(error)},
                                "cell": f"failed: {error}",
                            }

                    threads.append(threading.Thread(target=run_soak, daemon=True))
                    threads[-1].start()
        for name in bombs:
            for s in subjects:
                say(f"torture: {name} → {NAMES[s]}")
                rows.append(hostile(out, s, name, 0 if a.quick else 600, cap))
        for name in singles:
            for s in subjects:
                say(f"torture: {name} → {NAMES[s]}")
                rows.append(one(out, s, name, cap))
        if not a.quick:
            for s in subjects if cases else []:
                say(f"torture: pause → {NAMES[s]}")
                rows.append(pause(out, s))
            for s in subjects if cases else []:
                say(f"torture: egress → {NAMES[s]}")
                rows.append(egress(out, s, a.linked_cap_moalhaddar if s == "moalhaddar" else cap))
            for t in threads:
                t.join()
            for s, env, label in soaks:
                if label not in soak_rows:
                    n = soak_m if s == "moalhaddar" else soak_n
                    say(f"torture: soak of {n} → {CELL_LABEL.get(label, label)}")
                    soak_rows[label] = soak(out, s, n, env, label)
                rows.append(soak_rows[label])
        results["torture"] = rows

    meta["documents"] = documents(list(dict.fromkeys(used_docs)), used_torture)
    meta["elapsed_s"] = round(time.time() - began)
    with open(os.path.join(out, "results.json"), "w") as f:
        json.dump(results, f, indent=1, default=str)
    text = render(results)
    with open(os.path.join(out, "tables.md"), "w") as f:
        f.write(text)
    print(text)
    say(f"raw results: {os.path.relpath(out, ROOT)}/results.json; tables: {os.path.relpath(out, ROOT)}/tables.md")
    return 1 if results.get("speed", {}).get("failures") else 0


if __name__ == "__main__":
    sys.exit(main())
