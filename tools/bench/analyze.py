"""The bench's analysis: run summaries, runs.csv and the graphs.

    python analyze.py DIR [DIR ...] [--out DIR]

Every run directory under the DIRs with a `done` file gets a summary.json and a timeseries.png beside its raw data
(re-made from the raw data each time). All runs then go into OUT/runs.csv, and OUT gets the cross-run graphs whose
data exists:

1. latency.png      p50/p95/p99 against offered load (open and ramp runs), achieved rate on a second axis
2. spectrum.png     the latency percentile spectrum per open run (HdrHistogram style)
3. scaling.png      throughput against pool size (closed runs) with the USL fit, the linear ideal, the CPU bound
4. timeseries.png   per run, the USE panel: CPU, pressure, instances, requests in flight, memory, I/O
5. memory.png       an instance's peak anonymous memory (sampled each second) against decoded megapixels and
                    against file size
6. time.png         conversion time against decoded megapixels and against pages, log-log
7. soak.png         per long run, each instance's RSS against its conversions
8. effects.png      allocation of variation of a 2^k screen (runs of matrix `screen`)
9. paired.png       tracing on - off and tmpfs - disk, mean differences with 95 % confidence intervals

A matrix cell's replicates (`<cell>/r1..rN`) are one cell: OUT/cells.md (printed too) and cells.json give each
figure as mean ± σ (n), σ the sample standard deviation, under the hosts and commits the runs recorded (run.json's
`provenance`); latency.png and scaling.png draw the same mean ± σ. `--against REPORT` judges each figure against the
same cell in another report: they agree when |Δ| ≤ 2·√(σ₁²/n₁ + σ₂²/n₂) (OUT/against.md, printed too).

Latency is the client's (k6, every request, so a slow answer is never left out of the record): the server's
histograms are coarse. Percentiles below 1 000 requests are marked: a p99 needs that many to mean anything (with
fewer, its 95 % confidence interval reaches the maximum).
"""

import argparse
import glob
import itertools
import json
import math
import os
import sys

import figures
import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import provenance
from scipy import optimize, stats

DOCS = "/data/docs/manifest.tsv"
FACTORS = ["docs", "pool", "cpus", "memory", "tmp", "tracing", "mode", "rate", "vus"]


def percentiles(values):
    if len(values) == 0:
        return {}
    q = np.percentile(values, [50, 90, 95, 99])
    return {
        "p50": q[0],
        "p90": q[1],
        "p95": q[2],
        "p99": q[3],
        "max": float(np.max(values)),
        "mean": float(np.mean(values)),
        "n": int(len(values)),
    }


def load_requests(path):
    frame = pd.read_csv(
        path,
        usecols=["metric_name", "timestamp", "metric_value", "status", "extra_tags"],
        dtype={"status": "string", "extra_tags": "string"},
    )
    frame = frame[frame.metric_name == "http_req_duration"].copy()
    frame["start"] = frame.timestamp / 1000.0
    frame["seconds"] = frame.metric_value / 1000.0
    frame["end"] = frame.start + frame.seconds
    frame["doc"] = frame.extra_tags.fillna("").str.extract(r"doc=([^&]+)", expand=False)
    return frame[["start", "end", "seconds", "status", "doc"]]


def load_samples(path):
    samples, clk = [], 100
    with open(path) as f:
        for line in f:
            s = json.loads(line)
            if "clk_tck" in s:
                clk = s["clk_tck"]
            else:
                samples.append(s)
    return samples, clk


def nearest(samples, t):
    return min(samples, key=lambda s: abs(s["t"] - t))


def metric(sample, name):
    """A series by its exact name and labels, or a bare name summed over its labels."""
    series = sample.get("metrics", {})
    if name in series:
        return series[name]
    return sum(v for k, v in series.items() if k.startswith(name + "{")) if "{" not in name else 0.0


def with_metrics(samples):
    return [s for s in samples if "metrics" in s and "error" not in s["metrics"]]


def io_total(sample, key, meta):
    """Bytes over the container's devices, less those stacked on another (bench.py preflight: LVM)."""
    stacked = set((meta.get("preflight") or {}).get("stacked_devices") or ())
    return sum(dev.get(key, 0) for name, dev in sample["cg"]["io"].items() if name not in stacked)


def service_pid(sample):
    for p in sample["procs"]:
        if (p["comm"] == "docx-to-pdf" and p["ppid"] == 1) or (p["comm"] == "java" and p["ppid"] == 0):
            return p["pid"]
    return None


def instances(sample):
    """The instance processes: oxidestack/docx-to-pdf's, the service's docx-to-pdf children;
    moalhaddar/docx-to-pdf's, its soffice.bin processes."""
    office = [p for p in sample["procs"] if p["comm"] == "soffice.bin"]
    if office:
        return office
    pid = service_pid(sample)
    return [p for p in sample["procs"] if p["ppid"] == pid and p["comm"] != "sh"]


