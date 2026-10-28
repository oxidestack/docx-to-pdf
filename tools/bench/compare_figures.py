"""The three quick-plan reports on one set of graphs, every series on every graph.

    python compare_figures.py OUT_DIR LABEL=REPORT_DIR=RUNS_DIR=COEFFICIENTS.json ...

One series a report, in the theme's fixed order (theme.py). Where the platforms' scales differ, the graph indexes them
to a common base (the share of step 1's saturation rate, the uncapped disk's throughput, the fit's memory).

1. compare-conversion.png  each document's conversion time alone (the ramp's lowest plateau)
2. compare-throughput.png  throughput against vCPU at pool = vCPU, and against the pool at 4 vCPU
3. compare-memory.png      memory.peak against the pool with each fit; step 2's limits over the fit, held or OOM
4. compare-latency.png     p50 and p95 against offered load as a share of step 1's saturation rate
5. compare-disk.png        tmpfs, tracing and a 120 MiB/s cap against the uncapped disk
"""

import glob
import json
import os
import sys

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import sizing
import theme


def load(spec):
    label, report, runs, coefficients = spec.split("=")
    frame = pd.read_csv(os.path.join(report, "runs.csv"))
    k = sizing.load(coefficients)
    ramp = None
    for path in sorted(glob.glob(os.path.join(runs, "4-ramp", "*", "r*", "summary.json"))):
        ramp = json.load(open(path))
    return {"label": label, "frame": frame, "step1": frame[frame.run.str.contains("/1-pool/")], "k": k, "ramp": ramp}


def label_end(ax, x, y, text, dy=0):
    """A direct label in text ink at a line's end; `dy` points apart where two ends meet."""
    ax.annotate(text, (x, y), xytext=(6, dy), textcoords="offset points", va="center", fontsize=9, color=theme.TEXT_2)


def conversion(series, out):
    docs = list(series[0]["k"]["service_time"]["by_document_s"])
    fig, ax = plt.subplots(figsize=(9, 4.2))
    width = 0.26
    x = np.arange(len(docs))
    for i, s in enumerate(series):
        values = [s["k"]["service_time"]["by_document_s"].get(d, np.nan) for d in docs]
        bars = ax.bar(
            x + (i - 1) * width,
            values,
            width - 0.03,
            color=theme.SERIES[i],
            label=s["label"],
            edgecolor=theme.SURFACE,
            linewidth=2,
        )
        for bar, v in zip(bars, values, strict=False):
            ax.annotate(
                f"{v:.2f}",
                (bar.get_x() + bar.get_width() / 2, v),
                xytext=(0, 3),
                textcoords="offset points",
                ha="center",
                fontsize=8,
                color=theme.TEXT_2,
            )
    ax.set_xticks(x, [d.replace("report-", "") for d in docs])
    ax.grid(axis="x", visible=False)
    ax.set_ylabel("seconds, one conversion alone")
    ax.set_title("A conversion alone, by document")
    ax.legend(loc="upper left")
    theme.note(
        fig,
        "The ramp's lowest plateau (30 % of saturation), mean per document. The ratio between platforms is "
        "the same for every document: the CPU's speed, s1.",
    )
    fig.savefig(os.path.join(out, "compare-conversion.png"))
    plt.close(fig)


