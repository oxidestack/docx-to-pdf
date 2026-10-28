"""The bench's figures: a cell's replicates as mean ± σ (n), and two figures judged against each other at 2σ.

Standard library only, so bench.py (on the host) and analyze.py (in the tool image) share it. A cell is a matrix
row; its replicates are the run directories `r1..rN` under it. A figure is the mean over the replicates with σ, the
sample standard deviation, and n; two figures agree when |Δ| ≤ 2·√(σ₁²/n₁ + σ₂²/n₂), about 95 %. With three replicates
a side σ is itself uncertain: a disagreement is a reason for an interleaved, paired measurement, not a result.
"""

import os
import re
import statistics

# (label, summary.json's figure, decimals). Memory in MiB, writes in MB a conversion.
FIGURES = [
    ("conversions/s", "throughput", 3),
    ("CPU-s a conversion", "cpu", 3),
    ("p50 s", "p50", 2),
    ("p95 s", "p95", 2),
    ("p99 s", "p99", 2),
    ("memory.peak MiB", "peak", 0),
    ("written a conversion, MB", "written", 1),
]


def figure(summary, key):
    """One figure of an analyze.py summary.json, or None where the run did not record it."""
    latency = summary.get("latency") or {}
    raw = {
        "throughput": summary.get("throughput"),
        "cpu": summary.get("cpu_seconds_per_conversion"),
        "p50": latency.get("p50"),
        "p95": latency.get("p95"),
        "p99": latency.get("p99"),
        "peak": summary.get("memory_peak"),
        "written": summary.get("write_bytes_per_conversion"),
    }[key]
    if raw is None or raw != raw:  # NaN: no request in the window
        return None
    return raw / {"peak": 2**20, "written": 1e6}.get(key, 1)


def stats(values):
    """(mean, σ, n) of the replicates, σ the sample standard deviation (None for one replicate); None for none."""
    values = [v for v in values if v is not None]
    if not values:
        return None
    return statistics.mean(values), (statistics.stdev(values) if len(values) > 1 else None), len(values)


def fmt(v, digits=2):
    if v is None:
        return "—"
    if abs(v) >= 100:
        return f"{v:,.0f}".replace(",", " ")
    return f"{v:.{digits}f}"


def sigma(mean, sd, digits):
    """σ with the mean's decimals, and at least one significant digit."""
    if sd is None:
        return "—"
    places = digits if abs(mean) < 100 else 0
    while places < 4 and sd and round(sd, places) == 0:
        places += 1
    return f"{sd:.{places}f}"


def record(rec, digits=2):
    """`mean ± σ (n)`; one replicate says it has no σ."""
    if rec is None:
        return "—"
    mean, sd, n = rec
    if sd is None:
        return f"{fmt(mean, digits)} (1 run, no σ)"
    return f"{fmt(mean, digits)} ± {sigma(mean, sd, digits)} ({n})"


def spread(values, digits=2):
    return record(stats(values), digits)


def agreement(new, old):
    """(Δ, the 2σ bound) of two (mean, σ, n); (None, None) when either side has no σ."""
    (m1, s1, n1), (m2, s2, n2) = new, old
    if s1 is None or s2 is None:
        return None, None
    return m1 - m2, 2 * (s1**2 / n1 + s2**2 / n2) ** 0.5


REPLICATE = re.compile(r"r\d+")


def cell_of(run_dir):
    """The cell a run belongs to, as `<matrix>/<cell>`: the parent of a replicate directory `rN`; any other run (an
    ad-hoc cell, a probe phase) is a cell of its own, named by its last two components."""
    path = os.path.normpath(run_dir)
    if REPLICATE.fullmatch(os.path.basename(path)):
        path = os.path.dirname(path)
    return "/".join(path.split(os.sep)[-2:])


def cells(runs):
    """`runs`: (run directory, summary) pairs. Each cell: its replicates' run directories and every figure's
    (mean, σ, n), in the order the cells first appear."""
    grouped = {}
    for run_dir, summary in runs:
        grouped.setdefault(cell_of(run_dir), []).append((run_dir, summary))
    out = {}
    for name, members in grouped.items():
        out[name] = {
            "runs": [r for r, _ in members],
            "figures": {key: stats([figure(s, key) for _, s in members]) for _, key, _ in FIGURES},
        }
    return out


def cells_table(table):
    """The cells as a Markdown table: each figure mean ± σ (n)."""
    lines = [
        "| cell | replicates | " + " | ".join(label for label, _, _ in FIGURES) + " |",
        "|---" * (len(FIGURES) + 2) + "|",
    ]
    for name, cell in table.items():
        lines.append(
            f"| {name} | {len(cell['runs'])} | "
            + " | ".join(record(cell["figures"][key], digits) for _, key, digits in FIGURES)
            + " |"
        )
    return lines


def against_table(new, old):
    """Every figure of a cell both reports hold, judged at 2σ: the rows and how many agree, differ or lack a σ."""
    lines = ["| cell | figure | this report | the other | verdict |", "|---|---|---|---|---|"]
    count = {"agrees": 0, "differs": 0, "no σ": 0}
    for name in [n for n in new if n in old]:
        for label, key, digits in FIGURES:
            a, b = new[name]["figures"].get(key), old[name]["figures"].get(key)
            if a is None or b is None:
                continue
            a, b = tuple(a), tuple(b)
            delta, bound = agreement(a, b)
            if delta is None:
                verdict = "no σ"
                text = "no σ: one side has a single replicate"
            else:
                verdict = "agrees" if abs(delta) <= bound else "differs"
                relative = f"{delta / b[0] * 100:+.1f} %, " if b[0] else ""
                text = f"{verdict}: Δ {relative}{fmt(delta, digits + 1)}; 2σ {fmt(bound, digits + 1)}"
            count[verdict] += 1
            lines.append(f"| {name} | {label} | {record(a, digits)} | {record(b, digits)} | {text} |")
    return lines, count