def precision(ok, sizes=(20, 40, 80, 120, 160, 240, 320, 480)):
    """How precise a run of N conversions would have been (batch means): this run's successful
    requests, in order of their end, cut into back-to-back batches of N; for each N the relative standard deviation
    across batches of the throughput (N over the batch's span) and of the p95. A run needs about
    (1.96 · rsd / 0.05)² / N batches' worth... in short: the N whose rsd is under 2.5 % gives ±5 % at 95 %."""
    out = {}
    ends = np.sort(ok.end.values)
    seconds = ok.sort_values("end").seconds.values
    for n in sizes:
        k = (len(ends) - 1) // n
        if k < 3:
            continue
        rates = np.array([n / (ends[(i + 1) * n] - ends[i * n]) for i in range(k)])
        p95s = np.array([np.percentile(seconds[i * n + 1 : (i + 1) * n + 1], 95) for i in range(k)])
        out[str(n)] = {
            "batches": k,
            "throughput_rsd": float(rates.std(ddof=1) / rates.mean()),
            "p95_rsd": float(p95s.std(ddof=1) / p95s.mean()),
            "seconds_per_batch": float(n / rates.mean()),
        }
    return out


def duration_seconds(text):
    units = {"ms": 0.001, "s": 1, "m": 60, "h": 3600}
    total, number = 0.0, ""
    i = 0
    while i < len(text):
        if text[i].isdigit() or text[i] == ".":
            number += text[i]
            i += 1
            continue
        unit = "ms" if text[i : i + 2] == "ms" else text[i]
        total += float(number) * units[unit]
        number, i = "", i + len(unit)
    return total + (float(number) if number else 0)


def plateaus(meta, requests, settle=30.0):
    """A ramp's levels: consecutive stages at the same rate are a plateau (the first ramps to it, the rest hold it).
    Each plateau's requests are those that started inside it, after `settle` seconds, so the queue left by the step
    to it has drained or built up; their percentiles against the offered rate (graph 1)."""
    if meta["mode"] != "ramp" or not meta.get("stages") or "steady_started" not in meta:
        return []
    stages = [(float(r), duration_seconds(d)) for r, d in (s.split(":") for s in meta["stages"].split(","))]
    t0, out, i = requests.start.min(), [], 0
    clock = 0.0
    while i < len(stages):
        rate, start = stages[i][0], clock
        clock += stages[i][1]
        hold_from = clock if i > 0 and stages[i - 1][0] != rate else start
        i += 1
        while i < len(stages) and stages[i][0] == rate:
            clock += stages[i][1]
            i += 1
        lo, hi = t0 + hold_from + settle, t0 + clock
        inside = requests[(requests.start >= lo) & (requests.start < hi)]
        ok = inside[inside.status == "200"]
        done = requests[(requests.end >= lo) & (requests.end < hi) & (requests.status == "200")]
        out.append(
            {
                "rate": rate,
                "from_s": hold_from + settle,
                "to_s": clock,
                "requests": int(len(inside)),
                "errors": int(len(inside) - len(ok)),
                "throughput": len(done) / max(hi - lo, 1e-9),
                "latency": percentiles(ok.seconds.values),
            }
        )
    return out


