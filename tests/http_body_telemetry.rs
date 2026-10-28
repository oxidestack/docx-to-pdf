//! The request's telemetry ends with its response body, not when the handler returns, since a streamed PDF
//! is sent after it: `http.server.request.duration` covers the body's transfer, `http.server.active_requests` counts the
//! request until the body is gone, and the server span ends with it. A body that the client stops reading
//! keeps the status that was sent and gets `error.type` = `client_disconnected`.

use std::io;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::routing::get;
use docx_to_pdf::http_telemetry::{HttpTelemetry, instrument};
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use futures_util::stream;
use http_body_util::BodyExt as _;
use opentelemetry::Value;
use opentelemetry::trace::{SpanKind, Status};
use opentelemetry_sdk::logs::InMemoryLogExporter;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tower::ServiceExt as _;
use tracing_subscriber::layer::SubscriberExt as _;

struct Harness {
    telemetry: Telemetry,
    spans: InMemorySpanExporter,
    logs: InMemoryLogExporter,
    _guard: tracing::subscriber::DefaultGuard,
}

fn harness() -> Harness {
    let spans = InMemorySpanExporter::default();
    let logs = InMemoryLogExporter::default();
    let telemetry = TelemetryBuilder::new()
        .with_simple_span_exporter(spans.clone())
        .with_simple_log_exporter(logs.clone())
        .build()
        .unwrap();
    let subscriber = tracing_subscriber::registry().with(telemetry.layers());
    let guard = tracing::subscriber::set_default(subscriber);
    Harness {
        telemetry,
        spans,
        logs,
        _guard: guard,
    }
}

const PAUSE: Duration = Duration::from_millis(300);

/// Two chunks with a pause between them; an event is emitted while the body is being polled.
fn slow_body() -> Body {
    Body::from_stream(stream::unfold(0, |n| async move {
        match n {
            0 => Some((Ok::<_, io::Error>(Bytes::from_static(b"first")), 1)),
            1 => {
                tokio::time::sleep(PAUSE).await;
                tracing::info!("streaming the second chunk");
                Some((Ok(Bytes::from_static(b"second")), 2))
            }
            _ => None,
        }
    }))
}

/// One chunk, then nothing ever again.
fn stalled_body() -> Body {
    Body::from_stream(stream::unfold(0, |n| async move {
        if n == 0 {
            Some((Ok::<_, io::Error>(Bytes::from_static(b"first")), 1))
        } else {
            std::future::pending().await
        }
    }))
}

/// One chunk, then a read error.
fn failing_body() -> Body {
    Body::from_stream(stream::unfold(0, |n| async move {
        match n {
            0 => Some((Ok(Bytes::from_static(b"first")), 1)),
            1 => Some((Err(io::Error::other("the file went away")), 2)),
            _ => None,
        }
    }))
}

/// Chunks for as long as somebody reads them.
fn endless_body() -> Body {
    let chunk = Bytes::from(vec![b'x'; 64 * 1024]);
    Body::from_stream(stream::unfold(chunk, |chunk| async move {
        tokio::task::yield_now().await;
        Some((Ok::<_, io::Error>(chunk.clone()), chunk))
    }))
}

fn app(h: &Harness) -> Router {
    let routes = Router::new()
        .route("/slow/{id}", get(|| async { slow_body() }))
        .route("/stalled/{id}", get(|| async { stalled_body() }))
        .route("/failing/{id}", get(|| async { failing_body() }))
        .route("/endless/{id}", get(|| async { endless_body() }))
        .route("/fixed/{id}", get(|| async { "hello" }))
        .route("/empty/{id}", get(|| async { StatusCode::NO_CONTENT }));
    instrument(routes, HttpTelemetry::new(&h.telemetry))
}

