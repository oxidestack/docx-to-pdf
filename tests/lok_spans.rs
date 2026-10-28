//! The `backend.load` and `backend.export` spans of a conversion, in an
//! exported trace, sit where their phases ran: load starts no earlier than the request, ends no later than
//! export starts, and each lasts the time the instance reported for its phase. A phase that fails still marks
//! its span as an error with its `error.type`.
//!
//! The instance is a stand-in (`sh` speaking the instance protocol), so this runs everywhere; the real
//! instance's answer is the same line (`src/instance/protocol.rs`).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use docx_to_pdf::upload::Upload;
use http_body_util::BodyExt;
use opentelemetry::Value;
use opentelemetry::trace::{SpanKind, Status};
use opentelemetry_sdk::logs::InMemoryLogExporter;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

/// The load and export times the stand-in reports, in milliseconds; it spends their sum before answering.
const LOAD_MS: u64 = 300;
const EXPORT_MS: u64 = 200;

/// How far a span's length may be from the reported time: the answer's trip through the pipe and the pool.
const MARGIN: Duration = Duration::from_millis(30);

/// A stand-in instance: ready at once; for each request, sleeps for load + export, then answers `ok` with
/// those times and a PDF at the output, or `not_loaded` when the marker file `not-loaded` exists.
fn script(markers: &Path) -> String {
    format!(
        r#"
        printf 'ready\t1\t%s\n' "$$"
        tab="$(printf '\t')"
        while IFS="$tab" read -r cmd input output; do
            if [ -f '{markers}/not-loaded' ]; then
                sleep 0.2
                printf 'not_loaded\tthe stand-in refuses it\n'
                continue
            fi
            sleep 0.5
            printf '%%PDF-1.4\n%%%%EOF\n' > "$output"
            printf 'ok\t{LOAD_MS}\t{EXPORT_MS}\n'
        done
        "#,
        markers = markers.display()
    )
}

struct Harness {
    app: Router,
    telemetry: Telemetry,
    spans: InMemorySpanExporter,
    logs: InMemoryLogExporter,
    markers: PathBuf,
    _dir: tempfile::TempDir,
    _guard: tracing::subscriber::DefaultGuard,
}

/// The service's router on a one-instance stand-in pool, its spans captured in memory, on this thread.
async fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let (profiles, spool, markers) = (
        dir.path().join("profiles"),
        dir.path().join("spool"),
        dir.path().join("markers"),
    );
    for d in [&profiles, &spool, &markers] {
        std::fs::create_dir(d).unwrap();
    }
    let spans = InMemorySpanExporter::default();
    let logs = InMemoryLogExporter::default();
    let telemetry = TelemetryBuilder::new()
        .with_otlp_from(|_| None)
        .with_simple_span_exporter(spans.clone())
        .with_simple_log_exporter(logs.clone())
        .build()
        .unwrap();
    let guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(telemetry.layers()));
    let readiness = Readiness::new();
    let launcher = Launcher::with_args(
        "/bin/sh",
        ["-c".to_string(), script(&markers), "sh".to_string()],
        &profiles,
    );
    let pool = Pool::start(
        launcher,
        PoolSettings {
            size: 1,
            wait_timeout: Duration::from_secs(10),
            max_waiting: 4,
            start_timeout: Duration::from_secs(10),
        },
        readiness.clone(),
        &telemetry.meter(),
    )
    .await
    .unwrap();
    let mut upload = Upload::new(10 * 1024 * 1024);
    upload.spool_root = spool;
    let state = AppState::new(readiness, &telemetry).with_upload(upload);
    state.set_pool(pool);
    Harness {
        app: router(state),
        telemetry,
        spans,
        logs,
        markers,
        _dir: dir,
        _guard: guard,
    }
}

impl Harness {
    /// Posts upstream's ex1 to `/pdf` and reads the whole answer.
    async fn post(&self) -> StatusCode {
        let document = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("upstream/docx-to-pdf/examples/ex1/input.docx"),
        )
        .unwrap();
        let mut body = b"--B0042\r\nContent-Disposition: form-data; name=\"document\"; filename=\"input.docx\"\r\n\r\n".to_vec();
        body.extend_from_slice(&document);
        body.extend_from_slice(b"\r\n--B0042--\r\n");
        let request = Request::post("/pdf")
            .header("content-type", "multipart/form-data; boundary=B0042")
            .body(Body::from(body))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        response.into_body().collect().await.unwrap();
        status
    }

    fn finished(&self) -> Vec<SpanData> {
        self.telemetry.force_flush();
        self.spans.get_finished_spans().unwrap()
    }
}