def summarise(run_dir):
    meta = json.load(open(os.path.join(run_dir, "run.json")))
    requests = load_requests(os.path.join(run_dir, "steady", "k6.csv.gz"))
    samples, clk = load_samples(os.path.join(run_dir, "samples.jsonl"))
    k6_summary = json.load(open(os.path.join(run_dir, "steady", "k6-summary.json")))
    ok = requests[requests.status == "200"]
    w0, w1 = requests.start.min(), requests.end.max()
    window = max(w1 - w0, 1e-9)
    inside = [s for s in samples if w0 <= s["t"] <= w1] or samples
    a, b = nearest(samples, w0), nearest(samples, w1)
    span = max(b["t"] - a["t"], 1e-9)
    completions = len(ok)
    cpu_seconds = (b["cg"]["cpu"].get("usage_usec", 0) - a["cg"]["cpu"].get("usage_usec", 0)) / 1e6

    m_a, m_b = (
        (nearest(with_metrics(samples), w0), nearest(with_metrics(samples), w1)) if with_metrics(samples) else ({}, {})
    )

    def d(name):
        return metric(m_b, name) - metric(m_a, name)

    def dsum(prefix):
        """The change of every series of `prefix`, summed over its labels."""
        keys = {k for k in m_b.get("metrics", {}) if k == prefix or k.startswith(prefix + "{")}
        return sum(d(k) for k in keys)

    peak_count = d("docx_to_pdf_instance_memory_peak_bytes_count")
    starts = {
        k.split('reason="')[1].split('"')[0]: d(k)
        for k in m_b.get("metrics", {})
        if k.startswith("docx_to_pdf_instance_starts_total")
    }

    hwm = max((p["hwm"] or 0 for s in inside for p in instances(s)), default=0)
    # VmHWM and RSS count the file-backed pages too: LibreOffice's libraries, shared by every instance and not
    # charged to the container when another cgroup read them first. Anonymous memory is each instance's own.
    anon = max((p.get("anon") or 0 for s in inside for p in instances(s)), default=0)
    pids = {p["pid"] for s in inside for p in instances(s)}
    in_flight = [(s["t"], metric(s, "http_server_active_requests")) for s in with_metrics(inside)]
    slope = (
        float(np.polyfit([t for t, _ in in_flight], [v for _, v in in_flight], 1)[0]) if len(in_flight) > 2 else None
    )

    def pressure(res, kind):
        return (
            (
                b["cg"]["pressure"][res].get(kind, {}).get("total", 0)
                - a["cg"]["pressure"][res].get(kind, {}).get("total", 0)
            )
            / 1e6
            / span
        )

    dropped = k6_summary["metrics"].get("dropped_iterations", {}).get("values", {}).get("count", 0)

    prov = meta.get("provenance") or {}
    out = {
        "run": run_dir,
        "name": meta["name"],
        "commit": (prov.get("commit") or {}).get("commit"),
        "host_cpu": (prov.get("machine") or {}).get("cpu_model") or (prov.get("host") or {}).get("cpu_model"),
        "replicate": prov.get("replicate"),
        "subject": meta.get("subject", "ours"),
        "docs": ",".join(meta["docs"]),
        "pool": meta["pool"],
        "cpus": meta["cpus"],
        "memory": meta["memory"],
        "tmp": meta["tmp"],
        "tracing": meta["tracing"],
        "mode": meta["mode"],
        "rate": meta.get("rate"),
        "vus": meta.get("vus"),
        "io_bps": meta.get("io_bps"),
        "window_s": window,
        "requests": int(len(requests)),
        "completions": completions,
        "errors": int(len(requests) - completions),
        "error_rate": (len(requests) - completions) / len(requests) if len(requests) else None,
        "statuses": requests.status.value_counts().to_dict(),
        "dropped_iterations": dropped,
        "throughput": completions / window,
        "offered": meta.get("rate") if meta["mode"] == "open" else None,
        "latency": percentiles(ok.seconds.values),
        "latency_by_doc": {doc: percentiles(g.seconds.values) for doc, g in ok.groupby("doc")},
        "cpu_seconds_per_conversion": cpu_seconds / completions if completions else None,
        "cpu_utilisation": cpu_seconds / span / meta["cpus"],
        "throttled_s": (b["cg"]["cpu"].get("throttled_usec", 0) - a["cg"]["cpu"].get("throttled_usec", 0)) / 1e6,
        "memory_peak": samples[-1]["cg"]["memory_peak"],
        "memory_current_max": max(s["cg"]["memory_current"] or 0 for s in inside),
        "anon_max": max(s["cg"]["memory_stat"]["anon"] or 0 for s in inside),
        "shmem_max": max(s["cg"]["memory_stat"]["shmem"] or 0 for s in inside),
        "oom_kill": samples[-1]["cg"]["memory_events"].get("oom_kill", 0),
        "oom_killed": (meta.get("service_state") or {}).get("OOMKilled"),
        "read_bytes_per_conversion": (io_total(b, "rbytes", meta) - io_total(a, "rbytes", meta)) / completions
        if completions
        else None,
        "write_bytes_per_conversion": (io_total(b, "wbytes", meta) - io_total(a, "wbytes", meta)) / completions
        if completions
        else None,
        "write_rate": (io_total(b, "wbytes", meta) - io_total(a, "wbytes", meta)) / span,
        "pressure_cpu_some": pressure("cpu", "some"),
        "pressure_memory_some": pressure("memory", "some"),
        "pressure_memory_full": pressure("memory", "full"),
        "pressure_io_some": pressure("io", "some"),
        "vm_cpu_some_avg10_max": max(s["vm_pressure"]["cpu"]["some"]["avg10"] for s in inside),
        "instance_hwm_max": hwm,
        "instance_anon_max": anon,
        "instances_seen": len(pids),
        "instance_starts": starts,
        "server_peak_mean": d("docx_to_pdf_instance_memory_peak_bytes_sum") / peak_count if peak_count else None,
        "server_conversion_mean_s": (
            dsum("docx_to_pdf_conversion_duration_seconds_sum") / dsum("docx_to_pdf_conversion_duration_seconds_count")
        )
        if dsum("docx_to_pdf_conversion_duration_seconds_count")
        else None,
        "server_wait_mean_s": (
            d('docx_to_pdf_pool_wait_duration_seconds_sum{outcome="acquired"}')
            / d('docx_to_pdf_pool_wait_duration_seconds_count{outcome="acquired"}')
        )
        if d('docx_to_pdf_pool_wait_duration_seconds_count{outcome="acquired"}')
        else None,
        "backlog_slope_per_s": slope,
        "vm_warning": meta.get("warning"),
        "host_load_start": (meta.get("preflight", {}).get("host_loadavg") or [None])[0],
        "host_load_end": (meta.get("host_loadavg_end") or [None])[0],
    }
    if meta["mode"] == "closed" and meta.get("conversions"):
        # A count run's ends: its first round finishes slower (the quick plan's calibration: the first 2·VUs conversions
        # 4-6 % below the rest), and its last round drains with fewer than VUs in flight. Throughput is counted between
        # the 2·VUs-th completion and the last request's start, where every client is busy.
        vus = meta.get("vus") or meta["pool"] + 2
        ends, last_start = np.sort(ok.end.values), requests.start.max()
        if len(ends) > 2 * vus and last_start > ends[2 * vus]:
            lo = ends[2 * vus]
            out["throughput_raw"] = out["throughput"]
            out["throughput"] = ((ends > lo) & (ends <= last_start)).sum() / (last_start - lo)
    out["precision_by_conversions"] = precision(ok)
    out["plateaus"] = plateaus(meta, requests)
    json.dump(out, open(os.path.join(run_dir, "summary.json"), "w"), indent=1, default=float)
    timeseries(run_dir, meta, samples, clk, requests)
    return out, requests


