//! The access log: one `info` line on standard output when a request ends, inside its span, so a deployment
//! without OTLP still sees every request (`docs/guide/operations.md`, "Logs"). The probes, `/livez`, `/readyz`
//! and `/metrics`, have none at `info`; `RUST_LOG=docx_to_pdf::access_log=debug` shows them.
//!
//! No LibreOffice needed.

use std::io;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::http_telemetry::{ACCESS_LOG_TARGET, HttpTelemetry, instrument};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder, json_log_layer};
use http_body_util::BodyExt as _;
use opentelemetry::trace::SpanKind;
use opentelemetry_sdk::logs::InMemoryLogExporter;
use opentelemetry_sdk::trace::InMemorySpanExporter;
use serde_json::Value;
use tower::ServiceExt as _;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;

/// Every line the layer writes, kept in memory.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl io::Write for Lines {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Lines {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Lines {
    /// The access-log lines written so far.
    fn requests(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .filter(|l| l["target"] == ACCESS_LOG_TARGET)
            .collect()
    }
}

/// Telemetry with in-memory exporters, and the stdout JSON layer filtered by `filter` as `RUST_LOG` would.
struct Harness {
    telemetry: Telemetry,
    spans: InMemorySpanExporter,
    out: Lines,
    _guard: tracing::subscriber::DefaultGuard,
}

fn harness(filter: &str) -> Harness {
    let spans = InMemorySpanExporter::default();
    let telemetry = TelemetryBuilder::new()
        .with_simple_span_exporter(spans.clone())
        .with_simple_log_exporter(InMemoryLogExporter::default())
        .build()
        .unwrap();
    let out = Lines::default();
    let subscriber = tracing_subscriber::registry()
        .with(json_log_layer(out.clone()).with_filter(EnvFilter::new(filter)))
        .with(telemetry.layers());
    let guard = tracing::subscriber::set_default(subscriber);
    Harness {
        telemetry,
        spans,
        out,
        _guard: guard,
    }
}

fn routes() -> Router {
    Router::new()
        .route(
            "/documents/{id}",
            post(|body: String| async move { format!("got {}", body.len()) }),
        )
        .route(
            "/broken",
            get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route("/livez", get(|| async { "ok" }))
}

/// Sends `request` and reads its answer to the end, as hyper does.
async fn send(app: &Router, request: Request<Body>) -> StatusCode {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    response.into_body().collect().await.unwrap();
    status
}

fn get_req(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

#[tokio::test(flavor = "current_thread")]
async fn a_request_leaves_one_info_line_with_its_outcome_and_its_spans_ids() {
    let h = harness("info");
    let app = instrument(routes(), HttpTelemetry::new(&h.telemetry));
    let request = Request::post("/documents/42")
        .header("traceparent", format!("00-{TRACE}-00f067aa0ba902b7-01"))
        .body(Body::from("hello, world"))
        .unwrap();
    assert_eq!(send(&app, request).await, StatusCode::OK);
    h.telemetry.force_flush();

    let lines = h.out.requests();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert_eq!(line["level"], "INFO", "{line}");
    assert_eq!(line["message"], "request", "{line}");
    assert_eq!(line["http.request.method"], "POST", "{line}");
    assert_eq!(line["http.route"], "/documents/{id}", "{line}");
    assert_eq!(line["http.response.status_code"], 200, "{line}");
    assert_eq!(line["http.request.body.size"], 12, "{line}");
    // "got 12"
    assert_eq!(line["http.response.body.size"], 6, "{line}");
    let duration = line["http.server.request.duration"]
        .as_f64()
        .unwrap_or_else(|| panic!("a duration in seconds: {line}"));
    assert!((0.0..10.0).contains(&duration), "{line}");
    assert!(line.get("error.type").is_none(), "{line}");

    let finished = h.spans.get_finished_spans().unwrap();
    let server = finished
        .iter()
        .find(|s| s.span_kind == SpanKind::Server)
        .expect("the exported server span");
    assert_eq!(line["trace_id"], TRACE, "{line}");
    assert_eq!(
        line["span_id"],
        server.span_context.span_id().to_string(),
        "{line}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_failed_request_carries_its_error_type() {
    let h = harness("info");
    let app = instrument(routes(), HttpTelemetry::new(&h.telemetry));
    send(&app, get_req("/broken")).await;

    let lines = h.out.requests();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["http.response.status_code"], 500, "{}", lines[0]);
    assert_eq!(lines[0]["error.type"], "500", "{}", lines[0]);
    assert_eq!(lines[0]["http.request.body.size"], 0, "{}", lines[0]);
}

#[tokio::test(flavor = "current_thread")]
async fn an_unmatched_request_is_logged_without_a_route() {
    let h = harness("info");
    let app = instrument(routes(), HttpTelemetry::new(&h.telemetry));
    send(&app, get_req("/nowhere")).await;

    let lines = h.out.requests();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["http.response.status_code"], 404, "{}", lines[0]);
    assert!(lines[0].get("http.route").is_none(), "{}", lines[0]);
}

#[tokio::test(flavor = "current_thread")]
async fn turned_off_no_request_is_logged() {
    let h = harness("info");
    let app = instrument(
        routes(),
        HttpTelemetry::new(&h.telemetry).with_access_log(false),
    );
    send(&app, get_req("/broken")).await;
    assert_eq!(h.out.requests(), Vec::<Value>::new());
}

#[tokio::test(flavor = "current_thread")]
async fn the_services_probes_are_not_logged_at_info() {
    let h = harness("info");
    let app = router(AppState::new(Readiness::new(), &h.telemetry));
    for probe in ["/livez", "/readyz", "/metrics"] {
        send(&app, get_req(probe)).await;
    }
    assert_eq!(h.out.requests(), Vec::<Value>::new());

    send(&app, get_req("/nowhere")).await;
    assert_eq!(h.out.requests().len(), 1, "a request that is not a probe");
}

#[tokio::test(flavor = "current_thread")]
async fn a_probe_is_logged_at_debug_when_the_access_log_target_asks_for_it() {
    let h = harness(&format!("info,{ACCESS_LOG_TARGET}=debug"));
    let app = instrument(
        routes(),
        HttpTelemetry::new(&h.telemetry).with_probe_routes(&["/livez"]),
    );
    send(&app, get_req("/livez")).await;

    let lines = h.out.requests();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["level"], "DEBUG", "{}", lines[0]);
    assert_eq!(lines[0]["http.route"], "/livez", "{}", lines[0]);
    assert_eq!(lines[0]["http.response.status_code"], 200, "{}", lines[0]);
}