fn one<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
    let found: Vec<_> = spans.iter().filter(|s| s.name == name).collect();
    assert_eq!(found.len(), 1, "one {name} span: {spans:#?}");
    found[0]
}

fn request(spans: &[SpanData]) -> &SpanData {
    let found: Vec<_> = spans
        .iter()
        .filter(|s| s.span_kind == SpanKind::Server)
        .collect();
    assert_eq!(found.len(), 1, "one server span: {spans:#?}");
    found[0]
}

fn length(span: &SpanData) -> Duration {
    span.end_time.duration_since(span.start_time).unwrap()
}

fn close_to(actual: Duration, expected: Duration) -> bool {
    actual.abs_diff(expected) <= MARGIN
}

fn attribute(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

fn at(time: SystemTime) -> Duration {
    time.duration_since(SystemTime::UNIX_EPOCH).unwrap()
}

#[tokio::test]
async fn load_then_export_each_lasting_its_reported_time_inside_the_request() {
    let harness = harness().await;
    assert_eq!(harness.post().await, StatusCode::OK);
    let spans = harness.finished();
    let (request, load, export) = (
        request(&spans),
        one(&spans, "backend.load"),
        one(&spans, "backend.export"),
    );
    let times = format!(
        "request {:?}..{:?}, load {:?}..{:?}, export {:?}..{:?}",
        at(request.start_time),
        at(request.end_time),
        at(load.start_time),
        at(load.end_time),
        at(export.start_time),
        at(export.end_time)
    );
    assert!(load.start_time >= request.start_time, "{times}");
    assert!(load.end_time <= export.start_time, "{times}");
    assert!(export.end_time <= request.end_time, "{times}");
    assert!(
        close_to(length(load), Duration::from_millis(LOAD_MS)),
        "load lasts {:?}: {times}",
        length(load)
    );
    assert!(
        close_to(length(export), Duration::from_millis(EXPORT_MS)),
        "export lasts {:?}: {times}",
        length(export)
    );
    for phase in [load, export] {
        assert_eq!(phase.parent_span_id, request.span_context.span_id());
        assert_eq!(
            phase.span_context.trace_id(),
            request.span_context.trace_id()
        );
        assert_eq!(phase.status, Status::Unset);
        assert_eq!(attribute(phase, "error.type"), None);
    }
}

#[tokio::test]
async fn a_load_that_fails_is_an_error_span_with_its_error_type() {
    let harness = harness().await;
    std::fs::write(harness.markers.join("not-loaded"), "").unwrap();
    assert_eq!(harness.post().await, StatusCode::UNPROCESSABLE_ENTITY);
    let spans = harness.finished();
    let (request, load) = (request(&spans), one(&spans, "backend.load"));
    assert!(
        spans.iter().all(|s| s.name != "backend.export"),
        "{spans:#?}"
    );
    assert!(matches!(load.status, Status::Error { .. }), "{load:#?}");
    assert_eq!(
        attribute(load, "error.type"),
        Some(Value::from("not_loaded"))
    );
    assert_eq!(load.parent_span_id, request.span_context.span_id());
    assert!(load.start_time >= request.start_time);
    assert!(load.end_time <= request.end_time);
}

/// The log record of a load that fails names the backend, not the program behind it, in its
/// field and its message, so a log query survives a change of backend.
#[tokio::test]
async fn a_load_that_fails_is_logged_with_the_backend_error() {
    let harness = harness().await;
    std::fs::write(harness.markers.join("not-loaded"), "").unwrap();
    assert_eq!(harness.post().await, StatusCode::UNPROCESSABLE_ENTITY);
    harness.telemetry.force_flush();
    let logs = harness.logs.get_emitted_logs().unwrap();
    let log = logs
        .iter()
        .find(|l| {
            l.record
                .attributes_iter()
                .any(|(key, _)| key.as_str() == "backend_error")
        })
        .unwrap_or_else(|| panic!("a log record with backend_error: {logs:#?}"));
    let value = log
        .record
        .attributes_iter()
        .find(|(key, _)| key.as_str() == "backend_error")
        .map(|(_, value)| format!("{value:?}"));
    assert!(
        value.is_some_and(|v| v.contains("the stand-in refuses it")),
        "{log:#?}"
    );
    let body = format!("{:?}", log.record.body());
    assert!(
        body.contains("the backend could not load the document"),
        "{body}"
    );
    // Nothing in the records or their resource says which backend runs: telemetry does not advertise what runs
    // inside.
    let everything = format!("{logs:?}").to_lowercase();
    assert!(!everything.contains("libreoffice"), "{logs:#?}");
}