def timeseries(run_dir, meta, samples, clk, requests):
    """The USE panel of one run (graph 4), and each instance's RSS against its conversions (graph 7)."""
    t0 = samples[0]["t"]
    t = np.array([s["t"] - t0 for s in samples])
    cpu = np.array([s["cg"]["cpu"].get("usage_usec", 0) for s in samples]) / 1e6
    util = np.diff(cpu) / np.maximum(np.diff(t), 1e-9) / meta["cpus"] * 100
    psi = np.array([s["cg"]["pressure"]["cpu"]["some"]["avg10"] for s in samples])
    mem = np.array([(s["cg"]["memory_current"] or 0) for s in samples]) / 2**20
    shmem = np.array([(s["cg"]["memory_stat"]["shmem"] or 0) for s in samples]) / 2**20
    wb = np.array([io_total(s, "wbytes", meta) for s in samples]) / 2**20
    wrate = np.diff(wb) / np.maximum(np.diff(t), 1e-9)
    ms = with_metrics(samples)
    mt = np.array([s["t"] - t0 for s in ms])
    busy = [metric(s, 'docx_to_pdf_pool_instances{state="busy"}') for s in ms]
    active = [metric(s, "http_server_active_requests") for s in ms]
    fig, ax = plt.subplots(4, 1, figsize=(10, 11), sharex=True)
    ax[0].plot(t[1:], util, label="CPU % of the cpuset")
    ax[0].plot(t, psi, label="cpu pressure some avg10 %")
    ax[0].set_ylim(0, 110)
    ax[1].plot(mt, busy, label="instances busy")
    ax[1].plot(mt, active, label="requests in flight")
    ax[2].plot(t, mem, label="memory.current MiB")
    ax[2].plot(t, shmem, label="shmem (tmpfs) MiB")
    for pid in sorted({p["pid"] for s in samples for p in instances(s)}):
        rss = [
            (s["t"] - t0, p["rss"] / 2**20)
            for s in samples
            for p in instances(s)
            if p["pid"] == pid and p["rss"] is not None
        ]
        ax[2].plot(*zip(*rss, strict=False), lw=0.8, alpha=0.6, label=f"instance {pid} RSS")
    ax[3].plot(t[1:], wrate, label="disk write MiB/s")
    if len(requests):
        r = requests.sort_values("end")
        ax[3].plot(r.end - t0, r.seconds, ".", ms=2, alpha=0.4, label="request latency s")
    for a in ax:
        a.legend(fontsize=7, loc="upper left")
        a.grid(alpha=0.3)
        for _key in ("warmup_started", "steady_started", "steady_ended"):
            pass
    ax[-1].set_xlabel("seconds since the sampler started")
    fig.suptitle(
        f"{meta['name']}: {', '.join(meta['docs'][:3])}{'…' if len(meta['docs']) > 3 else ''}, "
        f"pool {meta['pool']}, {meta['cpus']} cpu, {meta['tmp']}, tracing {meta['tracing']}",
        fontsize=9,
    )
    fig.tight_layout()
    fig.savefig(os.path.join(run_dir, "timeseries.png"), dpi=110)
    plt.close(fig)


def flat(summary):
    row = {k: v for k, v in summary.items() if not isinstance(v, (dict, list))}
    for k, v in summary["latency"].items():
        row[f"lat_{k}"] = v
    return row


def manifest():
    if not os.path.exists(DOCS):
        return {}
    return pd.read_csv(DOCS, sep="\t").set_index("name").to_dict("index")


