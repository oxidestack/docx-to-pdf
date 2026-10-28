//! A conversion: the received document, named by its sniffed format, goes to a pool instance as a file in the
//! conversion's temporary directory; the instance loads it (no lock file, input untouched: `UseLocking=false` and
//! LOK's forced headless mode, since LOK's load cannot ask for `ReadOnly` or `Hidden`) and stores it with
//! `writer_pdf_Export` as a PDF in the same directory. The directory is the [`ReceivedDocument`]'s and goes with it
//! on every path, a dropped request included.
//!
//! A converted PDF is answered from its file, never held in memory, since memory is the service's first priority: its
//! `%PDF-` header is checked from the file's first bytes before the answer begins, then the body streams the file a
//! bounded buffer ([`FRAME_SIZE`]) at a time. The body owns the [`ReceivedDocument`], so the directory stays while
//! the body is under way and is removed once the body has been sent, or when the body is dropped with a client that
//! went away in the middle of it.
//!
//! Every conversion that reaches this module ends in exactly one `outcome` of `docx_to_pdf.conversions` and
//! `docx_to_pdf.conversion.duration`, `cancelled` among them: a request future dropped before its answer (the
//! client went away) counts when it is dropped, and the lease dropped with it kills and replaces the instance
//! at once, since LibreOfficeKit cannot interrupt a running load or export and the slot would otherwise
//! convert for nobody. Past the conversion timeout the pool retires the instance (`timeout`) and the answer is
//! 504.

use std::io::SeekFrom;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use axum::body::Body;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use bytes::{Bytes, BytesMut};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use opentelemetry::trace::{Span as _, Status, Tracer as _};
use tokio::fs::File;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::allowlist::Format;
use crate::instance::driver::ConvertError;
use crate::instance::protocol::Answer;
use crate::pool::{AcquireError, Conversion, Pool, PoolError};
use crate::problem::Problem;
use crate::upload::ReceivedDocument;

/// What a conversion ended in: the `outcome` of `docx_to_pdf.conversions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A PDF was answered.
    Success,
    /// The document's format is outside the allowlist (415); no instance was touched.
    Unsupported,
    /// The backend could not load an allowed document (422).
    NotLoaded,
    /// The instance died or answered garbage, and so did the retry's (500).
    DeadInstance,
    /// Past the conversion timeout (504).
    Timeout,
    /// The instance passed the hard memory limit and was killed (413).
    MemoryLimit,
    /// The client went away before the answer.
    Cancelled,
    /// Anything else (500).
    Error,
}

impl Outcome {
    /// The attribute value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Unsupported => "unsupported",
            Self::NotLoaded => "not_loaded",
            Self::DeadInstance => "dead_instance",
            Self::Timeout => "timeout",
            Self::MemoryLimit => "memory_limit",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }
}

/// How long a client waits before asking again when no instance was lent.
pub const RETRY_AFTER: Duration = Duration::from_secs(10);

/// The 422 detail. It does not name the backend, so the answer survives a change of backend; the backend's own error
/// text goes to the log only: it may name the file.
pub const CANNOT_LOAD: &str = "the document could not be loaded";

/// The 504 detail.
pub const TIMED_OUT: &str = "the conversion did not finish within the conversion timeout";

/// The 413 detail for a conversion cut short by the hard memory limit. Like every detail it names
/// neither the backend nor a number: the limit is the deployment's.
pub const OVER_MEMORY: &str = "the document needs more memory to convert than the service allows";

