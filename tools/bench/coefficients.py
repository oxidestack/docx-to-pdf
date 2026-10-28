"""The sizing coefficients from the quick plan's runs, for sizing.py and `bench.py size`.

    python coefficients.py REPORT_DIR RUNS_DIR --out FILE.json --platform TEXT --workload NAME

REPORT_DIR holds runs.csv of `bench.py report` over the quick plan; RUNS_DIR is .bench/runs. Reads:

- 1-pool: throughput per (vCPU, pool), the CPU-seconds per conversion, the memory against the pool, disk bytes;
- 2-memory: the smallest limit that held step 1's throughput, hence the headroom over the fit;
- 4-ramp: the service time's shape S / E[S] at the lowest plateau, where hardly a request waits.
"""

import argparse
import datetime
import glob
import json
import os

import analyze
import numpy as np
import pandas as pd
from scipy import optimize

MIB = 2**20


def usl(n, gamma, alpha, beta):
    return gamma * n / (1 + alpha * (n - 1) + beta * n * (n - 1))


def service_shape(runs_dir):
    """S / E[S] at the quantiles 0, 0.01, ..., 1 from the lowest plateau of the 4-vCPU ramp."""
    for summary_path in sorted(glob.glob(f"{runs_dir}/4-ramp/*/r*/summary.json")):
        summary = json.load(open(summary_path))
        if not summary.get("plateaus"):
            continue
        low = summary["plateaus"][0]
        run = os.path.dirname(summary_path)
        requests = analyze.load_requests(os.path.join(run, "steady", "k6.csv.gz"))
        t0 = requests.start.min()
        inside = requests[
            (requests.start >= t0 + low["from_s"]) & (requests.start < t0 + low["to_s"]) & (requests.status == "200")
        ]
        s = inside.seconds.values
        shape = np.percentile(s, np.arange(0, 101)) / s.mean()
        meta = json.load(open(os.path.join(run, "run.json")))
        return {
            "shape": [round(float(v), 4) for v in shape],
            "cv2": float((s.std() / s.mean()) ** 2),
            "cpus": meta["cpus"],
            "pool": meta["pool"],
            "mean_s_at_low_load": float(s.mean()),
            "requests": int(len(s)),
            "from": run,
            "by_document_s": inside.groupby("doc").seconds.mean().round(3).to_dict(),
        }
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("report")
    parser.add_argument("runs")
    parser.add_argument("--out", required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--workload", required=True)
    parser.add_argument("--max-document-mib", type=float, default=32)
    parser.add_argument("--run", default="the quick plan", help="the run that measured them")
    args = parser.parse_args()

    frame = pd.read_csv(os.path.join(args.report, "runs.csv"))
    pool = frame[frame.run.str.contains("/1-pool/")]
    memory = frame[frame.run.str.contains("/2-memory/")]

    throughput = {
        str(c): {str(p): [round(float(x), 4) for x in g.throughput] for p, g in cg.groupby("pool")}
        for c, cg in pool.groupby("cpus")
    }
    means = pool.groupby(["cpus", "pool"]).throughput.mean()
    best = {c: means[c].idxmax() for c in means.index.get_level_values(0).unique()}
    xs = np.array([float(c) for c in best])
    ys = np.array([means[c][best[c]] for c in best])
    (gamma, alpha, beta), _ = optimize.curve_fit(
        usl, xs, ys, p0=[ys[0], 0.01, 0.0001], bounds=([0, 0, 0], [np.inf, 1, 1])
    )

    # Memory: memory.peak (page cache included) against the pool, every step-1 run; step 2's smallest limit with no
    # OOM kill and no error sets the headroom. Throughput is kept beside it: reclaim without an OOM did not slow a
    # run in 0069 (memory pressure under 0.1 % of the time where no OOM happened).
    slope, intercept = np.polyfit(pool.pool, pool.memory_peak / MIB, 1)
    held, failed = [], []
    for _, row in memory.iterrows():
        limit = float(str(row.memory).rstrip("m"))
        reference = means.get((row.cpus, row.pool))
        ok = not row.oom_kill and not row.errors
        (held if ok else failed).append(
            {
                "cpus": int(row.cpus),
                "pool": int(row.pool),
                "limit_mib": limit,
                "fit_mib": float(intercept + slope * row.pool),
                "throughput": float(row.throughput),
                "throughput_step1": float(reference) if reference else None,
                "oom_kill": int(row.oom_kill),
                "errors": int(row.errors),
            }
        )
    # The headroom over the fit: per (vCPU, pool), the smallest limit that held; the largest of those ratios, to the
    # nearest 5 %. A limit between it and the failed ones is untested, so it is not offered.
    smallest = {}
    for h in held:
        key = (h["cpus"], h["pool"])
        smallest[key] = min(smallest.get(key, np.inf), h["limit_mib"] / h["fit_mib"])
    headroom = float(np.round((max(smallest.values()) - 1) * 20) / 20) if smallest else 0.25

    pdf_ratio = []
    for run in pool.run:
        samples, _ = analyze.load_samples(os.path.join(run, "samples.jsonl"))
        scraped = analyze.with_metrics(samples)
        if not scraped:  # moalhaddar/docx-to-pdf has no /metrics
            continue
        last = scraped[-1]
        docs = analyze.metric(last, "docx_to_pdf_document_size_bytes_sum")
        pdfs = analyze.metric(last, "docx_to_pdf_pdf_size_bytes_sum")
        if docs:
            pdf_ratio.append(pdfs / docs)
    manifest = analyze.manifest()
    names = pool.docs.iloc[0].split(",")
    largest = max(float(manifest[n]["bytes"]) for n in names) / MIB

    # Disk: what a conversion writes, uncapped; and the share of a capped device's rate it reached (step 3: bursts
    # of writes against a token bucket leave part of the cap unused).
    disk = frame[frame.run.str.contains("/3-disk/")]
    uncapped = disk[(disk.tmp == "disk") & disk.io_bps.isna() & (disk.tracing == "off")]
    capped = disk[disk.io_bps.notna()]
    written = (
        float(uncapped.write_bytes_per_conversion.mean())
        if len(uncapped)
        else float(pool.write_bytes_per_conversion.mean())
    )
    cap = capped.io_bps.map(lambda v: float(str(v).lower().rstrip("mb")) * 2**20)  # Docker reads 120mb as MiB
    reached = float((capped.throughput * written / cap).mean()) if len(capped) else 0.8
    out = {
        "platform": args.platform,
        "measured": f"{args.run}, {datetime.date.today().isoformat()}",
        "workload": {
            "name": args.workload,
            "documents": names,
            "largest_document_mib": round(largest, 2),
            "largest_pdf_mib": round(largest * float(np.max(pdf_ratio) if pdf_ratio else 1.1), 2),
            "max_document_mib": args.max_document_mib,
        },
        "throughput": throughput,
        "tie": 0.03,
        "pool_per_vcpu": float(np.median([best[c] / c for c in best])),
        "usl_vcpu": {"gamma": float(gamma), "alpha": float(alpha), "beta": float(beta)},
        "cpu_seconds_per_conversion": {
            "mean": float(pool[pool.pool <= pool.cpus].cpu_seconds_per_conversion.mean()),
            "sd": float(pool[pool.pool <= pool.cpus].cpu_seconds_per_conversion.std()),
        },
        "service_time": service_shape(args.runs),
        "memory": {
            "service_mib": round(float(intercept), 1),
            "per_instance_mib": round(float(slope), 1),
            "headroom": headroom,
            "instance_anon_peak_mib": round(float(pool.instance_anon_max.max() / MIB), 1),
            "instance_rss_peak_mean_mib": round(float(pool.server_peak_mean.mean() / MIB), 1),
            "instance_max_mib": 1024,
            "held": held,
            "failed": failed,
        },
        "disk": {
            "bytes_per_conversion": written,
            "utilisation": round(reached, 3),
            "tmpfs_throughput_ratio": float(disk[disk.tmp == "tmpfs"].throughput.mean() / uncapped.throughput.mean())
            if len(uncapped)
            else None,
            "tracing_throughput_ratio": float(disk[disk.tracing == "on"].throughput.mean() / uncapped.throughput.mean())
            if len(uncapped)
            else None,
        },
    }
    # S at low load against S at saturation (Little: pool / X) for the ramp's (vCPU, pool): conversions run faster
    # when fewer share the memory bus; sizing.py interpolates between the two by utilisation.
    st = out["service_time"]
    if st:
        st["low_load_ratio"] = st["mean_s_at_low_load"] / (st["pool"] / means[(st["cpus"], st["pool"])])
    with open(args.out, "w") as f:
        json.dump(out, f, indent=1)
        f.write("\n")
    print(json.dumps({k: v for k, v in out.items() if k not in ("throughput", "service_time")}, indent=1))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
