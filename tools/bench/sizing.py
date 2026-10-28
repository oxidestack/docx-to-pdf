"""Sizing from the bench's coefficients: the two questions a deployment asks, as formulas.

    hardware   vCPU, memory and disk → the launch settings and the throughput they give
    target     a response-time percentile and a request rate → the pod shape, the pod count and the settings

Standard library only, like the rest of bench.py's host side. Two inputs, measured apart because the first carries
over between machines and the second does not:

- the **workload**: what a document mix costs on any hardware: memory, the bytes a conversion writes, the shape of
  the service time S / E[S], the disk's share of its rate. One JSON file per platform and workload
  (`coefficients/<platform>-<workload>.json`), written from the quick plan's runs; arm64 and amd64 agree on every one
  of these to a few percent.
- the **platform**: what the hardware is, three numbers: `s1`, one conversion's mean time with nothing else running,
  the service's HTTP work around it included (the CPU's speed; 0.94 s on a Xeon Gold 6248R vCPU since pictures are
  no longer reduced; 1.53 s on an M1 Max core and 2.54 s on the Xeon before);
  `contention`, how much slower a conversion runs with every vCPU converting (1.10 and 1.19: the memory bus and the
  caches); and the disk's MB/s. `bench.py probe` measures them on the node itself in a few minutes; a quick plan's
  report carries its own.

The model:

- S_sat = contention · s1, the mean conversion with every instance busy; throughput X(C, pool) = min(pool, C) / S_sat
  (linear in vCPU: both platforms' USL had α, β ≈ 0; pool = vCPU is the best pool on both);
- at an open rate, S(ρ) = s1 + (S_sat - s1)·ρ; its shape S / E[S] from the workload;
- the response time of an open load λ on one pod: an M/G/p queue, Erlang C for the probability of waiting, the
  wait exponential with the Allen-Cunneen mean (c_a² + c_s²) / 2 times the M/M/p one, convolved with S;
- disk: X_disk = u · B / W, W the bytes a conversion writes, u the share of the disk's rate it gets;
- memory, from the worst case: the service plus, per pool slot, the hard per-conversion limit and one poll's growth
  past it; no document can take an instance further (it is killed there), so no document mix OOMs the pod.
  The typical fit (the service and one instance's peak on the report mix, each rounded up to a power of two, plus
  headroom) stays as the named option `basis="typical"`: smaller, and a heavy document can OOM the pod;
- /tmp, from the worst case: per conversion 3 × the document or a long layout's PDF twice, per waiting request the
  document, per instance its profile (measured on the heavy documents and the bombs); a deflate bomb is refused once
  its parts inflate past DOCX_TO_PDF_MAX_INFLATED_SIZE, max(3 × D, 256 MiB) by default.
"""

import json
import math
import os

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT = os.path.join(HERE, "coefficients", "amd64-report.json")
# Without a measured contention, the larger of the two measured (1.10 on an M1, 1.19 on a Xeon), rounded up.
CONTENTION = 1.2
# The service's default DOCX_TO_PDF_INSTANCE_HARD_MEMORY (`src/config.rs` DEFAULT_INSTANCE_HARD_MEMORY): an instance
# whose RSS passes it while converting is killed and its request answered 413, so one document cannot OOM the pod.
# 2 GiB passes every document measured within the 10 MiB default (the worst, 2000 tables, 1.86 GiB) and stops the
# bombs.
HARD_MEMORY_MIB = 2048
# How far past the hard limit an instance can get before the next 100 ms read of its RSS kills it: the largest growth in
# one 100 ms sample of every heavy document and bomb measured on amd64 is 183 MiB (eight 1-gigapixel PNGs; 55-63 MiB
# for embedded photographs, 100 MiB for tables), rounded up to a power of two. Counted per instance: every instance
# may be just under the limit at one read and past it at the next.
HARD_MEMORY_OVERSHOOT_MIB = 256
# /tmp per conversion, measured on the heavy documents: pictures leave the document, its PDF and about one more document
# of LibreOffice's temporary files, 3.0 × the document; a long layout leaves its PDF twice, 178 MiB for 2000 tables
# (1.2 MiB) and at most about 2 × 130 MiB for a tables bomb within the 180 s timeout. A profile is 0.35 MiB
# (migration guide §4).
TMP_DOCUMENTS_PER_CONVERSION = 3
TMP_LONG_LAYOUT_MIB = 256
PROFILE_MIB = 0.35
BASES = ("worst", "typical")