def throughput(series, out):
    fig, ax = plt.subplots(1, 2, figsize=(12, 4.6))
    for i, s in enumerate(series):
        p = s["step1"]
        diag = p[p.cpus == p.pool].groupby("cpus").throughput.mean()
        color, marker = theme.SERIES[i], theme.MARKERS[i]
        ax[0].plot(diag.index, diag.values, marker=marker, color=color, label=s["label"])
        hw = s["k"]["hw"]
        c = np.linspace(1, 6, 20)
        ax[0].plot(c, c / (hw["contention"] * hw["s1"]), ":", color=color, linewidth=1.2, alpha=0.8)
        label_end(ax[0], diag.index[-1], diag.values[-1], f"{diag.values[-1] / diag.index[-1]:.2f}/s per vCPU")
        at4 = p[p.cpus == 4].groupby("pool").throughput.mean()
        ax[1].plot(at4.index, at4.values, marker=marker, color=color, label=s["label"])
    ax[0].set_xlabel("vCPU (pool = vCPU)")
    ax[0].set_ylabel("conversions/s")
    ax[0].set_title("Throughput scales with vCPU")
    ax[0].set_xlim(0.5, 7.4)
    ax[0].legend(loc="upper left")
    ax[1].axvline(4, color=theme.AXIS, linewidth=1)
    ax[1].annotate(
        "pool = vCPU",
        (4, ax[1].get_ylim()[0]),
        xytext=(4, 6),
        textcoords="offset points",
        fontsize=8,
        color=theme.MUTED,
    )
    ax[1].set_xticks(range(2, 7))
    ax[1].set_xlabel("pool at 4 vCPU")
    ax[1].set_ylabel("conversions/s")
    ax[1].set_title("A pool above vCPU gains nothing")
    theme.note(
        fig,
        "Step 1, closed load, mean of the replicates; dotted: the platform model min(pool, vCPU) / "
        "(contention × s1). moalhaddar/docx-to-pdf's 4-vCPU pool 6 hung at boot.",
    )
    fig.tight_layout()
    fig.savefig(os.path.join(out, "compare-throughput.png"))
    plt.close(fig)


def memory(series, out):
    fig, ax = plt.subplots(1, 2, figsize=(12, 4.6), gridspec_kw={"width_ratios": [1.2, 1]})
    pools = np.arange(1, 9)
    for i, s in enumerate(series):
        p, m = s["step1"], s["k"]["memory"]
        color, marker = theme.SERIES[i], theme.MARKERS[i]
        ax[0].scatter(
            p.pool,
            p.memory_peak / 2**20,
            marker=marker,
            color=color,
            s=28,
            alpha=0.75,
            edgecolors=theme.SURFACE,
            linewidths=1,
            label=s["label"],
        )
        fit = m["service_mib"] + m["per_instance_mib"] * pools
        ax[0].plot(pools, fit, color=color, linewidth=1.5)
        # oxidestack/docx-to-pdf on both platforms share a line: their labels go below and above it
        label_end(
            ax[0], pools[-1], fit[-1], f"{m['service_mib']:.0f} + {m['per_instance_mib']:.0f}·pool", dy=(-9, 9, 0)[i]
        )
        for row, marker_kind in ((m["held"], marker), (m["failed"], "x")):
            ratios = [h["limit_mib"] / h["fit_mib"] for h in row]
            ax[1].scatter(
                ratios,
                [len(series) - 1 - i + (j % 4 - 1.5) * 0.08 for j in range(len(ratios))],
                marker=marker_kind,
                color=color,
                s=60 if marker_kind == "x" else 44,
                linewidths=2 if marker_kind == "x" else 1,
                edgecolors=theme.SURFACE if marker_kind != "x" else None,
            )
    ax[0].set_xlabel("pool")
    ax[0].set_ylabel("MiB")
    ax[0].set_title("Memory peak against the pool")
    ax[0].set_xlim(0.5, 10.5)
    ax[0].legend(loc="upper left")
    ax[1].axvline(1.25, color=theme.TEXT_2, linewidth=1, linestyle="--")
    ax[1].annotate(
        "recommended limit: 1.25 × fit",
        (1.25, len(series) - 0.4),
        xytext=(-4, 0),
        textcoords="offset points",
        ha="right",
        fontsize=8,
        color=theme.TEXT_2,
    )
    ax[1].set_yticks(range(len(series)), [s["label"] for s in reversed(series)])
    ax[1].set_ylim(-0.6, len(series) - 0.2)
    ax[1].set_xlabel("memory limit / fit")
    ax[1].set_title("Step 2: held (marker) or OOM-killed (×)")
    ax[1].grid(axis="y", visible=False)
    theme.note(
        fig,
        "Step 1: memory.peak at 16 GiB, page cache included, every run. Step 2: each vCPU = pool at its "
        "peak + 25 %, − 10 %, − 25 %.",
    )
    fig.tight_layout()
    fig.savefig(os.path.join(out, "compare-memory.png"))
    plt.close(fig)


