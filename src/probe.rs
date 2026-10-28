//! The platform probe: `POST /probe` on a port of its own (`DOCX_TO_PDF_PROBE_PORT`, off by
//! default), never next to the conversion paths, because it holds every instance for a minute or two.
//!
//! The request carries the documents (multipart, one or more `document` file parts: the reference report mix
//! `bench.py probe --url` sends, or a client's own). The probe reserves the pool ([`Pool::reserve`](crate::pool::Pool::reserve)): the
//! conversion paths answer 503 and `/readyz` is not ready meanwhile, so a Service routes around the pod. Then,
//! server-side, without the network:
//!
//! 1. warm-up: one conversion per instance at once, not counted;
//! 2. `s1`: the documents one at a time, `rounds` times over, the mean conversion time;
//! 3. `S_sat`: one conversion per instance at once, `busy` conversions in all, the mean of those that ended
//!    while every instance was still busy; both then plus h = 0.045 × `s1`, the HTTP path's CPU ([`HTTP_CPU_SHARE`]),
//!    which the probe's quiet conversions leave out; `contention = S_sat / s1` from the corrected pair;
//! 4. the disk: `disk_mib` MiB of 1 MiB direct writes (`O_DIRECT` where the file system takes it, else
//!    written and `fsync`ed) under the service's temporary directory, the file removed after; skipped on a
//!    tmpfs, which is memory, not a disk.
//!
//! The pool is given back, whole, and the answer is `tools/bench/sizing.py`'s `explain()` for these three
//! numbers ([`crate::sizing::explain`]): what `bench.py size --platform FILE` takes. Its worst-case memory is sized
//! from the pod's own `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`, the default when it is 0.
//!
//! Query parameters, all optional: `rounds` (default 5), `busy` (default 120 + 3 × pool), `disk_mib`
//! (default 1024; 0 skips the disk), `cpus` (the vCPU the formulas are worked out for; default the pool size),
//! `name` (the platform's name; default the CPU model and the vCPU count).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Map, Value, json};

use crate::allowlist;
use crate::conversion::PoolSlot;
use crate::instance::protocol::Answer;
use crate::num::{ToF64, usize_from_u64};
use crate::pool::{PoolError, Reservation, ReserveError};
use crate::problem::Problem;
use crate::sizing::{self, Platform, REPORT};
use crate::upload::{ReceivedDocument, Upload};

/// The most documents one probe takes.
pub const MAX_DOCUMENTS: usize = 16;

/// The largest document the probe takes, whatever `DOCX_TO_PDF_MAX_DOCUMENT_SIZE` is: the reference mix's
/// largest is 18.6 MiB, above the default 10 MiB.
pub const MIN_DOCUMENT_LIMIT: u64 = 64 * 1024 * 1024;

/// The HTTP path's CPU per conversion of the reference report mix over `s1` (measured: 0.041 CPU-s on 0.865 s,
/// 0.029 on 0.709 s): receiving 13.4 MiB, spooling and sniffing it, streaming 13.5 MiB of PDF back. The probe
/// converts with that path quiet; under traffic it runs on the same vCPU, in series alone and in the instances'
/// place at saturation.
pub const HTTP_CPU_SHARE: f64 = 0.045;

/// The measured `s1` and `S_sat` with the HTTP path's CPU, h = [`HTTP_CPU_SHARE`] × `s1`, added to both:
/// `(s1 + h, s_sat + h, h)`.
pub fn with_http(s1: f64, s_sat: f64) -> (f64, f64, f64) {
    let h = HTTP_CPU_SHARE * s1;
    (s1 + h, s_sat + h, h)
}

/// How long a client waits before asking again when the probe cannot run now.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// What the probe's handler reaches.
#[derive(Clone)]
pub struct ProbeState {
    /// The pool, once booted; the probe reserves it.
    pub pool: PoolSlot,
    /// How the documents are received: each up to `max(DOCX_TO_PDF_MAX_DOCUMENT_SIZE, 64 MiB)`.
    pub upload: Upload,
    /// Where the disk test writes: the service's temporary directory.
    pub scratch: PathBuf,
}