def load(path=None, platform=None, **override):
    """The workload's coefficients with `hw`, the platform, inside: `platform` is a probe's answer (explain()'s JSON),
    a quick plan's report (`arm64`, `amd64` or a path), or None for the workload's own; `override` sets s1,
    contention, disk_mbps."""
    with open(path or DEFAULT) as f:
        k = json.load(f)
    if platform in (None, ""):
        hw = platform_of(k)
    else:
        named = os.path.join(HERE, "coefficients", f"{platform}-report.json")
        with open(named if os.path.exists(named) else platform) as f:
            source = json.load(f)
        if "parameters" in source:  # explain()'s answer: a probe's
            hw = {
                "platform": source["platform"]["name"],
                "measured": source["platform"].get("measured"),
                "cpus": source["platform"].get("cpus_probed"),
                **{key: v["value"] for key, v in source["parameters"].items()},
            }
        else:
            hw = platform_of(source)
    hw = dict(hw)
    given = {key: value for key, value in override.items() if value is not None}
    if given:
        if "s1" in given and "contention" not in given and platform in (None, ""):
            given["contention"] = CONTENTION  # a new s1 without its own contention: not the workload's platform's
        hw.update(
            given,
            platform=f"{hw['platform']}, with " + ", ".join(f"{key} {value:g}" for key, value in sorted(given.items())),
        )
    k["hw"] = hw
    return k


