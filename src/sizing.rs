//! Sizing, as the platform probe answers it: the port of `tools/bench/sizing.py`'s `explain()`
//! and the formulas behind it. The answer carries the platform's three numbers (`s1`, `contention`, the disk's
//! MB/s), the reference workload's constants, and every formula worked out for the pod's vCPU, in exactly the
//! JSON `bench.py size explain` prints, so `bench.py size --platform FILE` takes it unchanged.
//!
//! The workload is embedded as numbers, not documents: [`REPORT`] holds what the reference report mix costs on any
//! hardware, from `tools/bench/coefficients/amd64-report.json` (measured the same on arm64 and amd64), and a test
//! checks it against that file. Every formula, its order of operations and its rounding follow `sizing.py`, so the
//! worked-out numbers agree with it to the digit (`tests/sizing_explain.rs` compares goldens it wrote).

use serde_json::{Value, json};

use crate::num::{ToF64, saturating_i64, saturating_u32, saturating_u64, saturating_usize};

/// `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`'s default in MiB: no instance converts past it.
pub const HARD_MEMORY_MIB: u64 = crate::config::DEFAULT_INSTANCE_HARD_MEMORY / (1024 * 1024);
/// How far past the hard limit an instance can get before the next 100 ms read of its RSS kills it, MiB: the
/// largest growth in one 100 ms sample of the heavy documents measured is 183 MiB (eight 1-gigapixel PNGs), rounded
/// up to a power of two. Counted per instance, since every instance can be at the limit at once.
pub const HARD_MEMORY_OVERSHOOT_MIB: u64 = 256;
/// A conversion's `/tmp` in documents: the document, its PDF and LibreOffice's temporary files.
pub const TMP_DOCUMENTS_PER_CONVERSION: u64 = 3;
/// A long layout's `/tmp`, MiB: its PDF twice, at most about 2 × 130 MiB for a tables bomb within the 180 s
/// timeout.
pub const TMP_LONG_LAYOUT_MIB: u64 = 256;
/// A LibreOffice profile, MiB (migration guide §4).
pub const PROFILE_MIB: f64 = 0.35;

// The service refuses a hard limit at or below the ceiling, so the defaults must keep that order.
const _: () = assert!(
    crate::config::DEFAULT_INSTANCE_HARD_MEMORY > crate::config::DEFAULT_INSTANCE_MAX_MEMORY
        && HARD_MEMORY_MIB > REPORT.instance_max_mib
);

/// The hard limit, MiB, that sizing takes for `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` set to `bytes`: the
/// bytes rounded up to a MiB; 0, no hard limit, leaves no bound on an instance, so the worst case is sized from the
/// default [`HARD_MEMORY_MIB`], which the launch settings then recommend.
pub fn hard_memory_mib(bytes: u64) -> u64 {
    match bytes {
        0 => HARD_MEMORY_MIB,
        bytes => bytes.div_ceil(1024 * 1024),
    }
}

/// What the memory limit is sized from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryBasis {
    /// Every instance at the hard limit at once, plus one poll's growth: no document mix can OOM the pod.
    Worst,
    /// The workload's measured fit with its headroom: smaller, and a heavy document can OOM the pod.
    Typical,
}

/// What a document mix costs on any hardware: the workload half of sizing, measured the same on arm64 and amd64.
#[derive(Debug, Clone, PartialEq)]
pub struct Workload {
    /// The mix's name.
    pub name: &'static str,
    /// Its documents.
    pub documents: &'static [&'static str],
    /// The service's own memory with the page cache it keeps, rounded up to a power of two, MiB.
    pub memory_service_mib: u64,
    /// One instance's memory, rounded up to a power of two, MiB.
    pub memory_per_instance_mib: u64,
    /// The share added above the two terms.
    pub memory_headroom: f64,
    /// `DOCX_TO_PDF_INSTANCE_MAX_MEMORY` the settings name, MiB.
    pub instance_max_mib: u64,
    /// `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` the settings name, MiB.
    pub max_document_mib: u64,
    /// The largest document of the mix, MiB.
    pub largest_document_mib: f64,
    /// The largest PDF of the mix, MiB.
    pub largest_pdf_mib: f64,
    /// Bytes a conversion writes to `/tmp`.
    pub bytes_per_conversion: f64,
    /// The share of the disk's rate the conversions get.
    pub disk_share: f64,
    /// The service time's shape: S / E\[S\] at the quantiles 0, 0.01, ..., 1.
    pub shape: &'static [f64],
    /// The service time's squared coefficient of variation.
    pub cv2: f64,
}