impl ProbeState {
    /// The probe over `pool`, receiving documents as `upload` does but each up to at least
    /// [`MIN_DOCUMENT_LIMIT`], writing its disk test where `upload` spools.
    pub fn new(pool: PoolSlot, upload: &Upload) -> Self {
        let mut upload = upload.clone();
        upload.max_document_size = upload.max_document_size.max(MIN_DOCUMENT_LIMIT);
        Self {
            pool,
            scratch: upload.spool_root.clone(),
            upload,
        }
    }
}

/// The probe's router: `POST /probe`; any other path is a 404 problem, another method a 405.
pub fn router(state: ProbeState) -> Router {
    Router::new()
        .route(
            "/probe",
            post(probe).fallback(|| async { Problem::MethodNotAllowed }),
        )
        .method_not_allowed_fallback(|| async { Problem::MethodNotAllowed })
        .fallback(|| async { Problem::NotFound })
        .with_state(state)
}

/// What the request asks for.
#[derive(Debug, Clone, PartialEq)]
struct Options {
    rounds: usize,
    busy: Option<usize>,
    disk_mib: u64,
    cpus: Option<usize>,
    name: Option<String>,
}

impl Options {
    fn parse(query: Option<&str>) -> Result<Self, Problem> {
        let mut options = Self {
            rounds: 5,
            busy: None,
            disk_mib: 1024,
            cpus: None,
            name: None,
        };
        for pair in query
            .unwrap_or_default()
            .split('&')
            .filter(|p| !p.is_empty())
        {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = percent_decode(value);
            let number = |max: u64| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n <= max)
                    .ok_or(Problem::BadRequest(
                        "rounds, busy, cpus and disk_mib are whole numbers within their bounds",
                    ))
            };
            match key {
                "rounds" => options.rounds = usize_from_u64(number(100)?.max(1)),
                "busy" => options.busy = Some(usize_from_u64(number(10_000)?)),
                "disk_mib" => options.disk_mib = number(65_536)?,
                "cpus" => options.cpus = Some(usize_from_u64(number(1024)?.max(1))),
                "name" => options.name = Some(value).filter(|v| !v.is_empty()),
                _ => {
                    return Err(Problem::BadRequest(
                        "the probe takes rounds, busy, disk_mib, cpus and name",
                    ));
                }
            }
        }
        Ok(options)
    }
}

/// `%XX` and `+` in a query value.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(high), Some(low)) => {
                        out.push(u8::try_from(high * 16 + low).expect("two hex digits"));
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A probe document: its file, named by its sniffed format, and its label in the answer.
struct Document {
    received: ReceivedDocument,
    input: PathBuf,
    label: String,
}

/// `POST /probe`.
async fn probe(
    State(state): State<ProbeState>,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match run(&state, uri.query(), &headers, body).await {
        Ok(answer) => (
            [(CONTENT_TYPE, "application/json")],
            serde_json::to_string_pretty(&answer).unwrap_or_default(),
        )
            .into_response(),
        Err(problem) => problem.into_response(),
    }
}

