//! A response body still streaming when the shutdown grace period ends is cut, and the request is recorded
//! as `shutdown_grace_exceeded` with the status that was sent before `serve` returns, so before `main`
//! flushes the telemetry, so the cut request is still counted. Two clients: one still reading, whose body
//! hyper keeps polling, and one that stopped reading, whose body hyper no longer polls.

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::routing::get;
use docx_to_pdf::http_telemetry::{HttpTelemetry, SHUTDOWN_GRACE_EXCEEDED, instrument};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::server::{Shutdown, serve};
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use futures_util::stream;
use opentelemetry::Value;
use opentelemetry::trace::{SpanKind, Status};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep, timeout};
use tracing_subscriber::layer::SubscriberExt as _;

struct Harness {
    telemetry: Telemetry,
    spans: InMemorySpanExporter,
    _guard: tracing::subscriber::DefaultGuard,
}

fn harness() -> Harness {
    let spans = InMemorySpanExporter::default();
    let telemetry = TelemetryBuilder::new()
        .with_simple_span_exporter(spans.clone())
        .build()
        .unwrap();
    let subscriber = tracing_subscriber::registry().with(telemetry.layers());
    let guard = tracing::subscriber::set_default(subscriber);
    Harness {
        telemetry,
        spans,
        _guard: guard,
    }
}

const GRACE: Duration = Duration::from_millis(200);

/// A small chunk every 50 ms, forever: a client reading it keeps hyper polling the body.
fn trickle_body() -> Body {
    Body::from_stream(stream::unfold((), |()| async {
        sleep(Duration::from_millis(50)).await;
        Some((Ok::<_, io::Error>(Bytes::from_static(b"chunk")), ()))
    }))
}

/// Large chunks for as long as the socket takes them: a client that stops reading fills the buffers, and
/// hyper waits on the socket without polling the body.
fn flood_body() -> Body {
    let chunk = Bytes::from(vec![b'x'; 64 * 1024]);
    Body::from_stream(stream::unfold(chunk, |chunk| async move {
        tokio::task::yield_now().await;
        Some((Ok::<_, io::Error>(chunk.clone()), chunk))
    }))
}

fn app(h: &Harness) -> Router {
    let routes = Router::new()
        .route("/trickle", get(|| async { trickle_body() }))
        .route("/flood", get(|| async { flood_body() }));
    instrument(routes, HttpTelemetry::new(&h.telemetry))
}

struct Running {
    addr: SocketAddr,
    stop: oneshot::Sender<()>,
    served: tokio::task::JoinHandle<io::Result<Shutdown>>,
}

async fn start(app: Router) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let readiness = Readiness::new();
    readiness.mark_ready();
    let (stop, stopped) = oneshot::channel::<()>();
    let served = tokio::spawn(serve(
        listener,
        app,
        readiness,
        async move {
            let _ = stopped.await;
        },
        GRACE,
    ));
    Running { addr, stop, served }
}

/// Sends a request and reads until the status line has come; the stream stays open.
async fn open(addr: SocketAddr, path: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut head = [0_u8; 12];
    stream.read_exact(&mut head).await.unwrap();
    assert_eq!(&head, b"HTTP/1.1 200");
    stream
}

/// Shuts the server down and waits for `serve` to return, which must happen soon after the grace period.
async fn shut_down(r: Running) {
    let began = Instant::now();
    r.stop.send(()).unwrap();
    let outcome = timeout(Duration::from_secs(5), r.served)
        .await
        .expect("serve returns after the grace period")
        .unwrap()
        .unwrap();
    assert_eq!(outcome, Shutdown::GraceExceeded);
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "{:?}",
        began.elapsed()
    );
}

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

fn attr(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

/// The request on `route` was recorded once, as cut short by the grace period with its 200 sent.
fn assert_recorded_as_grace_exceeded(h: &Harness, route: &str) {
    let counts = samples(h, "http_server_request_duration_seconds_count", route);
    assert_eq!(counts.len(), 1, "{counts:?}");
    assert_eq!(value(&counts[0]), 1.0, "{counts:?}");
    assert!(
        counts[0].contains(r#"http_response_status_code="200""#),
        "{counts:?}"
    );
    assert!(
        counts[0].contains(&format!(r#"error_type="{SHUTDOWN_GRACE_EXCEEDED}""#)),
        "{counts:?}"
    );
    let active = samples(h, "http_server_active_requests", route);
    assert_eq!(active.len(), 1, "{active:?}");
    assert_eq!(value(&active[0]), 0.0, "{active:?}");

    h.telemetry.force_flush();
    let spans: Vec<SpanData> = h
        .spans
        .get_finished_spans()
        .unwrap()
        .into_iter()
        .filter(|s| s.span_kind == SpanKind::Server)
        .collect();
    assert_eq!(spans.len(), 1, "the server span ended");
    let span = &spans[0];
    assert!(
        matches!(span.status, Status::Error { .. }),
        "{:?}",
        span.status
    );
    assert_eq!(
        attr(span, "error.type"),
        Some(SHUTDOWN_GRACE_EXCEEDED.into())
    );
    assert_eq!(
        attr(span, "http.response.status_code"),
        Some(200_i64.into())
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_body_streaming_to_a_reading_client_is_cut_and_recorded_when_the_grace_ends() {
    let h = harness();
    let r = start(app(&h)).await;
    let mut client = open(r.addr, "/trickle").await;
    let reading = tokio::spawn(async move {
        let mut sink = Vec::new();
        // Ends with EOF or an error once the server cuts the body.
        let _ = client.read_to_end(&mut sink).await;
    });
    sleep(Duration::from_millis(100)).await;

    shut_down(r).await;
    assert_recorded_as_grace_exceeded(&h, "/trickle");
    timeout(Duration::from_secs(2), reading)
        .await
        .expect("the client's connection was closed")
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_body_to_a_client_that_stopped_reading_is_cut_and_recorded_when_the_grace_ends() {
    let h = harness();
    let r = start(app(&h)).await;
    // Read the status line, then nothing: the socket's buffers fill and hyper stops polling the body.
    let client = open(r.addr, "/flood").await;
    sleep(Duration::from_millis(100)).await;

    shut_down(r).await;
    assert_recorded_as_grace_exceeded(&h, "/flood");
    drop(client);
}