/// The reference report mix, `tools/bench/coefficients/amd64-report.json`, at the cost without picture reduction
/// (the service's default: reducing pictures costs the typical report two thirds of its CPU): the four
/// `report-img*` documents of `bench.py`'s `PROBE_DOCS`.
pub const REPORT: Workload = Workload {
    name: "report",
    documents: &[
        "report-img30-10mb",
        "report-img45-12mb",
        "report-img60-15mb",
        "report-img60-20mb",
    ],
    memory_service_mib: 256,
    memory_per_instance_mib: 256,
    memory_headroom: 0.25,
    instance_max_mib: 1024,
    max_document_mib: 32,
    largest_document_mib: 18.6,
    largest_pdf_mib: 18.74,
    bytes_per_conversion: 28_161_951.507_797_25,
    disk_share: 0.459,
    shape: &[
        0.6733, 0.6856, 0.687, 0.6971, 0.7035, 0.7145, 0.7224, 0.7267, 0.7298, 0.7321, 0.7343,
        0.7388, 0.7422, 0.7446, 0.7499, 0.7539, 0.7606, 0.7642, 0.7705, 0.7804, 0.7919, 0.8084,
        0.8404, 0.8498, 0.86, 0.8641, 0.8643, 0.866, 0.8707, 0.8785, 0.8797, 0.8814, 0.8822,
        0.8862, 0.8899, 0.8915, 0.8922, 0.8967, 0.8987, 0.9054, 0.9071, 0.9088, 0.9117, 0.9142,
        0.9196, 0.949, 0.9652, 0.9746, 1.0091, 1.0188, 1.0214, 1.0221, 1.0228, 1.0272, 1.03,
        1.0339, 1.0354, 1.0383, 1.0396, 1.0403, 1.0419, 1.0431, 1.05, 1.0539, 1.0601, 1.0619,
        1.0644, 1.0733, 1.076, 1.0892, 1.0962, 1.1164, 1.1399, 1.1536, 1.1687, 1.186, 1.1947,
        1.2013, 1.208, 1.2098, 1.2174, 1.2221, 1.2287, 1.2303, 1.233, 1.2388, 1.2459, 1.2484,
        1.257, 1.2602, 1.2692, 1.2756, 1.2915, 1.298, 1.3009, 1.3053, 1.3087, 1.3101, 1.3337,
        1.4026, 1.4533,
    ],
    cv2: 0.038_746_472_295_991_92,
};

/// The platform half: what the hardware is, as a probe measured it.
#[derive(Debug, Clone, PartialEq)]
pub struct Platform {
    /// What it is: the CPU model and the vCPU, or a name given to the probe.
    pub name: String,
    /// When and by what it was measured.
    pub measured: Option<String>,
    /// The vCPU (= pool) the probe converted on in parallel.
    pub cpus_probed: Option<usize>,
    /// A conversion's mean time with nothing else running, the service's HTTP work around it included, s.
    pub s1: f64,
    /// S with every instance converting at once / `s1`.
    pub contention: f64,
    /// The disk's sustained write rate under `/tmp`, MB/s; `None` if not measured.
    pub disk_mbps: Option<f64>,
}

/// `sizing.py` over one workload and one platform.
struct Model<'a> {
    w: &'a Workload,
    s1: f64,
    contention: f64,
    /// `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`, MiB.
    hard_mib: u64,
}