async fn run(
    state: &ProbeState,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Body,
) -> Result<Value, Problem> {
    let options = Options::parse(query)?;
    let received = state
        .upload
        .receive_all(headers, body.into_data_stream(), MAX_DOCUMENTS)
        .await
        .map_err(Problem::from)?;
    let mut documents = Vec::with_capacity(received.len());
    for (i, (received, name)) in received.into_iter().enumerate() {
        let format = match allowlist::sniff_file(&received.path()).await {
            Ok(Some(format)) => format,
            Ok(None) => return Err(Problem::UnsupportedFormat(allowlist::REFUSED)),
            Err(_) => return Err(Problem::Internal),
        };
        let input = received
            .directory()
            .join(format!("document.{}", format.extension()));
        tokio::fs::rename(received.path(), &input)
            .await
            .map_err(|_| Problem::Internal)?;
        let label = name
            .map(|n| {
                n.rsplit_once('.')
                    .map_or_else(|| n.clone(), |(stem, _)| stem.to_owned())
            })
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("document-{}", i + 1));
        documents.push(Document {
            received,
            input,
            label,
        });
    }

    let Some(pool) = state.pool.get() else {
        return Err(Problem::Unavailable {
            detail: "the service is not ready to convert documents yet",
            retry_after: RETRY_AFTER,
        });
    };
    let reservation = pool.reserve().await.map_err(|e| Problem::Unavailable {
        detail: match e {
            ReserveError::Busy => "a probe is already running",
            ReserveError::Closed => "the service is shutting down",
            ReserveError::Timeout(_) => "the conversions under way did not end in time",
        },
        retry_after: RETRY_AFTER,
    })?;
    let pool_size = reservation.size();
    let hard_mib = sizing::hard_memory_mib(pool.limits().hard_memory);
    tracing::info!(
        documents = documents.len(),
        pool = pool_size,
        rounds = options.rounds,
        "the platform probe has the pool"
    );

    // Warm-up: each instance converts once, not counted.
    let warmup: Vec<usize> = (0..pool_size).map(|i| i % documents.len()).collect();
    futures_util::future::try_join_all(
        warmup
            .iter()
            .enumerate()
            .map(|(slot, &d)| timed(&reservation, &documents[d], slot)),
    )
    .await?;

    // s1: one at a time.
    let mut alone = vec![Vec::new(); documents.len()];
    for _ in 0..options.rounds {
        for (d, document) in documents.iter().enumerate() {
            alone[d].push(timed(&reservation, document, 0).await?);
        }
    }
    let all_alone: Vec<f64> = alone.iter().flatten().copied().collect();
    let s1 = mean(&all_alone);

    // S_sat: one per instance at once.
    let busy = options.busy.unwrap_or(120 + 3 * pool_size).max(pool_size);
    let (s_sat, busy_counted) = saturated(&reservation, &documents, busy).await?;

    // The disk, still drained: nothing else writes /tmp meanwhile.
    let scratch = state.scratch.clone();
    let mib = options.disk_mib;
    let disk = tokio::task::spawn_blocking(move || disk_write_rate(&scratch, mib))
        .await
        .map_err(|_| Problem::Internal)?
        .map_err(|error| {
            tracing::error!(%error, "the probe's disk test failed");
            Problem::Internal
        })?;
    drop(reservation);

    let (s1_conversion, s_sat_conversion) = (s1, s_sat);
    let (s1, s_sat, http) = with_http(s1, s_sat);
    let contention = s_sat / s1;
    let platform = Platform {
        name: options
            .name
            .clone()
            .unwrap_or_else(|| format!("{}, {} vCPU", cpu_model(), host_cpus())),
        measured: Some(format!("probe endpoint, {}", utc_now())),
        cpus_probed: Some(pool_size),
        s1: sizing::round_to(s1, 3),
        contention: sizing::round_to(contention, 3),
        disk_mbps: disk.mbps.map(|mbps| sizing::round_to(mbps, 1)),
    };
    tracing::info!(
        s1 = platform.s1,
        contention = platform.contention,
        disk_mbps = ?platform.disk_mbps,
        "the platform probe has measured"
    );
    let mut answer = sizing::explain(
        &REPORT,
        &platform,
        options.cpus.unwrap_or(pool_size),
        hard_mib,
    );
    if let Some(Value::Object(p)) = answer.get_mut("platform") {
        // The conversions' own times: the HTTP path is not in them.
        let by_document: Map<String, Value> = documents
            .iter()
            .zip(&alone)
            .map(|(d, times)| (d.label.clone(), json!(sizing::round_to(mean(times), 3))))
            .collect();
        p.insert("pool".into(), json!(pool_size));
        p.insert("s_sat".into(), json!(sizing::round_to(s_sat, 3)));
        p.insert(
            "s1_conversion".into(),
            json!(sizing::round_to(s1_conversion, 3)),
        );
        p.insert(
            "s_sat_conversion".into(),
            json!(sizing::round_to(s_sat_conversion, 3)),
        );
        p.insert("http_cpu_s".into(), json!(sizing::round_to(http, 3)));
        p.insert("http_cpu_share".into(), json!(HTTP_CPU_SHARE));
        p.insert("by_document_s".into(), Value::Object(by_document));
        p.insert(
            "conversions".into(),
            json!({"warmup": pool_size, "alone": all_alone.len(), "busy": busy,
                   "busy_counted": busy_counted}),
        );
        p.insert("disk".into(), json!({"mib": mib, "how": disk.how}));
    }
    Ok(answer)
}

