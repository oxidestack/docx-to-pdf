//! The JSON log lines on stdout carry the OpenTelemetry `trace_id` and `span_id` of the span they were
//! emitted in, so a log line leads to its trace (`docs/guide/operations.md`, "Logs").

use std::io;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use docx_to_pdf::http_telemetry::{HttpTelemetry, instrument};
use docx_to_pdf::telemetry::{TelemetryBuilder, json_log_layer};
use opentelemetry::trace::SpanKind;
use opentelemetry_sdk::logs::InMemoryLogExporter;
use opentelemetry_sdk::trace::InMemorySpanExporter;
use tower::ServiceExt as _;
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
    fn json(&self) -> Vec<serde_json::Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }
}

fn line<'a>(lines: &'a [serde_json::Value], message: &str) -> &'a serde_json::Value {
    lines
        .iter()
        .find(|l| l["message"] == message)
        .unwrap_or_else(|| panic!("no line {message:?} in {lines:?}"))
}

const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

#[tokio::test(flavor = "current_thread")]
async fn a_line_carries_the_ids_of_its_span_and_a_line_outside_any_span_carries_none() {
    let spans = InMemorySpanExporter::default();
    let telemetry = TelemetryBuilder::new()
        .with_simple_span_exporter(spans.clone())
        .with_simple_log_exporter(InMemoryLogExporter::default())
        .build()
        .unwrap();
    let out = Lines::default();
    let subscriber = tracing_subscriber::registry()
        .with(json_log_layer(out.clone()))
        .with(telemetry.layers());
    let _guard = tracing::subscriber::set_default(subscriber);

    let app = instrument(
        Router::new().route(
            "/documents/{id}",
            get(|| async {
                tracing::info!("inside the request");
                "ok"
            }),
        ),
        HttpTelemetry::new(&telemetry),
    );
    tracing::info!("outside any span");
    let request = Request::get("/documents/1")
        .header("traceparent", format!("00-{TRACE}-00f067aa0ba902b7-01"))
        .body(Body::empty())
        .unwrap();
    app.oneshot(request).await.unwrap();
    telemetry.force_flush();

    let finished = spans.get_finished_spans().unwrap();
    let server = finished
        .iter()
        .find(|s| s.span_kind == SpanKind::Server)
        .expect("the exported server span");
    assert_eq!(server.span_context.trace_id().to_string(), TRACE);

    let lines = out.json();
    let inside = line(&lines, "inside the request");
    assert_eq!(inside["trace_id"], TRACE, "{inside}");
    assert_eq!(
        inside["span_id"],
        server.span_context.span_id().to_string(),
        "{inside}"
    );
    let outside = line(&lines, "outside any span");
    assert!(outside.get("trace_id").is_none(), "{outside}");
    assert!(outside.get("span_id").is_none(), "{outside}");
}