/// Seconds, for `docx_to_pdf.conversion.duration`: from a one-page letter to a report well over a minute.
const SECONDS_BUCKETS: &[f64] = &[
    0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

/// PDF sizes from 1 KiB to 256 MiB, as the document sizes, past the largest allowed document.
const SIZE_BUCKETS: &[f64] = &[
    1024.0,
    4096.0,
    16384.0,
    65536.0,
    262_144.0,
    1_048_576.0,
    4_194_304.0,
    16_777_216.0,
    33_554_432.0,
    67_108_864.0,
    134_217_728.0,
    268_435_456.0,
];

/// The conversion instruments; clones share them.
#[derive(Clone)]
pub struct Conversions {
    count: Counter<u64>,
    duration: Histogram<f64>,
    pdf_size: Histogram<u64>,
}

impl Conversions {
    /// Registers `docx_to_pdf.conversions`, `docx_to_pdf.conversion.duration` and `docx_to_pdf.pdf.size`.
    pub fn new(meter: &Meter) -> Self {
        Self {
            count: meter
                .u64_counter("docx_to_pdf.conversions")
                .with_description("Conversions, by what they ended in and whether they took the retry")
                .build(),
            duration: meter
                .f64_histogram("docx_to_pdf.conversion.duration")
                .with_unit("s")
                .with_description(
                    "The backend's own time, load + export; where it gave none, the time from asking for an \
                     instance to the end",
                )
                .with_boundaries(SECONDS_BUCKETS.to_vec())
                .build(),
            pdf_size: meter
                .u64_histogram("docx_to_pdf.pdf.size")
                .with_unit("By")
                .with_description("Size of each answered PDF")
                .with_boundaries(SIZE_BUCKETS.to_vec())
                .build(),
        }
    }

    /// A document refused by the allowlist: `outcome` = `unsupported`, no duration (the backend never ran).
    pub fn unsupported(&self) {
        self.count.add(1, &attributes(Outcome::Unsupported, false));
    }

    fn end(&self, outcome: Outcome, retried: bool, duration: Duration) {
        self.count.add(1, &attributes(outcome, retried));
        self.duration.record(
            duration.as_secs_f64(),
            &[KeyValue::new("outcome", outcome.as_str())],
        );
    }

    /// Converts `document` of `format` on `pool`: the PDF, answered from its file in `document`'s directory,
    /// or the problem to answer (`document` and its directory dropped with it).
    pub async fn convert(
        &self,
        pool: &Pool,
        document: ReceivedDocument,
        format: Format,
    ) -> Result<Pdf, Problem> {
        let input = document
            .directory()
            .join(format!("document.{}", format.extension()));
        if let Err(error) = tokio::fs::rename(document.path(), &input).await {
            tracing::error!(%error, "the spooled document could not be named by its format");
            self.end(Outcome::Error, false, Duration::ZERO);
            return Err(Problem::Internal);
        }
        let output = document.directory().join("document.pdf");
        let pending = Pending {
            conversions: self,
            began: Instant::now(),
            began_at: SystemTime::now(),
            ended: false,
        };
        let converted = pool.convert(&input, &output).await;
        let pdf = pending.finish(converted, &output).await?;
        Ok(Pdf {
            file: pdf.file,
            size: pdf.size,
            document,
        })
    }
}

fn attributes(outcome: Outcome, retried: bool) -> [KeyValue; 2] {
    [
        KeyValue::new("outcome", outcome.as_str()),
        KeyValue::new("retried", retried),
    ]
}

/// A conversion under way. Dropped before it ends (the request future dropped by a client disconnect), it
/// counts as `cancelled`.
struct Pending<'a> {
    conversions: &'a Conversions,
    began: Instant,
    /// `began` on the wall clock, which spans are timed on.
    began_at: SystemTime,
    ended: bool,
}