def usl(p, gamma, alpha, beta):
    return gamma * p / (1 + alpha * (p - 1) + beta * p * (p - 1))


def plot_latency(frame, out, summaries=()):
    """Open runs, one point each, and every ramp's plateaus, one point each."""
    levels = [
        {
            **{k: s[k] for k in ("docs", "pool", "cpus", "tmp", "tracing")},
            "rate": p["rate"],
            "throughput": p["throughput"],
            "requests": p["requests"],
            **{f"lat_{q}": p["latency"].get(q) for q in ("p50", "p95", "p99")},
        }
        for s in summaries
        for p in s.get("plateaus", [])
        if p["latency"]
    ]
    open_runs = pd.concat([frame[frame["mode"] == "open"], pd.DataFrame(levels)], ignore_index=True)
    if open_runs.empty:
        return
    groups = open_runs.groupby(["docs", "pool", "cpus", "tmp", "tracing"])
    fig, axes = plt.subplots(len(groups), 1, figsize=(9, 3.5 * len(groups)), squeeze=False)
    for ax, (key, g) in zip(axes[:, 0], groups, strict=False):
        # Mean ± σ over the runs and plateaus at each offered rate.
        by = g.groupby("rate")
        n = by.size()
        sd = by.std(numeric_only=True).fillna(0).reset_index()
        g = by.mean(numeric_only=True).reset_index()
        for q in ("p50", "p95", "p99"):
            ax.errorbar(g.rate, g[f"lat_{q}"], yerr=sd[f"lat_{q}"], fmt="o-", capsize=3, label=f"{q} mean ± σ")
        span = f"{n.min()}" if n.min() == n.max() else f"{n.min()}-{n.max()}"
        ax.text(0.99, 0.02, f"n {span} a rate", transform=ax.transAxes, ha="right", fontsize=7)
        ax.set_yscale("log")
        ax.set_xlabel("offered requests/s")
        ax.set_ylabel("latency s")
        twin = ax.twinx()
        twin.plot(g.rate, g.throughput, "k--", alpha=0.5, label="achieved/s")
        twin.set_ylabel("achieved requests/s")
        ax.set_title(f"{key[0]}: pool {key[1]}, {key[2]} cpu, {key[3]}, tracing {key[4]}", fontsize=9)
        ax.legend(fontsize=7, loc="upper left")
        ax.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(os.path.join(out, "latency.png"), dpi=110)
    plt.close(fig)


def plot_spectrum(frame, requests, out):
    open_runs = frame[frame["mode"].isin(["open", "closed"])]
    if open_runs.empty:
        return
    fig, ax = plt.subplots(figsize=(9, 5))
    for _, row in open_runs.iterrows():
        values = np.sort(requests[row["run"]].query("status == '200'").seconds.values)
        if len(values) < 10:
            continue
        q = np.arange(1, len(values) + 1) / (len(values) + 1)
        ax.plot(
            1 / (1 - q),
            values,
            lw=1,
            label=f"{row['docs'][:24]} p{row['pool']} {row['mode']} {row['rate'] or row['vus'] or ''}",
        )
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xticks([2, 10, 100, 1000, 10000])
    ax.set_xticklabels(["50%", "90%", "99%", "99.9%", "99.99%"])
    ax.set_xlabel("percentile")
    ax.set_ylabel("latency s")
    ax.legend(fontsize=6)
    ax.grid(alpha=0.3, which="both")
    fig.tight_layout()
    fig.savefig(os.path.join(out, "spectrum.png"), dpi=110)
    plt.close(fig)