/// One conversion of `document` on the reservation, its output in `slot`'s file: its seconds, from asking
/// for an instance to the answer.
async fn timed(
    reservation: &Reservation,
    document: &Document,
    slot: usize,
) -> Result<f64, Problem> {
    let output = document
        .received
        .directory()
        .join(format!("probe-{slot}.pdf"));
    let began = Instant::now();
    let converted = reservation.convert(&document.input, &output).await;
    let seconds = began.elapsed().as_secs_f64();
    let _ = tokio::fs::remove_file(&output).await;
    match converted {
        Ok(conversion) => match conversion.answer {
            Answer::Converted { .. } => Ok(seconds),
            Answer::NotLoaded(text) => {
                tracing::warn!(backend_error = %text, "the probe could not load a document");
                Err(Problem::CannotLoad("a probe document could not be loaded"))
            }
            Answer::ExportFailed(_) | Answer::BadRequest(_) => Err(Problem::Internal),
        },
        Err(PoolError::Timeout(_)) => Err(Problem::Timeout(crate::conversion::TIMED_OUT)),
        Err(PoolError::Acquire(_)) => Err(Problem::Unavailable {
            detail: "the service is shutting down",
            retry_after: RETRY_AFTER,
        }),
        Err(PoolError::Convert(_)) => Err(Problem::Internal),
        Err(PoolError::MemoryLimit { .. }) => {
            Err(Problem::TooLarge(crate::conversion::OVER_MEMORY))
        }
    }
}

/// `busy` conversions, the documents in turn, one per instance at once: the mean time of those that ended
/// while every instance was still busy (before the first worker found nothing left to start), and how many
/// those were.
async fn saturated(
    reservation: &Reservation,
    documents: &[Document],
    busy: usize,
) -> Result<(f64, usize), Problem> {
    let next = AtomicUsize::new(0);
    let began = Instant::now();
    let first_idle = std::sync::Mutex::new(None::<Duration>);
    let worker = |slot: usize| {
        let (next, first_idle) = (&next, &first_idle);
        async move {
            let mut ended = Vec::new();
            loop {
                let n = next.fetch_add(1, Ordering::SeqCst);
                if n >= busy {
                    let mut first = first_idle
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    first.get_or_insert(began.elapsed());
                    return Ok::<_, Problem>(ended);
                }
                let seconds = timed(reservation, &documents[n % documents.len()], slot).await?;
                ended.push((began.elapsed(), seconds));
            }
        }
    };
    let ended = futures_util::future::try_join_all((0..reservation.size()).map(worker)).await?;
    let cut = first_idle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .unwrap_or(Duration::MAX);
    let counted: Vec<f64> = ended
        .iter()
        .flatten()
        .filter(|(at, _)| *at <= cut)
        .map(|(_, seconds)| *seconds)
        .collect();
    let all: Vec<f64> = ended.iter().flatten().map(|(_, s)| *s).collect();
    // With fewer conversions than instances nothing ends while all are busy: count every one.
    let times = if counted.is_empty() { all } else { counted };
    Ok((mean(&times), times.len()))
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len().max(1).to_f64()
}

/// What the disk test found.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskRate {
    /// MB/s (10^6 bytes), or `None` when not measured.
    pub mbps: Option<f64>,
    /// How it was measured, or why not.
    pub how: &'static str,
}

/// The sustained write rate of `mib` MiB written in 1 MiB blocks to a new file under `dir`, direct where the
/// file system allows it, synced, then removed. `None` for 0 MiB or on a tmpfs.
pub fn disk_write_rate(dir: &Path, mib: u64) -> std::io::Result<DiskRate> {
    use std::io::Write as _;

    if mib == 0 {
        return Ok(DiskRate {
            mbps: None,
            how: "skipped (disk_mib=0)",
        });
    }
    if is_tmpfs(dir) {
        return Ok(DiskRate {
            mbps: None,
            how: "skipped: the temporary directory is a tmpfs (memory, no disk bound)",
        });
    }
    const MIB: usize = 1 << 20;
    const ALIGN: usize = 4096;
    let scratch = tempfile::Builder::new()
        .prefix("docx-to-pdf-probe-")
        .tempdir_in(dir)?;
    let path = scratch.path().join("disk");
    let (mut file, how) = open_direct(&path)?;
    // O_DIRECT wants an aligned buffer: a slice of a larger one, at its first aligned byte.
    let buffer = vec![0x5au8; MIB + ALIGN];
    let offset = buffer.as_ptr().align_offset(ALIGN);
    let block = &buffer[offset..offset + MIB];
    let began = Instant::now();
    for _ in 0..mib {
        file.write_all(block)?;
    }
    file.sync_all()?;
    let seconds = began.elapsed().as_secs_f64();
    drop(file);
    drop(scratch);
    Ok(DiskRate {
        mbps: Some(mib.to_f64() * MIB.to_f64() / seconds / 1e6),
        how,
    })
}