fn get_req(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

/// The samples of `name` (a Prometheus series) whose labels contain `route`.
fn samples(h: &Harness, name: &str, route: &str) -> Vec<String> {
    let label = format!(r#"http_route="{route}""#);
    h.telemetry
        .prometheus_text()
        .lines()
        .filter(|l| {
            l.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
        })
        .filter(|l| l.contains(&label))
        .map(str::to_owned)
        .collect()
}

fn value(line: &str) -> f64 {
    line.rsplit(' ').next().unwrap().parse().unwrap()
}

fn counts(h: &Harness, route: &str) -> Vec<String> {
    samples(h, "http_server_request_duration_seconds_count", route)
}

fn active(h: &Harness, route: &str) -> f64 {
    let lines = samples(h, "http_server_active_requests", route);
    assert_eq!(lines.len(), 1, "{lines:?}");
    value(&lines[0])
}

fn server_spans(h: &Harness) -> Vec<SpanData> {
    h.telemetry.force_flush();
    h.spans
        .get_finished_spans()
        .unwrap()
        .into_iter()
        .filter(|s| s.span_kind == SpanKind::Server)
        .collect()
}

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

#[tokio::test(flavor = "current_thread")]
async fn the_duration_is_recorded_once_the_body_has_been_read_and_covers_its_transfer() {
    let h = harness();
    let app = app(&h);
    let response = app.oneshot(get_req("/slow/1")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    assert!(
        counts(&h, "/slow/{id}").is_empty(),
        "recorded before the body was sent: {:?}",
        counts(&h, "/slow/{id}")
    );
    assert_eq!(active(&h, "/slow/{id}"), 1.0, "the request is still active");
    assert!(
        server_spans(&h).is_empty(),
        "the span ended before the body"
    );

    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"firstsecond");

    let counts = counts(&h, "/slow/{id}");
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert_eq!(value(&counts[0]), 1.0, "{counts:?}");
    assert!(counts[0].contains(r#"http_response_status_code="200""#));
    assert!(!counts[0].contains("error_type"), "{counts:?}");
    let sum = samples(&h, "http_server_request_duration_seconds_sum", "/slow/{id}");
    assert!(
        value(&sum[0]) >= PAUSE.as_secs_f64(),
        "the duration covers the body's transfer: {sum:?}"
    );
    assert_eq!(active(&h, "/slow/{id}"), 0.0);

    let spans = server_spans(&h);
    assert_eq!(spans.len(), 1);
    let span = &spans[0];
    assert_eq!(span.status, Status::Unset);
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(200_i64.into())
    );
    let logs = h.logs.get_emitted_logs().unwrap();
    let log = logs
        .iter()
        .find(|l| {
            l.record
                .body()
                .is_some_and(|b| format!("{b:?}").contains("streaming the second chunk"))
        })
        .expect("the body's event as a log record");
    let context = log.record.trace_context().expect("a trace context");
    assert_eq!(
        context.span_id,
        span.span_context.span_id(),
        "an event emitted while the body is polled belongs to the request's span"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_body_dropped_mid_stream_keeps_its_status_and_is_client_disconnected() {
    let h = harness();
    let app = app(&h);
    let response = app.oneshot(get_req("/stalled/1")).await.unwrap();
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap();
    assert_eq!(first.into_data().unwrap(), "first");
    // hyper drops the body when the client goes away mid-transfer; dropping it here is the same.
    drop(body);

    let counts = counts(&h, "/stalled/{id}");
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert_eq!(value(&counts[0]), 1.0, "{counts:?}");
    assert!(
        counts[0].contains(r#"error_type="client_disconnected""#),
        "{counts:?}"
    );
    assert!(
        counts[0].contains(r#"http_response_status_code="200""#),
        "the status was sent: {counts:?}"
    );
    assert_eq!(active(&h, "/stalled/{id}"), 0.0);

    let spans = server_spans(&h);
    assert_eq!(spans.len(), 1);
    let span = &spans[0];
    assert!(
        matches!(span.status, Status::Error { .. }),
        "{:?}",
        span.status
    );
    assert_eq!(attr(span, "error.type"), Some("client_disconnected".into()));
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(200_i64.into())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_body_that_fails_mid_stream_keeps_its_status_and_records_an_error() {
    let h = harness();
    let app = app(&h);
    let response = app.oneshot(get_req("/failing/1")).await.unwrap();
    assert!(response.into_body().collect().await.is_err());

    let counts = counts(&h, "/failing/{id}");
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert!(counts[0].contains(r#"error_type="_OTHER""#), "{counts:?}");
    assert!(
        counts[0].contains(r#"http_response_status_code="200""#),
        "{counts:?}"
    );
    assert_eq!(active(&h, "/failing/{id}"), 0.0);
    let spans = server_spans(&h);
    assert_eq!(spans.len(), 1);
    assert!(matches!(spans[0].status, Status::Error { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn an_empty_body_dropped_without_being_polled_is_a_plain_success() {
    let h = harness();
    let app = app(&h);
    // hyper never polls a body that says it is at its end.
    let response = app.oneshot(get_req("/empty/1")).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    drop(response);

    let counts = counts(&h, "/empty/{id}");
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert!(
        counts[0].contains(r#"http_response_status_code="204""#),
        "{counts:?}"
    );
    assert!(!counts[0].contains("error_type"), "{counts:?}");
    assert_eq!(active(&h, "/empty/{id}"), 0.0);
}

async fn serve(app: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// Polls `check` until it holds or five seconds pass.
async fn eventually(mut check: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "current_thread")]
async fn over_tcp_a_client_that_hangs_up_mid_body_is_client_disconnected() {
    let h = harness();
    let addr = serve(app(&h)).await;

    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /endless/1 HTTP/1.1\r\nhost: test\r\n\r\n")
        .await
        .unwrap();
    let mut head = Vec::new();
    let mut buf = [0_u8; 4096];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = client.read(&mut buf).await.unwrap();
        assert!(n > 0, "the connection closed before the head");
        head.extend_from_slice(&buf[..n]);
    }
    assert!(head.starts_with(b"HTTP/1.1 200"));
    assert!(
        counts(&h, "/endless/{id}").is_empty(),
        "recorded while the body is still being sent"
    );
    drop(client);

    assert!(
        eventually(|| !counts(&h, "/endless/{id}").is_empty()).await,
        "the request was never recorded"
    );
    let counts = counts(&h, "/endless/{id}");
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert!(
        counts[0].contains(r#"error_type="client_disconnected""#),
        "{counts:?}"
    );
    assert!(
        counts[0].contains(r#"http_response_status_code="200""#),
        "{counts:?}"
    );
    assert_eq!(active(&h, "/endless/{id}"), 0.0);
}

#[tokio::test(flavor = "current_thread")]
async fn over_tcp_a_fixed_body_keeps_its_content_length_and_is_a_success() {
    let h = harness();
    let addr = serve(app(&h)).await;

    let mut client = TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET /fixed/1 HTTP/1.1\r\nhost: test\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut answer = String::new();
    client.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(
        answer.to_ascii_lowercase().contains("content-length: 5"),
        "{answer}"
    );
    assert!(answer.ends_with("hello"), "{answer}");

    assert!(eventually(|| !counts(&h, "/fixed/{id}").is_empty()).await);
    let counts = counts(&h, "/fixed/{id}");
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert!(!counts[0].contains("error_type"), "{counts:?}");
    assert_eq!(active(&h, "/fixed/{id}"), 0.0);
}