def plot_scaling(frame, out):
    closed = frame[frame["mode"] == "closed"]
    groups = [(k, g) for k, g in closed.groupby(["docs", "tmp", "tracing"]) if g.pool.nunique() >= 3]
    if not groups:
        return {}
    fits = {}
    fig, axes = plt.subplots(len(groups), 2, figsize=(11, 4 * len(groups)), squeeze=False)
    for (ax, ax2), (key, g) in zip(axes, groups, strict=False):
        for cpus_equal, style in ((True, "o"), (False, "s")):
            sub = g[(g.cpus == g.pool) == cpus_equal]
            if sub.empty:
                continue
            # Mean ± σ over each cell's replicates (one pool and cpuset), n in the legend.
            m = sub.assign(per_cpu=sub.throughput / sub.cpus).groupby(["pool", "cpus"])[["throughput", "per_cpu"]]
            mean, sd, n = m.mean().reset_index(), m.std().fillna(0).reset_index(), m.count().throughput
            label = "cpuset = pool" if cpus_equal else "cpuset fixed"
            label += f", mean ± σ (n {n.min()}{f'-{n.max()}' if n.max() != n.min() else ''})"
            ax.errorbar(mean.pool, mean.throughput, yerr=sd.throughput, fmt=style, capsize=3, alpha=0.8, label=label)
            ax2.errorbar(mean.pool, mean.per_cpu, yerr=sd.per_cpu, fmt=style, capsize=3, alpha=0.8)
        sub = g[g.cpus == g.pool]
        if sub.pool.nunique() >= 3:
            try:
                (gamma, alpha, beta), cov = optimize.curve_fit(
                    usl,
                    sub.pool,
                    sub.throughput,
                    p0=[sub.throughput.min(), 0.01, 0.001],
                    bounds=([0, 0, 0], [np.inf, 1, 1]),
                )
                p = np.linspace(1, max(sub.pool.max(), 8), 100)
                ax.plot(p, usl(p, gamma, alpha, beta), "-", label=f"USL α={alpha:.3f} β={beta:.4f}")
                ax.plot(p, gamma * p, ":", label="linear")
                pmax = math.sqrt((1 - alpha) / beta) if beta > 0 else None
                fits[" ".join(map(str, key))] = {"gamma": gamma, "alpha": alpha, "beta": beta, "p_max": pmax}
            except (RuntimeError, ValueError):
                pass
        demand = g.cpu_seconds_per_conversion.median()
        if demand:
            p = np.array(sorted(g.cpus.unique()))
            ax.plot(p, p / demand, "k--", alpha=0.4, label="CPU bound C/D")
        ax.set_xlabel("pool size")
        ax.set_ylabel("conversions/s")
        ax.set_title(f"{key[0]}: {key[1]}, tracing {key[2]}", fontsize=9)
        ax.legend(fontsize=7)
        ax.grid(alpha=0.3)
        ax2.set_xlabel("pool size")
        ax2.set_ylabel("conversions/s per vCPU")
        ax2.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(os.path.join(out, "scaling.png"), dpi=110)
    plt.close(fig)
    return fits


def per_doc(frame, docs):
    """Single-document runs at pool 1 (the size series), joined to the manifest."""
    single = frame[(~frame.docs.str.contains(",")) & (frame.pool == 1)].copy()
    if single.empty or not docs:
        return single
    for col in ("bytes", "decoded_mpx", "pages_hint", "class"):
        single[col] = single.docs.map(lambda d, col=col: docs.get(d, {}).get(col))
    return single


def plot_memory(single, out):
    if single.empty or "decoded_mpx" not in single:
        return {}
    fig, ax = plt.subplots(1, 2, figsize=(11, 4.5))
    peak = single.instance_anon_max.where(single.instance_anon_max > 0, single.instance_hwm_max) / 2**20
    for cls, g in single.assign(peak=peak).groupby("class"):
        ax[0].plot(g.decoded_mpx.astype(float) * 4, g.peak, "o", label=cls)
        ax[1].plot(g["bytes"].astype(float) / 1e6, g.peak, "o", label=cls)
    fit = {}
    data = single.assign(peak=peak).dropna(subset=["decoded_mpx", "peak"])
    if len(data) >= 3:
        x = data.decoded_mpx.astype(float) * 4
        slope, intercept, r, *_ = stats.linregress(x, data.peak)
        xs = np.linspace(0, x.max(), 50)
        ax[0].plot(xs, intercept + slope * xs, "k--", alpha=0.5, label=f"{intercept:.0f} + {slope:.2f}·MB (r={r:.2f})")
        fit = {"intercept_mib": intercept, "mib_per_decoded_mb": slope, "r": r}
    ax[0].set_xlabel("decoded image MB (megapixels × 4)")
    ax[1].set_xlabel("file MB")
    for a in ax:
        a.set_ylabel("instance peak anonymous RSS MiB")
        a.legend(fontsize=7)
        a.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(os.path.join(out, "memory.png"), dpi=110)
    plt.close(fig)
    return fit


def plot_time(single, out):
    if single.empty or "decoded_mpx" not in single:
        return
    fig, ax = plt.subplots(1, 2, figsize=(11, 4.5))
    for cls, g in single.groupby("class"):
        for a, x in ((ax[0], g.decoded_mpx.astype(float)), (ax[1], g.pages_hint.astype(float))):
            if x.notna().any():
                a.errorbar(
                    x,
                    g.lat_p50,
                    yerr=[np.zeros(len(g)), (g.lat_p99 - g.lat_p50).clip(lower=0)],
                    fmt="o",
                    label=cls,
                    capsize=3,
                )
    ax[0].set_xlabel("decoded megapixels")
    ax[1].set_xlabel("pages")
    for a in ax:
        a.set_xscale("log")
        a.set_yscale("log")
        a.set_ylabel("conversion latency s (p50, bar to p99)")
        a.legend(fontsize=7)
        a.grid(alpha=0.3, which="both")
    fig.tight_layout()
    fig.savefig(os.path.join(out, "time.png"), dpi=110)
    plt.close(fig)


