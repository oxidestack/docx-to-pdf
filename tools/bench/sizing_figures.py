"""The sizing graphs: the quick plan's runs against sizing.py's model.

    python sizing_figures.py REPORT_DIR COEFFICIENTS.json OUT_DIR

1. throughput.png  throughput against vCPU (pool = vCPU) and against the pool at 4 and 6 vCPU, with the USL and C/D
2. memory.png      memory.peak against the pool, the fit, and step 2's limits that held or OOM-killed
3. disk.png        throughput per /tmp setting at 4 vCPU (step 3)
4. latency.png     the ramp's plateaus (p50, p95, p99) against the model at c_a² = 0 (k6's even arrivals) and 1, on the
                   platform the report's own runs give (sizing.platform_of: s1 and contention)
"""

import json
import os
import sys

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd
import sizing
import theme


def main(report, coefficients, out):
    theme.apply()
    os.makedirs(out, exist_ok=True)
    frame = pd.read_csv(os.path.join(report, "runs.csv"))
    k = sizing.load(coefficients)
    pool = frame[frame.run.str.contains("/1-pool/")]

    fig, ax = plt.subplots(1, 2, figsize=(11, 4.2))
    diag = pool[pool.cpus == pool.pool]
    ax[0].plot(diag.cpus, diag.throughput, "o", label="measured, pool = vCPU")
    c = np.linspace(1, 8, 50)
    u = k["usl_vcpu"]
    ax[0].plot(c, sizing.usl(c, u["gamma"], u["alpha"], u["beta"]), "-", label=f"USL: {u['gamma']:.3f}·C")
    ax[0].plot(
        c, c / k["cpu_seconds_per_conversion"]["mean"], "--", color=theme.MUTED, linewidth=1.2, label="CPU bound C/D"
    )
    hw = k["hw"]
    ax[0].plot(
        c,
        c / (hw["contention"] * hw["s1"]),
        ":",
        label=f"platform model C / ({hw['contention']:.2f} × {hw['s1']:.2f} s)",
    )
    ax[0].set_xlabel("vCPU")
    ax[0].set_ylabel("conversions/s")
    for cpus, style in ((4, "s"), (6, "^")):
        g = pool[pool.cpus == cpus]
        ax[1].plot(g.pool, g.throughput, style, label=f"{cpus} vCPU")
        ax[1].axvline(cpus, ls=":", alpha=0.4)
    ax[1].set_xlabel("pool")
    ax[1].set_ylabel("conversions/s")
    for a in ax:
        a.legend(fontsize=8)
        a.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(os.path.join(out, "throughput.png"), dpi=100)
    plt.close(fig)

    m = k["memory"]
    fig, ax = plt.subplots(figsize=(7, 4.2))
    ax.plot(pool.pool, pool.memory_peak / 2**20, "o", alpha=0.6, label="memory.peak at 16 GiB (step 1)")
    ax.plot(pool.pool, pool.anon_max / 2**20, ".", alpha=0.6, label="anonymous memory (step 1)")
    p = np.arange(1, 9)
    ax.plot(
        p,
        m["service_mib"] + m["per_instance_mib"] * p,
        "-",
        label=f"fit {m['service_mib']:.0f} + {m['per_instance_mib']:.0f}·pool MiB",
    )
    ax.plot(
        p,
        (m["service_mib"] + m["per_instance_mib"] * p) * (1 + m["headroom"]),
        "--",
        label=f"recommended limit, fit × {1 + m['headroom']:.2f}",
    )
    ax.plot(
        [h["pool"] for h in m["held"]],
        [h["limit_mib"] for h in m["held"]],
        "^",
        color=theme.SERIES[2],
        ms=8,
        linestyle="none",
        label="limit held",
    )
    ax.plot(
        [f["pool"] for f in m["failed"]],
        [f["limit_mib"] for f in m["failed"]],
        "x",
        color=theme.SERIES[1],
        ms=9,
        mew=2,
        linestyle="none",
        label="limit OOM-killed",
    )
    ax.set_xlabel("pool (= vCPU)")
    ax.set_ylabel("MiB")
    ax.legend(fontsize=8)
    ax.grid(alpha=0.3)
    fig.tight_layout()
    fig.savefig(os.path.join(out, "memory.png"), dpi=100)
    plt.close(fig)

    disk = frame[frame.run.str.contains("/3-disk/")]
    if len(disk):
        g = (
            disk.groupby("name")
            .throughput.agg(["mean", "min", "max"])
            .reindex(["disk", "tmpfs", "tracing-on", "disk120"])
        )
        labels = ["disk, uncapped", "tmpfs", "disk, tracing on", "disk, 120 MiB/s"]
        fig, ax = plt.subplots(figsize=(7, 3.6))
        ax.bar(labels, g["mean"], yerr=[g["mean"] - g["min"], g["max"] - g["mean"]], capsize=4)
        ax.set_ylabel("conversions/s, 4 vCPU, pool 4")
        ax.grid(alpha=0.3, axis="y")
        fig.tight_layout()
        fig.savefig(os.path.join(out, "disk.png"), dpi=100)
        plt.close(fig)

    ramp = [json.load(open(os.path.join(r, "summary.json"))) for r in frame[frame["mode"] == "ramp"].run]
    if ramp:
        s = ramp[0]
        levels = [pl for pl in s["plateaus"] if pl["latency"]]
        rates = [pl["rate"] for pl in levels]
        fig, ax = plt.subplots(figsize=(8, 4.5))
        x = np.linspace(0.05, max(rates) * 1.05, 60)
        for q, color in ((0.5, "C0"), (0.95, "C1"), (0.99, "C2")):
            ax.plot(
                rates,
                [pl["latency"][f"p{int(q * 100)}"] for pl in levels],
                "o",
                color=color,
                label=f"measured p{int(q * 100)}",
            )
            for ca2, style in ((0, "-"), (1, ":")):
                y = [sizing.response_quantile(k, s["cpus"], s["pool"], r, q, ca2) for r in x]
                ax.plot(
                    x,
                    [v if v is not None else np.nan for v in y],
                    style,
                    color=color,
                    alpha=0.7,
                    label=f"model p{int(q * 100)}, c_a²={ca2}",
                )
        for pl in levels:
            ax.annotate(
                str(pl["requests"]),
                (pl["rate"], pl["latency"]["p99"]),
                textcoords="offset points",
                xytext=(0, 6),
                fontsize=7,
                ha="center",
            )
        ax.set_ylim(0, 8)
        ax.set_xlabel("offered requests/s (4 vCPU, pool 4); numbers: requests per plateau")
        ax.set_ylabel("response time s")
        ax.legend(fontsize=7, ncol=2)
        ax.grid(alpha=0.3)
        fig.tight_layout()
        fig.savefig(os.path.join(out, "latency.png"), dpi=100)
        plt.close(fig)
    return 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:4]))