impl Model<'_> {
    fn saturated(&self) -> f64 {
        self.contention * self.s1
    }

    fn throughput(&self, cpus: usize, pool: usize) -> f64 {
        pool.min(cpus).to_f64() / self.saturated()
    }

    fn service_mean(&self, cpus: usize, pool: usize, rate: Option<f64>) -> f64 {
        let sat = pool.to_f64() / self.throughput(cpus, pool);
        let Some(rate) = rate else {
            return sat;
        };
        let low = self.s1;
        let rho = (rate * low / (pool.to_f64() - rate * (sat - low)).max(1e-9)).min(1.0);
        low + (sat - low) * rho
    }

    fn service_quantile(&self, cpus: usize, pool: usize, q: f64) -> f64 {
        self.service_mean(cpus, pool, None) * shape_quantile(self.w.shape, q)
    }

    /// The q-quantile of the response time at an open rate on one pod; `None` past saturation.
    fn response_quantile(&self, cpus: usize, pool: usize, rate: f64, q: f64) -> Option<f64> {
        let ca2 = 1.0;
        let mean = self.service_mean(cpus, pool, Some(rate));
        let offered = rate * mean;
        if offered >= pool.to_f64() {
            return None;
        }
        let wait_p = erlang_c(pool, offered);
        let theta = (pool.to_f64() / mean - rate) * 2.0 / (ca2 + self.w.cv2);
        let points: Vec<f64> = self.w.shape.iter().map(|s| mean * s).collect();
        let n = (points.len() - 1).to_f64();
        let cdf = |r: f64| {
            let mut total = 0.0;
            for pair in points.windows(2) {
                let s = f64::midpoint(pair[0], pair[1]);
                if s <= r {
                    total += (1.0 - wait_p * (-theta * (r - s)).exp()) / n;
                }
            }
            total
        };
        let (mut lo, mut hi) = (0.0, points[points.len() - 1] + 50.0 / theta + 1.0);
        for _ in 0..60 {
            let mid = f64::midpoint(lo, hi);
            if cdf(mid) < q {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some(hi)
    }

    /// The MiB a pool slot adds and the factor over the sum (`sizing.per_slot`).
    fn per_slot(&self, basis: MemoryBasis) -> (f64, f64) {
        match basis {
            MemoryBasis::Worst => ((self.hard_mib + HARD_MEMORY_OVERSHOOT_MIB).to_f64(), 1.0),
            MemoryBasis::Typical => (
                self.w.memory_per_instance_mib.to_f64(),
                1.0 + self.w.memory_headroom,
            ),
        }
    }

    fn memory_needed(&self, pool: usize, tmpfs_mib: f64, basis: MemoryBasis) -> f64 {
        let (slot, factor) = self.per_slot(basis);
        (self.w.memory_service_mib.to_f64() + pool.to_f64() * slot + tmpfs_mib) * factor
    }

    fn pool_for_memory(&self, ram_mib: f64, tmpfs_mib: f64, basis: MemoryBasis) -> i64 {
        let (slot, factor) = self.per_slot(basis);
        saturating_i64(
            ((ram_mib / factor - self.w.memory_service_mib.to_f64() - tmpfs_mib) / slot).floor(),
        )
    }

    fn tmpfs_mib(&self, pool: usize, waiting: usize) -> u64 {
        let w = self.w;
        let exact = (pool + 1).to_f64() * PROFILE_MIB
            + (pool + waiting).to_f64() * w.largest_document_mib
            + pool.to_f64() * (w.largest_pdf_mib + 0.06);
        power_of_two(exact / pool.to_f64()) * pool as u64
    }

    /// The most `/tmp` holds at once, MiB, rounded up (`sizing.tmp_worst_mib`): profiles, the waiting requests'
    /// documents, and per conversion the larger of 3 × the document and a long layout's PDF twice.
    fn tmp_worst_mib(&self, pool: usize, waiting: usize) -> u64 {
        let d = self.w.max_document_mib;
        let conversion = (TMP_DOCUMENTS_PER_CONVERSION * d).max(TMP_LONG_LAYOUT_MIB);
        saturating_u64(
            ((pool + 1).to_f64() * PROFILE_MIB
                + (waiting as u64 * d).to_f64()
                + (pool as u64 * conversion).to_f64())
            .ceil(),
        )
    }

    /// The launch settings, in `sizing.settings()`'s order.
    fn settings(&self, cpus: usize, pool: usize) -> Vec<(&'static str, String)> {
        let p99 = self.service_quantile(cpus, pool, 0.99);
        let waiting = pool;
        // Never under the service's default: the mix's p99 says nothing of the documents outside it, and the
        // slowest one within the default size limit takes 63 s on one amd64 vCPU.
        let floor = crate::config::DEFAULT_CONVERSION_TIMEOUT.as_secs_f64();
        let conversion_timeout = floor.max((4.0 * p99 / 10.0).ceil() * 10.0);
        let wait_timeout =
            30f64.max((1.5 * (waiting.to_f64() / pool.to_f64() + 1.0) * p99 / 10.0).ceil() * 10.0);
        vec![
            ("DOCX_TO_PDF_POOL_SIZE", pool.to_string()),
            ("DOCX_TO_PDF_POOL_MAX_WAITING", waiting.to_string()),
            (
                "DOCX_TO_PDF_CONVERSION_TIMEOUT",
                format!("{}s", saturating_i64(conversion_timeout)),
            ),
            (
                "DOCX_TO_PDF_POOL_WAIT_TIMEOUT",
                format!("{}s", saturating_i64(wait_timeout)),
            ),
            (
                "DOCX_TO_PDF_INSTANCE_MAX_MEMORY",
                format!("{}MiB", self.w.instance_max_mib),
            ),
            (
                "DOCX_TO_PDF_INSTANCE_HARD_MEMORY",
                format!("{}MiB", self.hard_mib),
            ),
            (
                "DOCX_TO_PDF_MAX_DOCUMENT_SIZE",
                format!("{}MiB", self.w.max_document_mib),
            ),
        ]
    }

    /// `sizing.target(k, latency, rate, quantile, [cpus])`'s one row: the pods, or `None` when one conversion
    /// alone already misses the target.
    fn pods_for(&self, cpus: usize, latency: f64, rate: f64, quantile: f64) -> Option<u64> {
        let utilisation = 0.85;
        let pool = cpus;
        let x = self.throughput(cpus, pool);
        match self.response_quantile(cpus, pool, 1e-9, quantile) {
            Some(floor) if floor <= latency => {}
            _ => return None,
        }
        let mut pods = saturating_u64((rate / (utilisation * x)).ceil()).max(1);
        loop {
            let r = self.response_quantile(cpus, pool, rate / pods.to_f64(), quantile);
            if r.is_some_and(|r| r <= latency) {
                return Some(pods);
            }
            pods += 1;
        }
    }
}

/// The probability that an arrival waits, M/M/c with c `servers` and offered load a = λ·E\[S\] (a < c).
pub fn erlang_c(servers: usize, offered: f64) -> f64 {
    let (mut term, mut total) = (1.0, 1.0);
    for i in 1..servers {
        term *= offered / i.to_f64();
        total += term;
    }
    let last = term * offered / servers.to_f64() / (1.0 - offered / servers.to_f64());
    last / (total + last)
}

/// `shape` holds S / E\[S\] at the quantiles 0, 0.01, ..., 1; linear between them.
pub fn shape_quantile(shape: &[f64], q: f64) -> f64 {
    let x = q * (shape.len() - 1).to_f64();
    let i = saturating_usize(x).min(shape.len() - 2);
    shape[i] + (shape[i + 1] - shape[i]) * (x - i.to_f64())
}

/// The next power of two at or above `mib`.
pub fn power_of_two(mib: f64) -> u64 {
    2u64.pow(saturating_u32(mib.log2().ceil()))
}

/// `round(x, digits)` as Python rounds: the decimal nearest the exact binary value.
pub fn round_to(x: f64, digits: usize) -> f64 {
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// Python's `format(x, "g")`: six significant digits, trailing zeros dropped, an exponent below 1e-4 and
/// from 1e6 on.
pub fn fmt_g(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.into();
    }
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".into()
        } else if x > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let scientific = format!("{x:.5e}");
    let (mantissa, exponent) = scientific
        .split_once('e')
        .expect("the scientific format has an exponent");
    let exponent: i32 = exponent
        .parse()
        .expect("the scientific format's exponent is a number");
    let trim = |s: &str| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s.to_owned()
        }
    };
    if (-4..6).contains(&exponent) {
        let decimals = usize::try_from(5 - exponent).expect("the exponent is below 6");
        trim(&format!("{x:.decimals$}"))
    } else {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exponent.abs())
    }
}