def plot_soak(frame, out):
    long = frame[frame.window_s >= 1800]
    if long.empty:
        return
    fig, axes = plt.subplots(len(long), 1, figsize=(9, 3.5 * len(long)), squeeze=False)
    for ax, (_, row) in zip(axes[:, 0], long.iterrows(), strict=False):
        samples, _ = load_samples(os.path.join(row["run"], "samples.jsonl"))
        ms = with_metrics(samples)
        conversions = np.array(
            [sum(v for k, v in s["metrics"].items() if k.startswith("docx_to_pdf_conversions_total")) for s in ms]
        )
        for pid in sorted({p["pid"] for s in ms for p in instances(s)}):
            pts = [
                (c, p["rss"] / 2**20)
                for s, c in zip(ms, conversions, strict=False)
                for p in instances(s)
                if p["pid"] == pid
            ]
            ax.plot(*zip(*pts, strict=False), lw=0.8, label=str(pid))
        ax.set_xlabel("conversions by the service")
        ax.set_ylabel("instance RSS MiB")
        ax.set_title(f"{row['name']} {row['docs'][:40]}", fontsize=9)
        ax.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(os.path.join(out, "soak.png"), dpi=110)
    plt.close(fig)


def screen_effects(frame, out):
    """Allocation of variation of a 2^k r design (Jain ch. 18) on log throughput and log p99."""
    screen = frame[frame.run.str.contains("/screen/")]
    factors = [f for f in ("docs", "pool", "tracing", "tmp") if screen[f].nunique() == 2] if not screen.empty else []
    if len(factors) < 2:
        return {}
    results = {}
    fig, axes = plt.subplots(1, 2, figsize=(12, 4.5))
    for ax, response in zip(axes, ("throughput", "lat_p99"), strict=False):
        data = screen.dropna(subset=[response]).copy()
        levels = {f: sorted(data[f].unique(), key=str) for f in factors}
        signs = pd.DataFrame({f: np.where(data[f] == levels[f][1], 1.0, -1.0) for f in factors}, index=data.index)
        y = np.log(data[response].astype(float))
        terms = {}
        for k in range(1, len(factors) + 1):
            for combo in itertools.combinations(factors, k):
                terms["×".join(combo)] = signs[list(combo)].prod(axis=1)
        n = len(y)
        sst = float(((y - y.mean()) ** 2).sum())
        shares = {name: float(n * (np.dot(x, y) / n) ** 2) / sst * 100 if sst else 0 for name, x in terms.items()}
        cells = data.groupby(factors)[response].apply(lambda v: np.log(v.astype(float)))
        sse = float(sum(((g - g.mean()) ** 2).sum() for _, g in data.assign(y=y).groupby(factors).y))
        shares["error"] = sse / sst * 100 if sst else 0
        results[response] = {"levels": {f: [str(v) for v in levels[f]] for f in factors}, "share_percent": shares}
        names = sorted(shares, key=shares.get, reverse=True)
        ax.barh(names[::-1], [shares[n] for n in names[::-1]])
        ax.set_xlabel(f"% of variation in log {response}")
        ax.grid(alpha=0.3)
        del cells
    fig.tight_layout()
    fig.savefig(os.path.join(out, "effects.png"), dpi=110)
    plt.close(fig)
    return results


def paired(frame, out):
    """tracing on - off and tmpfs - disk on matched runs: relative difference, mean and 95 % CI."""
    rows = []
    for factor, (low, high) in (("tracing", ("off", "on")), ("tmp", ("disk", "tmpfs"))):
        others = [f for f in ("docs", "pool", "cpus", "memory", "tmp", "tracing", "mode", "rate", "vus") if f != factor]
        keyed = frame.assign(key=frame[others].apply(lambda c: c.map(str)).agg("|".join, axis=1))
        for response in ("throughput", "lat_p99", "cpu_seconds_per_conversion"):
            diffs = []
            for _, g in keyed.groupby("key"):
                lo, hi = g[g[factor] == low][response].dropna(), g[g[factor] == high][response].dropna()
                if len(lo) and len(hi) and lo.mean():
                    diffs.append((hi.mean() - lo.mean()) / lo.mean() * 100)
            if len(diffs) >= 2:
                mean = float(np.mean(diffs))
                half = float(stats.t.ppf(0.975, len(diffs) - 1) * np.std(diffs, ddof=1) / math.sqrt(len(diffs)))
                rows.append(
                    {
                        "factor": f"{factor} {high} - {low}",
                        "response": response,
                        "mean_percent": mean,
                        "ci95_percent": half,
                        "pairs": len(diffs),
                    }
                )
    if not rows:
        return []
    fig, ax = plt.subplots(figsize=(8, 0.6 * len(rows) + 1.5))
    labels = [f"{r['factor']}: {r['response']} (n={r['pairs']})" for r in rows]
    ax.errorbar(
        [r["mean_percent"] for r in rows], range(len(rows)), xerr=[r["ci95_percent"] for r in rows], fmt="o", capsize=4
    )
    ax.axvline(0, color="k", lw=0.8)
    ax.set_yticks(range(len(rows)))
    ax.set_yticklabels(labels, fontsize=8)
    ax.set_xlabel("difference, % of the low level (95 % CI)")
    ax.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(os.path.join(out, "paired.png"), dpi=110)
    plt.close(fig)
    return rows