def latency(series, out):
    fig, ax = plt.subplots(1, 2, figsize=(12, 4.6), sharex=True)
    for i, s in enumerate(series):
        if not s["ramp"]:
            continue
        p = s["step1"]
        x_sat = p[(p.cpus == 4) & (p.pool == 4)].throughput.mean()
        levels = [pl for pl in s["ramp"]["plateaus"] if pl["latency"]]
        share = [pl["rate"] / x_sat * 100 for pl in levels]
        for a, q in zip(ax, ("p50", "p95"), strict=False):
            y = [pl["latency"][q] for pl in levels]
            a.plot(share, y, marker=theme.MARKERS[i], color=theme.SERIES[i], label=s["label"])
            label_end(a, share[-1], y[-1], f"{y[-1]:.1f} s")
    for a, q in zip(ax, ("p50", "p95"), strict=False):
        a.set_xlabel("offered load, % of step 1's saturation rate (4 vCPU, pool 4)")
        a.set_ylabel(f"{q}, seconds")
        a.set_ylim(bottom=0)
        a.set_xlim(20, 125)
    ax[0].set_title("Median response against load")
    ax[1].set_title("p95 response against load")
    ax[0].legend(loc="lower left")
    theme.note(
        fig,
        "Step 4: an open ramp of plateaus (k6, evenly spaced arrivals), each plateau's first 30 s dropped. "
        "Every platform held 110 % of its morning's step-1 rate: the cloud was faster in the afternoon.",
    )
    fig.tight_layout()
    fig.savefig(os.path.join(out, "compare-latency.png"))
    plt.close(fig)


def disk(series, out):
    settings = [("tmpfs", "/tmp on tmpfs"), ("tracing-on", "tracing on"), ("disk120", "disk capped 120 MiB/s")]
    fig, ax = plt.subplots(figsize=(10, 4.4))
    width = 0.26
    x = np.arange(len(settings))
    for i, s in enumerate(series):
        d = s["frame"][s["frame"].run.str.contains("/3-disk/")]
        base = d[d.name == "disk"].throughput.mean()
        values = [
            d[d.name == name].throughput.mean() / base * 100 - 100 if (d.name == name).any() else np.nan
            for name, _ in settings
        ]
        bars = ax.bar(
            x + (i - 1) * width,
            np.nan_to_num(values),
            width - 0.03,
            color=theme.SERIES[i],
            label=s["label"],
            edgecolor=theme.SURFACE,
            linewidth=2,
        )
        for bar, v in zip(bars, values, strict=False):
            text = "n/a" if np.isnan(v) else f"{v:+.1f} %"
            y = 0 if np.isnan(v) else v
            ax.annotate(
                text,
                (bar.get_x() + bar.get_width() / 2, y),
                xytext=(0, 4 if y >= 0 else -12),
                textcoords="offset points",
                ha="center",
                fontsize=8,
                color=theme.TEXT_2,
            )
    ax.axhline(0, color=theme.TEXT_2, linewidth=1)
    ax.set_xticks(x, [title for _, title in settings])
    ax.grid(axis="x", visible=False)
    ax.set_ylabel("throughput against /tmp on disk, %")
    ax.set_title("What the disk, tmpfs and tracing change (4 vCPU, pool 4)")
    ax.legend(loc="lower left")
    theme.note(
        fig,
        "Step 3, 200 conversions, 2 replicates. The cap costs less where a conversion is slower: the "
        "Xeon writes half the M1's MB/s. moalhaddar/docx-to-pdf has no tracing.",
    )
    fig.savefig(os.path.join(out, "compare-disk.png"))
    plt.close(fig)


def main(out, *specs):
    theme.apply()
    os.makedirs(out, exist_ok=True)
    series = [load(spec) for spec in specs]
    for draw in (conversion, throughput, memory, latency, disk):
        draw(series, out)
    return 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