impl Pending<'_> {
    async fn finish(
        mut self,
        converted: Result<Conversion, PoolError>,
        output: &Path,
    ) -> Result<OpenPdf, Problem> {
        let elapsed = self.began.elapsed();
        let now = SystemTime::now();
        let (outcome, retried, duration, answer) = match converted {
            Ok(Conversion { answer, retried }) => match answer {
                Answer::Converted { load, export } => {
                    // The answer comes as the export ends; the load ended where the export began.
                    let export_began = self.not_before(now.checked_sub(export));
                    let load_began = self.not_before(export_began.checked_sub(load));
                    phase_span("backend.load", load_began, export_began, None);
                    phase_span("backend.export", export_began, now, None);
                    match open_pdf(output).await {
                        Ok(pdf) => {
                            self.conversions.pdf_size.record(pdf.size, &[]);
                            (Outcome::Success, retried, load + export, Ok(pdf))
                        }
                        Err(error) => {
                            tracing::error!(%error, "the instance answered ok but its PDF cannot be read");
                            (
                                Outcome::Error,
                                retried,
                                load + export,
                                Err(Problem::Internal),
                            )
                        }
                    }
                }
                Answer::NotLoaded(text) => {
                    phase_span("backend.load", self.began_at, now, Some("not_loaded"));
                    tracing::warn!(backend_error = %text, "the backend could not load the document");
                    (
                        Outcome::NotLoaded,
                        retried,
                        elapsed,
                        Err(Problem::CannotLoad(CANNOT_LOAD)),
                    )
                }
                Answer::ExportFailed(text) => {
                    phase_span("backend.export", self.began_at, now, Some("export_failed"));
                    tracing::error!(backend_error = %text, "the backend could not export the PDF");
                    (Outcome::Error, retried, elapsed, Err(Problem::Internal))
                }
                Answer::BadRequest(text) => {
                    tracing::error!(error = %text, "the instance refused the request");
                    (Outcome::Error, retried, elapsed, Err(Problem::Internal))
                }
            },
            Err(PoolError::Acquire(error)) => {
                // No conversion took place: `docx_to_pdf.pool.wait.duration{outcome=rejected}` counts it.
                self.ended = true;
                return Err(Problem::Unavailable {
                    detail: unavailable(error),
                    retry_after: RETRY_AFTER,
                });
            }
            Err(PoolError::Timeout(timeout)) => {
                // The pool has retired the instance with the reason `timeout` (killed with its process
                // group, profile deleted, replaced); the directory goes with the request.
                tracing::error!(
                    timeout_seconds = timeout.as_secs_f64(),
                    "the conversion exceeded its timeout"
                );
                (
                    Outcome::Timeout,
                    false,
                    elapsed,
                    Err(Problem::Timeout(TIMED_OUT)),
                )
            }
            Err(PoolError::MemoryLimit { resident, limit }) => {
                // The pool has retired the instance with the reason `memory_limit` (killed with its process
                // group, profile deleted, replaced) and did not retry: the document would kill another one.
                tracing::error!(
                    rss_bytes = resident,
                    hard_memory_bytes = limit,
                    "the conversion passed the hard memory limit"
                );
                (
                    Outcome::MemoryLimit,
                    false,
                    elapsed,
                    Err(Problem::TooLarge(OVER_MEMORY)),
                )
            }
            Err(PoolError::Convert(error)) => {
                // The pool retries once on `Dead`, so a `Dead` here is the retry's.
                let (outcome, retried) = match error {
                    ConvertError::Dead(_) => (Outcome::DeadInstance, true),
                    ConvertError::BadAnswer(_) => (Outcome::DeadInstance, false),
                    ConvertError::Request(_) | ConvertError::Unusable => (Outcome::Error, false),
                };
                tracing::error!(%error, "the conversion got no answer");
                (outcome, retried, elapsed, Err(Problem::Internal))
            }
        };
        self.ended = true;
        self.conversions.end(outcome, retried, duration);
        answer
    }
}

impl Pending<'_> {
    /// `time`, or the conversion's start if it lies before it (or underflowed): a phase is never older than
    /// the conversion it belongs to.
    fn not_before(&self, time: Option<SystemTime>) -> SystemTime {
        time.map_or(self.began_at, |time| time.max(self.began_at))
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if !self.ended {
            tracing::info!("the client went away before the answer; the conversion is cancelled");
            self.conversions
                .end(Outcome::Cancelled, false, self.began.elapsed());
        }
    }
}

/// A `backend.load` or `backend.export` span, under the request's span, from `start` to
/// `end`. The instance reports each phase's time only with its answer, so the span is built after the fact
/// with those timestamps, from the tracer behind the subscriber's OpenTelemetry layer; with no
/// such layer there is nothing to export to and no span. A failure sets the span's status to error and its
/// `error.type`.
fn phase_span(name: &'static str, start: SystemTime, end: SystemTime, error: Option<&'static str>) {
    let Some(tracer) = crate::telemetry::current_tracer() else {
        return;
    };
    let parent = tracing::Span::current().context();
    let mut span = tracer
        .span_builder(name)
        .with_start_time(start)
        .start_with_context(&tracer, &parent);
    if let Some(error) = error {
        span.set_attribute(KeyValue::new("error.type", error));
        span.set_status(Status::error(""));
    }
    span.end_with_timestamp(end);
}