def platform_of(k):
    """s1 and contention from a quick plan's report: s1 is the ramp's lowest plateau; S_sat the median over 2+ vCPU
    of C / X(C, C), step 1's closed runs."""
    table = measured(k)
    sat = sorted(c / table[c][c] for c in table if c >= 2 and c in table[c])
    s_sat = sat[len(sat) // 2] if len(sat) % 2 else (sat[len(sat) // 2 - 1] + sat[len(sat) // 2]) / 2
    s1 = k["service_time"]["mean_s_at_low_load"]
    return {
        "platform": k["platform"],
        "s1": round(s1, 3),
        "contention": round(s_sat / s1, 3),
        "disk_mbps": None,
        "measured": k["measured"],
    }


def usl(n, gamma, alpha, beta):
    return gamma * n / (1 + alpha * (n - 1) + beta * n * (n - 1))


def measured(k):
    """{cpus: {pool: mean conversions/s}} from the replicates."""
    return {int(c): {int(p): sum(v) / len(v) for p, v in pools.items()} for c, pools in k["throughput"].items()}


# Throughput and service time -------------------------------------------------------------------------------------


def best_pool(k, cpus):
    """pool = vCPU: more adds no throughput and slows each conversion, fewer leaves cores idle (measured on arm64 and
    on amd64)."""
    return cpus


def saturated(k):
    return k["hw"]["contention"] * k["hw"]["s1"]


def throughput(k, cpus, pool):
    return min(pool, cpus) / saturated(k)


def service_mean(k, cpus, pool, rate=None):
    """E[S] at saturation (rate None), or at an open rate: S(ρ) = s1 + (S_sat - s1)·ρ with ρ = rate·S(ρ)/pool, so
    ρ = rate·s1 / (pool - rate·(S_sat - s1)). A pool above the vCPU shares them: S_sat × pool / vCPU."""
    sat = pool / throughput(k, cpus, pool)
    if rate is None:
        return sat
    low = k["hw"]["s1"]
    rho = min(1.0, rate * low / max(pool - rate * (sat - low), 1e-9))
    return low + (sat - low) * rho


def service_quantile(k, cpus, pool, q, rate=None):
    return service_mean(k, cpus, pool, rate) * shape_quantile(k["service_time"]["shape"], q)


def shape_quantile(shape, q):
    """`shape` holds S / E[S] at the quantiles 0, 0.01, ..., 1."""
    x = q * (len(shape) - 1)
    i = min(int(x), len(shape) - 2)
    return shape[i] + (shape[i + 1] - shape[i]) * (x - i)


# The queue ------------------------------------------------------------------------------------------------------


def erlang_c(servers, offered):
    """The probability that an arrival waits, M/M/c with c servers and offered load a = λ·E[S] (a < c)."""
    term, total = 1.0, 1.0
    for i in range(1, servers):
        term *= offered / i
        total += term
    last = term * offered / servers / (1 - offered / servers)
    return last / (total + last)


def response_quantile(k, cpus, pool, rate, q, ca2=1.0):
    """The q-quantile of response time (wait + conversion) at an open, Poisson-like rate on one pod; None past
    saturation. The wait: Erlang C, exponential with the Allen-Cunneen mean."""
    mean = service_mean(k, cpus, pool, rate)
    offered = rate * mean
    if offered >= pool:
        return None
    shape = k["service_time"]["shape"]
    cs2 = k["service_time"]["cv2"]
    wait_p = erlang_c(pool, offered)
    theta = (pool / mean - rate) * 2 / (ca2 + cs2)
    points = [mean * s for s in shape]

    def cdf(r):
        total = 0.0
        for i in range(len(points) - 1):
            s = (points[i] + points[i + 1]) / 2
            if s <= r:
                total += (1 - wait_p * math.exp(-theta * (r - s))) / (len(points) - 1)
        return total

    lo, hi = 0.0, points[-1] + 50 / theta + 1
    for _ in range(60):
        mid = (lo + hi) / 2
        lo, hi = (mid, hi) if cdf(mid) < q else (lo, mid)
    return hi


# Memory and disk ------------------------------------------------------------------------------------------------


def power_of_two(mib):
    """The next power of two at or above `mib`: the formulas a person writes down (the fit 180 + 253·pool on amd64
    and 182 + 250·pool on arm64 become 256 + 256·pool)."""
    return 2 ** math.ceil(math.log2(mib))


def memory_terms(k):
    """The service's and one instance's MiB, each rounded up to a power of two."""
    m = k["memory"]
    return power_of_two(m["service_mib"]), power_of_two(m["per_instance_mib"])


def per_slot(k, basis):
    """The MiB a pool slot adds and the factor over the sum: the hard limit and its overshoot with no headroom (the
    worst case is a bound already), or the typical instance with the measured headroom."""
    if basis == "worst":
        return HARD_MEMORY_MIB + HARD_MEMORY_OVERSHOOT_MIB, 1.0
    if basis == "typical":
        return memory_terms(k)[1], 1 + k["memory"]["headroom"]
    raise ValueError(f"basis {basis!r} is not one of {BASES}")


def memory_needed(k, pool, tmpfs_mib=0.0, basis="worst"):
    """The container's memory limit, MiB. "worst": service + pool × (hard limit + overshoot) + tmpfs, which no
    document mix passes; "typical": (1 + headroom) × (service + pool × instance + tmpfs), fitted on the report mix."""
    slot, factor = per_slot(k, basis)
    return (memory_terms(k)[0] + pool * slot + tmpfs_mib) * factor


def pool_for_memory(k, ram_mib, tmpfs_mib=0.0, basis="worst"):
    slot, factor = per_slot(k, basis)
    return int((ram_mib / factor - memory_terms(k)[0] - tmpfs_mib) // slot)


def tmpfs_mib(k, pool, waiting):
    """The typical mix's /tmp (migration guide §4): profiles, a document per request held, a PDF per conversion; per
    pool slot, rounded up to a power of two (64 MiB a slot for the report mix with the queue as long as the pool)."""
    w = k["workload"]
    exact = (
        (pool + 1) * PROFILE_MIB + (pool + waiting) * w["largest_document_mib"] + pool * (w["largest_pdf_mib"] + 0.06)
    )
    return power_of_two(exact / pool) * pool


def tmp_worst_mib(k, pool, waiting, document_mib=None):
    """The most /tmp holds at once, MiB, rounded up: a profile per instance and one replacement's,
    the document of each waiting request, and per conversion the larger of 3 × the document and a long layout's PDF
    twice. `document_mib` is DOCX_TO_PDF_MAX_DOCUMENT_SIZE, the workload's by default. A deflate bomb is refused
    once its parts inflate past DOCX_TO_PDF_MAX_INFLATED_SIZE, max(3 × D, 256 MiB) by default."""
    d = k["workload"]["max_document_mib"] if document_mib is None else document_mib
    conversion = max(TMP_DOCUMENTS_PER_CONVERSION * d, TMP_LONG_LAYOUT_MIB)
    return math.ceil((pool + 1) * PROFILE_MIB + waiting * d + pool * conversion)


# The two answers ------------------------------------------------------------------------------------------------

# The service's default conversion timeout (`src/config.rs` DEFAULT_CONVERSION_TIMEOUT): the slowest document within
# the default size limit, `tables-1000`, takes 63 s, which moalhaddar/docx-to-pdf converts, so no recommendation goes
# below it.
DEFAULT_CONVERSION_TIMEOUT_S = 180


def settings(k, cpus, pool):
    """Timeouts and the queue from the measured tail: a conversion may take 4× the p99 at saturation, and never less
    than the service's default, since the mix's p99 says nothing of the documents outside it; the queue holds
    one request per instance (the SRE book's ≤ 50-100 % of the pool for steady traffic); a request waits at most
    as long as a full queue takes to drain at the p99, with a 1.5× margin."""
    p99 = service_quantile(k, cpus, pool, 0.99)
    waiting = pool
    conversion_timeout = max(DEFAULT_CONVERSION_TIMEOUT_S, math.ceil(4 * p99 / 10) * 10)
    wait_timeout = max(30, math.ceil(1.5 * (waiting / pool + 1) * p99 / 10) * 10)
    return {
        "DOCX_TO_PDF_POOL_SIZE": pool,
        "DOCX_TO_PDF_POOL_MAX_WAITING": waiting,
        "DOCX_TO_PDF_CONVERSION_TIMEOUT": f"{conversion_timeout}s",
        "DOCX_TO_PDF_POOL_WAIT_TIMEOUT": f"{wait_timeout}s",
        "DOCX_TO_PDF_INSTANCE_MAX_MEMORY": f"{int(k['memory']['instance_max_mib'])}MiB",
        "DOCX_TO_PDF_INSTANCE_HARD_MEMORY": f"{HARD_MEMORY_MIB}MiB",
        "DOCX_TO_PDF_MAX_DOCUMENT_SIZE": f"{int(k['workload']['max_document_mib'])}MiB",
    }


def hardware(k, cpus, ram_gib, disk_mbps=None, tmpfs=False, basis="worst"):
    """Answer 1: vCPU, memory and disk → the pool and the settings, and what they give. `basis` "worst" holds every
    instance at the hard limit at once; "typical" the report mix's fit."""
    ram = ram_gib * 1024

    def tmp_of(pool):
        return tmp_worst_mib(k, pool, pool) if basis == "worst" else tmpfs_mib(k, pool, pool)

    def needed(pool):
        return memory_needed(k, pool, tmp_of(pool) if tmpfs else 0.0, basis)

    by_cpu = best_pool(k, cpus)
    pool = by_cpu
    while pool >= 1 and needed(pool) > ram:
        pool -= 1
    if pool < 1:
        return {"error": f"{ram_gib} GiB does not hold one instance: {needed(1) / 1024:.2f} GiB needed ({basis} case)"}
    x = throughput(k, cpus, pool)
    bound = "memory" if pool < by_cpu else "CPU"
    disk_mbps = disk_mbps or k["hw"].get("disk_mbps")
    if disk_mbps and not tmpfs:
        x_disk = disk_mbps * 1e6 * k["disk"]["utilisation"] / k["disk"]["bytes_per_conversion"]
        if x_disk < x:
            x, bound = x_disk, "disk"
    s = service_mean(k, cpus, pool)
    return {
        "cpus": cpus,
        "ram_gib": ram_gib,
        "pool": pool,
        "pool_by_cpu": by_cpu,
        "pool_by_memory": pool_for_memory(k, ram, tmp_of(pool) if tmpfs else 0.0, basis),
        "bound": bound,
        "basis": basis,
        "throughput_per_s": x,
        "throughput_per_min": x * 60,
        "disk_mbps": disk_mbps,
        "service_mean_s": s,
        "service_p95_s": service_quantile(k, cpus, pool, 0.95),
        "service_p99_s": service_quantile(k, cpus, pool, 0.99),
        "memory_limit_gib": needed(pool) / 1024,
        "tmp_mib": tmp_of(pool),
        "settings": settings(k, cpus, pool),
    }


def target(k, latency_s, rate_per_s, quantile=0.95, shapes=None, utilisation=0.85, basis="worst"):
    """Answer 2: for each pod shape (vCPU at its best pool), the fewest pods whose response-time quantile stays under
    the target at rate/pods each, and no pod above `utilisation` of its saturation throughput. Sorted by total
    vCPU, then by pods."""
    shapes = shapes or [1, 2, 4, 6, 8]
    options = []
    for cpus in shapes:
        pool = best_pool(k, cpus)
        x = throughput(k, cpus, pool)
        floor = response_quantile(k, cpus, pool, 1e-9, quantile)
        if floor is None or floor > latency_s:
            options.append(
                {
                    "cpus": cpus,
                    "pool": pool,
                    "pods": None,
                    "why": f"p{quantile * 100:g} of one conversion alone is {floor:.1f} s",
                }
            )
            continue
        pods = max(1, math.ceil(rate_per_s / (utilisation * x)))
        while True:
            r = response_quantile(k, cpus, pool, rate_per_s / pods, quantile)
            if r is not None and r <= latency_s:
                break
            pods += 1
        options.append(
            {
                "cpus": cpus,
                "pool": pool,
                "pods": pods,
                "total_vcpu": pods * cpus,
                "memory_gib": memory_needed(k, pool, basis=basis) / 1024,
                "utilisation": rate_per_s / pods / x,
                "latency_s": r,
                "settings": settings(k, cpus, pool),
            }
        )
    options.sort(key=lambda o: (o.get("total_vcpu") or math.inf, o.get("pods") or math.inf))
    return options


# The self-describing answer ---------------------------------------------------------------------------------------


def explain(k, cpus=4):
    """The platform's numbers, the workload's constants and every formula of this module in words, each worked out
    for `cpus` vCPU on this platform: what `bench.py probe` writes and the probe endpoint answers, so a person or
    another program sizes a deployment from it without this module: it measures the machine, but answers with the
    parameters and how to use them, not with final settings."""
    hw, m, w = k["hw"], k["memory"], k["workload"]
    s1, contention = hw["s1"], hw["contention"]
    sat = contention * s1
    service, instance = memory_terms(k)
    headroom = m["headroom"]
    written = k["disk"]["bytes_per_conversion"] / 1e6
    share = k["disk"]["utilisation"]
    shape = k["service_time"]["shape"]
    q50, q95, q99 = (shape_quantile(shape, q) for q in (0.5, 0.95, 0.99))
    cs2 = k["service_time"]["cv2"]
    pool = cpus
    x = throughput(k, cpus, pool)
    tmpfs_slot = tmpfs_mib(k, pool, pool) / pool
    tmp = tmp_worst_mib(k, pool, pool)
    hard, over = HARD_MEMORY_MIB, HARD_MEMORY_OVERSHOOT_MIB
    document = w["max_document_mib"]
    disk = hw.get("disk_mbps")
    s = settings(k, cpus, pool)
    lam = 0.7 * x
    return {
        "platform": {"name": hw["platform"], "measured": hw.get("measured"), "cpus_probed": hw.get("cpus")},
        "parameters": {
            "s1": {
                "value": s1,
                "unit": "s",
                "meaning": "a conversion's mean time with nothing else running, the service's HTTP work around "
                "it included and the client's transfer not: the CPU's speed for this workload (lower "
                "is faster; 0.94 on a Xeon Gold 6248R vCPU)",
            },
            "contention": {
                "value": contention,
                "unit": "ratio",
                "meaning": "how much slower a conversion runs with every vCPU converting at once: the "
                "memory bus and the caches (1.10 on the M1, 1.18 on the Xeon)",
            },
            "disk_mbps": {
                "value": disk,
                "unit": "MB/s",
                "meaning": "the disk's sustained write rate under /tmp, 1 MiB direct writes; null if not "
                "measured (IOPS do not matter: a conversion writes ~50 writes of ~0.9 MB)",
            },
        },
        "workload": {
            "name": w["name"],
            "documents": w["documents"],
            "meaning": "what this document mix costs on any hardware (measured equal on arm64 and amd64)",
            "memory_service_mib": service,
            "memory_per_instance_mib": instance,
            "memory_headroom": headroom,
            "written_mb_per_conversion": round(written, 1),
            "disk_share": share,
            "tmpfs_per_pool_slot_mib": tmpfs_slot,
            "time_quantiles_over_mean": {"p50": round(q50, 3), "p95": round(q95, 3), "p99": round(q99, 3)},
            "service_time_cv2": round(cs2, 3),
        },
        "limits": {
            "meaning": "what bounds a conversion whatever the document: the "
            "hard memory limit an instance is killed above, how far past it one 100 ms read lets it get, and what "
            "a conversion leaves in /tmp",
            "hard_memory_mib": hard,
            "hard_memory_overshoot_mib": over,
            "tmp_documents_per_conversion": TMP_DOCUMENTS_PER_CONVERSION,
            "tmp_long_layout_mib": TMP_LONG_LAYOUT_MIB,
            "profile_mib": PROFILE_MIB,
        },
        "formulas": [
            {
                "what": "pool size",
                "formula": "POOL_SIZE = vCPU",
                "why": "one conversion uses one core; a larger pool adds no throughput and slows each conversion, a "
                "smaller one leaves cores idle",
                "here": f"{cpus} vCPU → POOL_SIZE = {pool}",
            },
            {
                "what": "a conversion's time with every instance busy",
                "formula": "S_sat = contention × s1",
                "here": f"{contention:g} × {s1:g} = {sat:.2f} s",
            },
            {
                "what": "throughput, CPU-bound",
                "formula": "X_cpu = min(POOL_SIZE, vCPU) / S_sat conversions/s (× 60 a minute)",
                "here": f"{pool} / {sat:.2f} = {x:.2f}/s = {x * 60:.0f}/min",
            },
            {
                "what": "throughput, disk-bound (/tmp on disk; none on a tmpfs)",
                "formula": "X_disk = disk_share × disk_mbps / written_mb_per_conversion",
                "here": (
                    f"{share:g} × {disk:g} / {written:.1f} = {share * disk / written:.2f}/s"
                    if disk
                    else f"{share:g} × B / {written:.1f}: e.g. a 120 MB/s SATA SSD gives {share * 120 / written:.2f}/s"
                ),
            },
            {
                "what": "throughput of a pod",
                "formula": "X = min(X_cpu, X_disk)",
                "here": f"{min(x, share * disk / written) if disk else x:.2f}/s",
            },
            {
                "what": "a conversion's time: the floor (alone) and at saturation",
                "formula": "p_q = s1 × time_quantiles_over_mean[q] alone, S_sat × time_quantiles_over_mean[q] busy",
                "here": f"alone p50 {s1 * q50:.1f} s, p95 {s1 * q95:.1f} s, p99 {s1 * q99:.1f} s; busy p50 "
                f"{sat * q50:.1f} s, p95 {sat * q95:.1f} s, p99 {sat * q99:.1f} s; no hardware beats the floor",
            },
            {
                "what": "memory limit (= request)",
                "formula": "memory_service_mib + POOL_SIZE × (hard_memory_mib + hard_memory_overshoot_mib) + tmpfs "
                "MiB, tmpfs = the /tmp size below if /tmp is a tmpfs, else 0: every instance at the hard limit at "
                "once, so no document mix can OOM the pod; set hard_memory_mib to "
                "DOCX_TO_PDF_INSTANCE_HARD_MEMORY if it is not the default",
                "here": f"{service} + {pool} × ({hard} + {over}) = {memory_needed(k, pool):.0f} MiB on disk, "
                f"{memory_needed(k, pool, tmp):.0f} MiB with a tmpfs of {tmp} MiB",
            },
            {
                "what": "memory limit for the typical mix only (a named option)",
                "formula": "(1 + memory_headroom) × (memory_service_mib + memory_per_instance_mib × POOL_SIZE + tmpfs) "
                "MiB, tmpfs = tmpfs_per_pool_slot_mib × POOL_SIZE if /tmp is a tmpfs, else 0: fitted on "
                "the workload's documents; a heavier document (many tables, gigapixel pictures) can OOM the "
                "pod, and with memory.oom.group the OOM kills every conversion in it",
                "here": f"{1 + headroom:g} × ({service} + {instance} × {pool}) = "
                f"{memory_needed(k, pool, basis='typical'):.0f} MiB on disk, "
                f"{memory_needed(k, pool, tmpfs_slot * pool, 'typical'):.0f} MiB with a tmpfs of "
                f"{tmpfs_slot * pool:.0f} MiB",
            },
            {
                "what": "the pool a memory limit R MiB allows",
                "formula": "floor((R - memory_service_mib - tmpfs) / (hard_memory_mib + hard_memory_overshoot_mib)); "
                "for the typical mix floor((R / (1 + memory_headroom) - memory_service_mib - tmpfs) / "
                "memory_per_instance_mib)",
                "here": f"8 GiB → {pool_for_memory(k, 8192)}; typical mix {pool_for_memory(k, 8192, basis='typical')}",
            },
            {
                "what": "/tmp size (an emptyDir sizeLimit, or the tmpfs)",
                "formula": "(POOL_SIZE + 1) × profile_mib + POOL_MAX_WAITING × D + POOL_SIZE × max("
                "tmp_documents_per_conversion × D, tmp_long_layout_mib) MiB, rounded up, D = "
                "DOCX_TO_PDF_MAX_DOCUMENT_SIZE in MiB; a deflate bomb is refused once its parts inflate past "
                "DOCX_TO_PDF_MAX_INFLATED_SIZE, max(3 × D, 256 MiB) by default",
                "here": f"({pool} + 1) × {PROFILE_MIB:g} + {pool} × {document:g} + {pool} × max("
                f"{TMP_DOCUMENTS_PER_CONVERSION} × {document:g}, {TMP_LONG_LAYOUT_MIB}) = {tmp} MiB",
            },
            {
                "what": "timeouts and the queue",
                "formula": "POOL_MAX_WAITING = POOL_SIZE; CONVERSION_TIMEOUT = max(180 s, 4 × busy p99); "
                "POOL_WAIT_TIMEOUT = max(30 s, 1.5 × (POOL_MAX_WAITING / POOL_SIZE + 1) × busy p99), "
                "both rounded up to 10 s",
                "here": ", ".join(f"{key}={value}" for key, value in s.items()),
            },
            {
                "what": "response time at an open rate λ requests/s on one pod (an M/G/c queue)",
                "formula": "ρ = λ × s1 / (POOL_SIZE - λ × (S_sat - s1)); E[S] = s1 + (S_sat - s1) × ρ; "
                "a = λ × E[S] (must stay below POOL_SIZE); P_wait = Erlang C(POOL_SIZE, a) = "
                "(a^c / c!) × c / (c - a) / (Σ_{i<c} a^i / i! + (a^c / c!) × c / (c - a)), c = POOL_SIZE; "
                "the wait is P_wait × an exponential of rate θ = (POOL_SIZE / E[S] - λ) × 2 / (c_a² + "
                "service_time_cv2), c_a² = 1 for independent clients; response = wait + E[S] × "
                "time_quantiles_over_mean, its quantile solved numerically (bench.py size target)",
                "here": f"λ = {lam:.2f}/s (70 % of X): p50 {response_quantile(k, cpus, pool, lam, 0.5):.1f} s, p95 "
                f"{response_quantile(k, cpus, pool, lam, 0.95):.1f} s, p99 "
                f"{response_quantile(k, cpus, pool, lam, 0.99):.1f} s",
            },
            {
                "what": "pods for a rate R and a response-time target",
                "formula": "pods = ceil(R / (0.85 × X)), then one more while the target quantile at R / pods exceeds "
                "the target; add one for N+1",
                "here": f"R = 60/min, p95 ≤ 10 s: {next(o for o in target(k, 10, 1.0, 0.95, [cpus]))['pods']} pod(s) "
                f"of {cpus} vCPU",
            },
        ],
    }