def provenance_lines(run_dirs):
    """The distinct hosts and commits the runs recorded, each with how many runs."""
    hosts, commits = {}, {}
    for run_dir in run_dirs:
        try:
            prov = json.load(open(os.path.join(run_dir, "run.json"))).get("provenance") or {}
        except (OSError, ValueError):
            prov = {}
        line = provenance.host_line(prov.get("host"), prov.get("machine")) if prov else "not recorded by this run"
        hosts[line] = hosts.get(line, 0) + 1
        c = prov.get("commit") or {}
        key = (c.get("commit") or "not recorded") + (" (with local changes)" if c.get("dirty") else "")
        commits[key] = commits.get(key, 0) + 1
    return [f"- Host ({n} runs): {h}." for h, n in hosts.items()] + [
        f"- Commit ({n} runs): `{c}`." for c, n in commits.items()
    ]


def report_cells(summaries, out, against=None):
    """cells.md and cells.json: each cell's figures as mean ± σ (n); against.md when `against` names a report."""
    table = figures.cells([(s["run"], s) for s in summaries])
    lines = [
        "# The bench's cells",
        "",
        "Each figure is the mean ± σ (n) over a cell's replicates, σ the sample standard deviation.",
        "",
        *provenance_lines([s["run"] for s in summaries]),
        "",
        *figures.cells_table(table),
        "",
    ]
    with open(os.path.join(out, "cells.md"), "w") as f:
        f.write("\n".join(lines))
    json.dump(table, open(os.path.join(out, "cells.json"), "w"), indent=1)
    print("\n".join(lines))
    if not against:
        return
    path = os.path.join(against, "cells.json")
    if not os.path.exists(path):
        print(f"--against {against}: no cells.json (make bench-report there first)", file=sys.stderr)
        return
    rows, count = figures.against_table(table, json.load(open(path)))
    lines = [
        f"# Against {against}",
        "",
        "Two figures agree when |Δ| ≤ 2·√(σ₁²/n₁ + σ₂²/n₂), about 95 %; with few replicates a difference is a reason "
        "for an interleaved, paired measurement, not a result.",
        "",
        *rows,
        "",
        f"{count['agrees']} agree, {count['differs']} differ, {count['no σ']} without σ.",
        "",
    ]
    with open(os.path.join(out, "against.md"), "w") as f:
        f.write("\n".join(lines))
    print("\n".join(lines))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("dirs", nargs="+")
    parser.add_argument("--out", default="/data/report")
    parser.add_argument("--against", help="another report's directory: its cells.json, each figure judged at 2σ")
    args = parser.parse_args()
    run_dirs = sorted({os.path.dirname(p) for d in args.dirs for p in glob.glob(f"{d}/**/done", recursive=True)})
    if not run_dirs:
        print("no finished runs", file=sys.stderr)
        return 1
    os.makedirs(args.out, exist_ok=True)
    summaries, requests = [], {}
    for run_dir in run_dirs:
        try:
            summary, reqs = summarise(run_dir)
        except (OSError, KeyError, ValueError) as error:
            print(f"{run_dir}: skipped ({error})", file=sys.stderr)
            continue
        summaries.append(summary)
        requests[run_dir] = reqs
        lat = summary["latency"]
        note = "" if lat.get("n", 0) >= 1000 else " (p99 under 1 000 requests)"
        print(
            f"{run_dir}: {summary['throughput']:.3f}/s, p50 {lat.get('p50', float('nan')):.2f} s, "
            f"p99 {lat.get('p99', float('nan')):.2f} s{note}",
            file=sys.stderr,
        )
    frame = pd.DataFrame([flat(s) for s in summaries])
    frame.to_csv(os.path.join(args.out, "runs.csv"), index=False)
    report_cells(summaries, args.out, args.against)
    docs = manifest()
    single = per_doc(frame, docs)
    plot_latency(frame, args.out, summaries)
    plot_spectrum(frame, requests, args.out)
    analysis = {
        "usl": plot_scaling(frame, args.out),
        "memory_fit": plot_memory(single, args.out),
        "screen": screen_effects(frame, args.out),
        "paired": paired(frame, args.out),
    }
    plot_time(single, args.out)
    plot_soak(frame, args.out)
    json.dump(analysis, open(os.path.join(args.out, "analysis.json"), "w"), indent=1, default=float)
    print(f"{len(frame)} runs → {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