/// The most bytes one frame of a PDF's body carries, and so the answer's buffer.
pub const FRAME_SIZE: usize = 64 * 1024;

/// What every PDF starts with.
const PDF_HEADER: &[u8] = b"%PDF-";

/// A PDF's file, open at its start, its header checked.
struct OpenPdf {
    file: File,
    size: u64,
}

/// Opens the instance's output and checks its header from its first bytes; nothing else of it is read.
async fn open_pdf(path: &Path) -> std::io::Result<OpenPdf> {
    let mut file = File::open(path).await?;
    let size = file.metadata().await?.len();
    let mut header = [0u8; PDF_HEADER.len()];
    let mut read = 0;
    while read < header.len() {
        match file.read(&mut header[read..]).await? {
            0 => break,
            n => read += n,
        }
    }
    if &header[..read] != PDF_HEADER {
        return Err(std::io::Error::other("the output is not a PDF"));
    }
    file.seek(SeekFrom::Start(0)).await?;
    Ok(OpenPdf { file, size })
}

/// A converted PDF: its file, open at its start, in the conversion's temporary directory, which this owns.
/// Answered, it is `200 application/pdf` with `Content-Length` and a body read from the file a
/// [`FRAME_SIZE`] buffer at a time; the directory goes when the body has been sent or is dropped.
pub struct Pdf {
    file: File,
    size: u64,
    document: ReceivedDocument,
}

impl Pdf {
    /// The PDF's size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl IntoResponse for Pdf {
    fn into_response(self) -> Response {
        let size = self.size;
        let body = futures_util::stream::unfold(
            Some((self.file, self.document)),
            |state| async move {
                let (mut file, document) = state?;
                let mut buffer = BytesMut::with_capacity(FRAME_SIZE);
                match file.read_buf(&mut buffer).await {
                    // The end of the file: the state, the directory with it, is dropped here.
                    Ok(0) => None,
                    Ok(_) => Some((
                        Ok::<Bytes, std::io::Error>(buffer.freeze()),
                        Some((file, document)),
                    )),
                    Err(error) => {
                        tracing::error!(%error, "the PDF could not be read while it was being sent");
                        Some((Err(error), None))
                    }
                }
            },
        );
        (
            [
                (CONTENT_TYPE, "application/pdf".to_string()),
                (CONTENT_LENGTH, size.to_string()),
            ],
            Body::from_stream(body),
        )
            .into_response()
    }
}

pub(crate) fn unavailable(error: AcquireError) -> &'static str {
    match error {
        AcquireError::Timeout => "no instance came free within the wait limit",
        AcquireError::QueueFull => "too many requests are already waiting for an instance",
        AcquireError::Closed => "the service is shutting down",
        AcquireError::Reserved => {
            "the service is measuring its platform; another replica can convert meanwhile"
        }
    }
}

/// Where the booted pool goes, so the router can serve (`/livez`, `/readyz`) while the pool starts.
#[derive(Clone, Default)]
pub struct PoolSlot(std::sync::Arc<std::sync::OnceLock<Pool>>);

impl PoolSlot {
    /// Fills the slot once; a second pool is refused and returned.
    pub fn set(&self, pool: Pool) -> Result<(), Pool> {
        self.0.set(pool)
    }

    /// The pool, once booted.
    pub fn get(&self) -> Option<&Pool> {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No client-facing detail names the backend: the answers survive a change of backend and do not show which
    /// one runs.
    #[test]
    fn no_detail_names_the_backend() {
        let details = [
            CANNOT_LOAD,
            TIMED_OUT,
            OVER_MEMORY,
            unavailable(AcquireError::Timeout),
            unavailable(AcquireError::QueueFull),
            unavailable(AcquireError::Closed),
            unavailable(AcquireError::Reserved),
        ];
        for detail in details {
            let lower = detail.to_lowercase();
            for word in ["libreoffice", "lok", "soffice", "office"] {
                assert!(
                    !lower.contains(word),
                    "{detail:?} names the backend ({word})"
                );
            }
        }
    }
}