/// The platform's numbers, the workload's constants and every formula, each worked out for `cpus` vCPU:
/// `sizing.explain(k, cpus)` for `workload` on `platform`, the worst case sized from a hard limit of `hard_mib`
/// MiB (the pod's own, [`hard_memory_mib`]; `sizing.py` takes the default [`HARD_MEMORY_MIB`]).
pub fn explain(workload: &Workload, platform: &Platform, cpus: usize, hard_mib: u64) -> Value {
    let m = Model {
        w: workload,
        s1: platform.s1,
        contention: platform.contention,
        hard_mib,
    };
    let w = workload;
    let (s1, contention) = (platform.s1, platform.contention);
    let sat = contention * s1;
    let (service, instance) = (w.memory_service_mib, w.memory_per_instance_mib);
    let headroom = w.memory_headroom;
    let written = w.bytes_per_conversion / 1e6;
    let share = w.disk_share;
    let [q50, q95, q99] = [0.5, 0.95, 0.99].map(|q| shape_quantile(w.shape, q));
    let cs2 = w.cv2;
    let pool = cpus;
    let x = m.throughput(cpus, pool);
    let tmpfs_slot = m.tmpfs_mib(pool, pool).to_f64() / pool.to_f64();
    let tmp = m.tmp_worst_mib(pool, pool);
    let (hard, over) = (hard_mib, HARD_MEMORY_OVERSHOOT_MIB);
    let document = w.max_document_mib;
    let (worst, typical) = (MemoryBasis::Worst, MemoryBasis::Typical);
    let disk = platform.disk_mbps.filter(|d| *d != 0.0);
    let settings = m.settings(cpus, pool);
    let lam = 0.7 * x;
    let response = |q: f64| {
        m.response_quantile(cpus, pool, lam, q)
            .map_or_else(|| "None".to_owned(), |r| format!("{r:.1}"))
    };
    let pods = m
        .pods_for(cpus, 10.0, 1.0, 0.95)
        .map_or_else(|| "None".to_owned(), |p| p.to_string());
    let g = fmt_g;
    json!({
        "platform": {
            "name": platform.name,
            "measured": platform.measured,
            "cpus_probed": platform.cpus_probed,
        },
        "parameters": {
            "s1": {"value": s1, "unit": "s",
                   "meaning": "a conversion's mean time with nothing else running, the service's HTTP work around \
                               it included and the client's transfer not: the CPU's speed for this workload (lower \
                               is faster; 0.94 on a Xeon Gold 6248R vCPU)"},
            "contention": {"value": contention, "unit": "ratio",
                           "meaning": "how much slower a conversion runs with every vCPU converting at once: the \
                                       memory bus and the caches (1.10 on the M1, 1.18 on the Xeon)"},
            "disk_mbps": {"value": platform.disk_mbps, "unit": "MB/s",
                          "meaning": "the disk's sustained write rate under /tmp, 1 MiB direct writes; null if not \
                                      measured (IOPS do not matter: a conversion writes ~50 writes of ~0.9 MB)"},
        },
        "workload": {
            "name": w.name, "documents": w.documents,
            "meaning": "what this document mix costs on any hardware (measured equal on arm64 and amd64)",
            "memory_service_mib": service, "memory_per_instance_mib": instance, "memory_headroom": headroom,
            "written_mb_per_conversion": round_to(written, 1), "disk_share": share,
            "tmpfs_per_pool_slot_mib": tmpfs_slot,
            "time_quantiles_over_mean": {"p50": round_to(q50, 3), "p95": round_to(q95, 3), "p99": round_to(q99, 3)},
            "service_time_cv2": round_to(cs2, 3),
        },
        "limits": {
            "meaning": "what bounds a conversion whatever the document: the hard \
                        memory limit an instance is killed above, how far past it one 100 ms read lets it get, and what \
                        a conversion leaves in /tmp",
            "hard_memory_mib": hard, "hard_memory_overshoot_mib": over,
            "tmp_documents_per_conversion": TMP_DOCUMENTS_PER_CONVERSION, "tmp_long_layout_mib": TMP_LONG_LAYOUT_MIB,
            "profile_mib": PROFILE_MIB,
        },
        "formulas": [
            {"what": "pool size",
             "formula": "POOL_SIZE = vCPU",
             "why": "one conversion uses one core; a larger pool adds no throughput and slows each conversion, a \
                     smaller one leaves cores idle",
             "here": format!("{cpus} vCPU → POOL_SIZE = {pool}")},
            {"what": "a conversion's time with every instance busy",
             "formula": "S_sat = contention × s1",
             "here": format!("{} × {} = {sat:.2} s", g(contention), g(s1))},
            {"what": "throughput, CPU-bound",
             "formula": "X_cpu = min(POOL_SIZE, vCPU) / S_sat conversions/s (× 60 a minute)",
             "here": format!("{pool} / {sat:.2} = {x:.2}/s = {:.0}/min", x * 60.0)},
            {"what": "throughput, disk-bound (/tmp on disk; none on a tmpfs)",
             "formula": "X_disk = disk_share × disk_mbps / written_mb_per_conversion",
             "here": match disk {
                 Some(disk) => format!("{} × {} / {written:.1} = {:.2}/s", g(share), g(disk), share * disk / written),
                 None => format!("{} × B / {written:.1}: e.g. a 120 MB/s SATA SSD gives {:.2}/s", g(share),
                                 share * 120.0 / written),
             }},
            {"what": "throughput of a pod",
             "formula": "X = min(X_cpu, X_disk)",
             "here": format!("{:.2}/s", disk.map_or(x, |disk| x.min(share * disk / written)))},
            {"what": "a conversion's time: the floor (alone) and at saturation",
             "formula": "p_q = s1 × time_quantiles_over_mean[q] alone, S_sat × time_quantiles_over_mean[q] busy",
             "here": format!("alone p50 {:.1} s, p95 {:.1} s, p99 {:.1} s; busy p50 {:.1} s, p95 {:.1} s, p99 {:.1} s; \
                              no hardware beats the floor",
                             s1 * q50, s1 * q95, s1 * q99, sat * q50, sat * q95, sat * q99)},
            {"what": "memory limit (= request)",
             "formula": "memory_service_mib + POOL_SIZE × (hard_memory_mib + hard_memory_overshoot_mib) + tmpfs MiB, \
                         tmpfs = the /tmp size below if /tmp is a tmpfs, else 0: every instance at the hard limit at \
                         once, so no document mix can OOM the pod; set hard_memory_mib to \
                         DOCX_TO_PDF_INSTANCE_HARD_MEMORY if it is not the default",
             "here": format!("{service} + {pool} × ({hard} + {over}) = {:.0} MiB on disk, {:.0} MiB with a tmpfs of \
                              {tmp} MiB",
                             m.memory_needed(pool, 0.0, worst), m.memory_needed(pool, tmp.to_f64(), worst))},
            {"what": "memory limit for the typical mix only (a named option)",
             "formula": "(1 + memory_headroom) × (memory_service_mib + memory_per_instance_mib × POOL_SIZE + tmpfs) \
                         MiB, tmpfs = tmpfs_per_pool_slot_mib × POOL_SIZE if /tmp is a tmpfs, else 0: fitted on \
                         the workload's documents; a heavier document (many tables, gigapixel pictures) can OOM the \
                         pod, and with memory.oom.group the OOM kills every conversion in it",
             "here": format!("{} × ({service} + {instance} × {pool}) = {:.0} MiB on disk, {:.0} MiB with a tmpfs of \
                              {:.0} MiB",
                             g(1.0 + headroom), m.memory_needed(pool, 0.0, typical),
                             m.memory_needed(pool, tmpfs_slot * pool.to_f64(), typical), tmpfs_slot * pool.to_f64())},
            {"what": "the pool a memory limit R MiB allows",
             "formula": "floor((R - memory_service_mib - tmpfs) / (hard_memory_mib + hard_memory_overshoot_mib)); \
                         for the typical mix floor((R / (1 + memory_headroom) - memory_service_mib - tmpfs) / \
                         memory_per_instance_mib)",
             "here": format!("8 GiB → {}; typical mix {}", m.pool_for_memory(8192.0, 0.0, worst),
                             m.pool_for_memory(8192.0, 0.0, typical))},
            {"what": "/tmp size (an emptyDir sizeLimit, or the tmpfs)",
             "formula": "(POOL_SIZE + 1) × profile_mib + POOL_MAX_WAITING × D + POOL_SIZE × max(\
                         tmp_documents_per_conversion × D, tmp_long_layout_mib) MiB, rounded up, D = \
                         DOCX_TO_PDF_MAX_DOCUMENT_SIZE in MiB; a deflate bomb is refused once its parts inflate past \
                         DOCX_TO_PDF_MAX_INFLATED_SIZE, max(3 × D, 256 MiB) by default",
             "here": format!("({pool} + 1) × {} + {pool} × {document} + {pool} × max({TMP_DOCUMENTS_PER_CONVERSION} \
                              × {document}, {TMP_LONG_LAYOUT_MIB}) = {tmp} MiB", g(PROFILE_MIB))},
            {"what": "timeouts and the queue",
             "formula": "POOL_MAX_WAITING = POOL_SIZE; CONVERSION_TIMEOUT = max(180 s, 4 × busy p99); \
                         POOL_WAIT_TIMEOUT = max(30 s, 1.5 × (POOL_MAX_WAITING / POOL_SIZE + 1) × busy p99), \
                         both rounded up to 10 s",
             "here": settings.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(", ")},
            {"what": "response time at an open rate λ requests/s on one pod (an M/G/c queue)",
             "formula": "ρ = λ × s1 / (POOL_SIZE - λ × (S_sat - s1)); E[S] = s1 + (S_sat - s1) × ρ; \
                         a = λ × E[S] (must stay below POOL_SIZE); P_wait = Erlang C(POOL_SIZE, a) = \
                         (a^c / c!) × c / (c - a) / (Σ_{i<c} a^i / i! + (a^c / c!) × c / (c - a)), c = POOL_SIZE; \
                         the wait is P_wait × an exponential of rate θ = (POOL_SIZE / E[S] - λ) × 2 / (c_a² + \
                         service_time_cv2), c_a² = 1 for independent clients; response = wait + E[S] × \
                         time_quantiles_over_mean, its quantile solved numerically (bench.py size target)",
             "here": format!("λ = {lam:.2}/s (70 % of X): p50 {} s, p95 {} s, p99 {} s",
                             response(0.5), response(0.95), response(0.99))},
            {"what": "pods for a rate R and a response-time target",
             "formula": "pods = ceil(R / (0.85 × X)), then one more while the target quantile at R / pods exceeds \
                         the target; add one for N+1",
             "here": format!("R = 60/min, p95 ≤ 10 s: {pods} pod(s) of {cpus} vCPU")},
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_g_is_pythons_general_format() {
        for (x, g) in [
            (2.542, "2.542"),
            (1.0, "1"),
            (1.25, "1.25"),
            (0.459, "0.459"),
            (120.0, "120"),
            (219.3, "219.3"),
            (1.183, "1.183"),
            (123_456.0, "123456"),
            (1_234_567.0, "1.23457e+06"),
            (0.0001, "0.0001"),
            (0.000_012_34, "1.234e-05"),
            (2.0 / 3.0, "0.666667"),
            (-3.5, "-3.5"),
            (0.0, "0"),
        ] {
            assert_eq!(fmt_g(x), g, "{x}");
        }
    }

    #[test]
    fn round_to_is_pythons_round() {
        assert_eq!(round_to(45.602_580_48, 1), 45.6);
        assert_eq!(round_to(0.9639, 3), 0.964);
        assert_eq!(round_to(0.060_209_876_867_063_94, 3), 0.06);
    }

    #[test]
    fn erlang_c_of_one_server_is_its_load() {
        assert!((erlang_c(1, 0.5) - 0.5).abs() < 1e-12);
        // M/M/2 at a = 1: P_wait = 1/3.
        assert!((erlang_c(2, 1.0) - 1.0 / 3.0).abs() < 1e-12);
    }

    fn conversion_timeout(s1: f64, cpus: usize) -> String {
        let m = Model {
            w: &REPORT,
            s1,
            contention: 1.2,
            hard_mib: HARD_MEMORY_MIB,
        };
        let settings = m.settings(cpus, cpus);
        let (_, value) = settings
            .iter()
            .find(|(key, _)| *key == "DOCX_TO_PDF_CONVERSION_TIMEOUT")
            .unwrap();
        value.clone()
    }

    #[test]
    fn conversion_timeout_never_goes_under_the_services_default() {
        // The typical mix's 4 × p99 is a few seconds; tables-1000 takes 63 s.
        for (s1, cpus) in [(0.459, 1), (0.9, 4), (2.542, 8), (10.0, 2)] {
            assert_eq!(conversion_timeout(s1, cpus), "180s", "s1 {s1}, {cpus} vCPU");
        }
        // A tail past the floor still sets it: 4 × p99, rounded up to 10 s.
        let slow = conversion_timeout(60.0, 2);
        let secs: u64 = slow.trim_end_matches('s').parse().unwrap();
        assert!(secs > 180 && secs.is_multiple_of(10), "{slow}");
    }

    fn model() -> Model<'static> {
        Model {
            w: &REPORT,
            s1: 0.94,
            contention: 1.18,
            hard_mib: HARD_MEMORY_MIB,
        }
    }

    #[test]
    fn memory_is_sized_from_the_worst_case_by_default() {
        // Every instance at the 2 GiB hard limit at once, plus one poll's growth past it.
        let m = model();
        assert_eq!(HARD_MEMORY_MIB, 2048);
        assert_eq!(
            m.memory_needed(4, 0.0, MemoryBasis::Worst),
            f64::from(256 + 4 * (2048 + 256))
        );
        assert_eq!(m.memory_needed(4, 0.0, MemoryBasis::Typical), 1.25 * 1280.0);
        assert_eq!(m.pool_for_memory(8192.0, 0.0, MemoryBasis::Worst), 3);
        assert_eq!(m.pool_for_memory(8192.0, 0.0, MemoryBasis::Typical), 24);
    }

    #[test]
    fn a_configured_hard_limit_sizes_the_worst_case() {
        // A pod with a 1 GiB hard limit, not the 2 GiB default.
        let m = Model {
            hard_mib: hard_memory_mib(1024 * 1024 * 1024),
            ..model()
        };
        assert_eq!(
            m.memory_needed(4, 0.0, MemoryBasis::Worst),
            f64::from(256 + 4 * (1024 + 256))
        );
        assert_eq!(m.pool_for_memory(8192.0, 0.0, MemoryBasis::Worst), 6);
        assert!(
            m.settings(4, 4)
                .contains(&("DOCX_TO_PDF_INSTANCE_HARD_MEMORY", "1024MiB".into()))
        );
    }

    #[test]
    fn no_hard_limit_sizes_from_the_default_and_bytes_round_up_to_a_mib() {
        assert_eq!(hard_memory_mib(0), HARD_MEMORY_MIB);
        assert_eq!(hard_memory_mib(1536 * 1024 * 1024), 1536);
        assert_eq!(hard_memory_mib(1536 * 1024 * 1024 + 1), 1537);
    }

    #[test]
    fn tmp_holds_three_documents_or_a_long_layout_per_conversion() {
        // 32 MiB documents: 3 × 32 < 256 MiB, a tables bomb's PDF twice within the timeout.
        assert_eq!(
            model().tmp_worst_mib(4, 4),
            saturating_u64((5.0f64 * 0.35 + 128.0 + 1024.0).ceil())
        );
    }

    #[test]
    fn power_of_two_rounds_up() {
        assert_eq!(power_of_two(179.6), 256);
        assert_eq!(power_of_two(256.0), 256);
        assert_eq!(power_of_two(56.5), 64);
    }
}