/// The file opened for writing with `O_DIRECT` where the platform and the file system take it, else plainly
/// (the rate then counts the final `fsync`).
fn open_direct(path: &Path) -> std::io::Result<(std::fs::File, &'static str)> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut direct = options.clone();
        direct.custom_flags(libc::O_DIRECT);
        match direct.open(path) {
            Ok(file) => return Ok((file, "1 MiB writes with O_DIRECT, then fsync")),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {}
            Err(e) => return Err(e),
        }
    }
    Ok((
        options.open(path)?,
        "1 MiB writes, then fsync (no O_DIRECT here)",
    ))
}

/// Whether `dir` is on a tmpfs. `f_type`'s type differs between the C libraries; i64 holds the magic on each.
#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_cast)]
#[allow(unsafe_code, reason = "statfs(2), which std has no form of")]
fn is_tmpfs(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: `path` is a NUL-terminated string and `stat` a buffer of the struct statfs fills.
    let filled = unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } == 0;
    // SAFETY: statfs returned 0, so it filled `stat`; zeroed is a valid statfs anyway.
    filled && unsafe { stat.assume_init() }.f_type as i64 == TMPFS_MAGIC
}

#[cfg(not(target_os = "linux"))]
fn is_tmpfs(_: &Path) -> bool {
    false
}

/// The CPU's model name from `/proc/cpuinfo`, else the architecture.
fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, model)| model.trim().to_owned())
        })
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| std::env::consts::ARCH.to_owned())
}

/// The CPUs this process may use (the cgroup's quota included).
fn host_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// Now, `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rest) = (
        i64::try_from(seconds / 86_400).expect("u64::MAX / 86400 fits an i64"),
        seconds % 86_400,
    );
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_default_and_parse() {
        assert_eq!(
            Options::parse(None).unwrap(),
            Options {
                rounds: 5,
                busy: None,
                disk_mib: 1024,
                cpus: None,
                name: None
            }
        );
        let o = Options::parse(Some(
            "rounds=2&busy=8&disk_mib=0&cpus=4&name=Xeon+6248R%2C+8",
        ))
        .unwrap();
        assert_eq!(o.rounds, 2);
        assert_eq!(o.busy, Some(8));
        assert_eq!(o.disk_mib, 0);
        assert_eq!(o.cpus, Some(4));
        assert_eq!(o.name.as_deref(), Some("Xeon 6248R, 8"));
        assert!(Options::parse(Some("rounds=x")).is_err());
        assert!(Options::parse(Some("disk_mib=100000")).is_err());
        assert!(Options::parse(Some("what=1")).is_err());
    }

    #[test]
    fn the_disk_test_writes_and_removes_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let rate = disk_write_rate(dir.path(), 4).unwrap();
        assert!(rate.mbps.is_some_and(|r| r > 0.0), "{rate:?}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert_eq!(disk_write_rate(dir.path(), 0).unwrap().mbps, None);
    }

    /// A tmpfs is memory: the disk test would fill it, so it is skipped.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_disk_test_skips_a_tmpfs() {
        let shm = Path::new("/dev/shm");
        if !is_tmpfs(shm) {
            eprintln!("/dev/shm is not a tmpfs here: not tried");
            return;
        }
        assert_eq!(disk_write_rate(shm, 1).unwrap().mbps, None);
    }

    /// The answer's `s1` is 1.045 × the measured mean, `S_sat` the measured plus 0.045 × that mean.
    #[test]
    fn the_http_path_adds_its_share_of_s1_to_both() {
        let (s1, s_sat, h) = with_http(0.865, 1.0);
        assert!((h - 0.045 * 0.865).abs() < 1e-12, "h {h}");
        assert!((s1 - 1.045 * 0.865).abs() < 1e-12, "s1 {s1}");
        assert!(
            (s_sat - (1.0 + 0.045 * 0.865)).abs() < 1e-12,
            "S_sat {s_sat}"
        );
        assert_eq!(HTTP_CPU_SHARE, 0.045);
    }

    #[test]
    fn utc_now_is_an_iso_timestamp() {
        let now = utc_now();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.starts_with("20") && now.ends_with('Z'), "{now}");
    }
}
