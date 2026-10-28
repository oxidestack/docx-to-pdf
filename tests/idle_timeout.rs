//! A client that stops sending (a slow loris) is cut, so it cannot hold a connection open: no byte of the request's head or body for
//! the idle limit (`DOCX_TO_PDF_IDLE_TIMEOUT`) answers 408, a problem document, and closes the connection.
//! A pause shorter than the limit is waited for, however long the whole upload takes, and a conversion that
//! runs longer than the limit is not cut, although its client sends nothing meanwhile. Everything goes over a
//! real socket through the production `server::serve_with_idle_timeout` and router; the limit is 300 ms here,
//! and the pool is a stand-in instance (`sh` speaking the instance protocol, as in `tests/upload_pauses.rs`).

use std::net::SocketAddr;
use std::time::Duration;

use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::config::Config;
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::server::{serve_configured, serve_with_idle_timeout};
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use docx_to_pdf::upload::Upload;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep, timeout};

const IDLE: Duration = Duration::from_millis(300);

/// Answers every conversion with a small PDF, after `delay` seconds.
fn stand_in(delay: &str) -> String {
    format!(
        r#"
    test -d "$1" || exit 9
    printf 'ready\t1\t%s\n' "$$"
    tab="$(printf '\t')"
    while IFS="$tab" read -r cmd input output; do
        sleep {delay}
        printf '%%PDF-1.4\n%% idle limit\n%%%%EOF\n' > "$output"
        printf 'ok\t1\t1\n'
    done
"#
    )
}

struct Service {
    addr: SocketAddr,
    telemetry: Telemetry,
    _dir: tempfile::TempDir,
    _stop: oneshot::Sender<()>,
}

/// The service with the idle limit [`IDLE`], its conversions taking `delay` seconds.
async fn start(delay: &str) -> Service {
    start_with_idle(delay, IDLE).await
}

/// The service with the idle limit `idle`, its conversions taking `delay` seconds.
async fn start_with_idle(delay: &str, idle: Duration) -> Service {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["profiles", "spool"] {
        std::fs::create_dir(dir.path().join(sub)).unwrap();
    }
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let launcher = Launcher::with_args(
        "/bin/sh",
        ["-c".to_string(), stand_in(delay), "sh".to_string()],
        dir.path().join("profiles"),
    );
    let settings = PoolSettings {
        size: 1,
        wait_timeout: Duration::from_secs(5),
        max_waiting: 4,
        start_timeout: Duration::from_secs(10),
    };
    let pool = Pool::start_with_limits(
        launcher,
        settings,
        InstanceLimits::default(),
        readiness.clone(),
        &telemetry.meter(),
    )
    .await
    .unwrap();
    let mut upload = Upload::new(1024 * 1024);
    upload.spool_root = dir.path().join("spool");
    upload.idle_timeout = idle;
    let state = AppState::new(readiness.clone(), &telemetry).with_upload(upload);
    state.set_pool(pool);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(serve_with_idle_timeout(
        listener,
        router(state),
        readiness,
        async move {
            let _ = stopped.await;
        },
        Duration::from_secs(1),
        idle,
    ));
    Service {
        addr,
        telemetry,
        _dir: dir,
        _stop: stop,
    }
}

const HEAD: &[u8] =
    b"--B0060\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.rtf\"\r\n\r\n";
const CLOSE: &[u8] = b"\r\n--B0060--\r\n";

fn document() -> Vec<u8> {
    format!("{{\\rtf1 {}}}", "idle limit ".repeat(500)).into_bytes()
}

fn whole_body() -> Vec<u8> {
    [HEAD, &document(), CLOSE].concat()
}

fn request_head(length: usize, connection: &str) -> String {
    format!(
        "POST /pdf HTTP/1.1\r\nHost: localhost\r\n\
         Content-Type: multipart/form-data; boundary=B0060\r\n\
         Content-Length: {length}\r\nConnection: {connection}\r\n\r\n"
    )
}

async fn connect(addr: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect(addr).await.unwrap();
    stream.set_nodelay(true).unwrap();
    stream
}

/// Everything the service sends until it closes the connection; fails if it does not close within 5 s.
async fn read_until_closed(stream: &mut TcpStream) -> String {
    let mut answer = Vec::new();
    timeout(Duration::from_secs(5), stream.read_to_end(&mut answer))
        .await
        .expect("the service closes the connection")
        .unwrap();
    String::from_utf8_lossy(&answer).into_owned()
}

fn assert_request_timeout(answer: &str) {
    assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
    let lower = answer.to_ascii_lowercase();
    assert!(
        lower.contains("content-type: application/problem+json"),
        "{answer}"
    );
    assert!(lower.contains("connection: close"), "{answer}");
    let json = answer.split("\r\n\r\n").nth(1).unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(json).expect(answer);
    assert_eq!(json["status"], 408, "{answer}");
    assert_eq!(json["title"], "Request Timeout", "{answer}");
    let detail = json["detail"].as_str().unwrap().to_ascii_lowercase();
    assert!(!detail.contains("libreoffice"), "{answer}");
}

#[tokio::test]
async fn a_body_that_stops_is_answered_408_and_closed() {
    let service = start("0").await;
    let body = whole_body();
    let mut stream = connect(service.addr).await;
    stream
        .write_all(request_head(body.len(), "keep-alive").as_bytes())
        .await
        .unwrap();
    stream.write_all(&body[..body.len() / 2]).await.unwrap();
    let began = Instant::now();
    let answer = read_until_closed(&mut stream).await;
    assert_request_timeout(&answer);
    assert!(began.elapsed() >= IDLE, "cut before the limit");
    assert!(began.elapsed() < Duration::from_secs(3), "cut far past it");
}

#[tokio::test]
async fn a_head_that_stops_is_answered_408_and_closed() {
    let service = start("0").await;
    let mut stream = connect(service.addr).await;
    stream
        .write_all(b"POST /pdf HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .unwrap();
    let began = Instant::now();
    let answer = read_until_closed(&mut stream).await;
    assert_request_timeout(&answer);
    assert!(began.elapsed() >= IDLE, "cut before the limit");
}

/// The idle limit of the test of pauses under it: ten times each pause ([`PAUSE`]), so a pause that a CPU-starved
/// runner stretches, on the client's side or in the server's reading, stays under the limit (with
/// pauses of half a 300 ms limit, a loaded CI runner had the connection cut mid-upload).
const PAUSES_IDLE: Duration = Duration::from_millis(1500);
const PAUSE: Duration = Duration::from_millis(150);

#[tokio::test]
async fn pauses_under_the_limit_convert_however_long_the_upload_takes() {
    let service = start_with_idle("0", PAUSES_IDLE).await;
    let body = whole_body();
    let mut stream = connect(service.addr).await;
    let head = request_head(body.len(), "close");
    let (first, rest) = head.as_bytes().split_at(30);
    stream.write_all(first).await.unwrap();
    sleep(PAUSE).await;
    stream.write_all(rest).await.unwrap();
    let began = Instant::now();
    // Twenty-five pieces, each after a pause of a tenth of the limit: the upload takes 2.5 limits.
    for piece in body.chunks(body.len().div_ceil(25)) {
        sleep(PAUSE).await;
        stream.write_all(piece).await.unwrap();
    }
    let answer = read_until_closed(&mut stream).await;
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains("% idle limit"), "{answer}");
    assert!(began.elapsed() > 2 * PAUSES_IDLE);
}

#[tokio::test]
async fn a_conversion_longer_than_the_limit_is_not_cut() {
    let service = start("1").await;
    let body = whole_body();
    let mut stream = connect(service.addr).await;
    stream
        .write_all(request_head(body.len(), "close").as_bytes())
        .await
        .unwrap();
    stream.write_all(&body).await.unwrap();
    let began = Instant::now();
    let answer = read_until_closed(&mut stream).await;
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    assert!(answer.contains("% idle limit"), "{answer}");
    assert!(began.elapsed() >= Duration::from_secs(1));
}

#[tokio::test]
async fn an_idle_kept_alive_connection_is_closed_without_an_answer() {
    let service = start("0").await;
    let mut stream = connect(service.addr).await;
    stream
        .write_all(b"GET /livez HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut first = vec![0u8; 4096];
    let n = timeout(Duration::from_secs(5), stream.read(&mut first))
        .await
        .unwrap()
        .unwrap();
    let first = String::from_utf8_lossy(&first[..n]).into_owned();
    assert!(first.starts_with("HTTP/1.1 200 OK\r\n"), "{first}");
    let began = Instant::now();
    let rest = read_until_closed(&mut stream).await;
    assert_eq!(rest, "", "an idle connection gets no unasked-for answer");
    assert!(began.elapsed() >= IDLE / 2, "closed before the limit");
}

#[tokio::test]
async fn a_body_cut_by_the_limit_is_recorded_as_a_408_without_an_error_type() {
    let service = start("0").await;
    let body = whole_body();
    let mut stream = connect(service.addr).await;
    stream
        .write_all(request_head(body.len(), "keep-alive").as_bytes())
        .await
        .unwrap();
    stream.write_all(&body[..10]).await.unwrap();
    assert_request_timeout(&read_until_closed(&mut stream).await);

    let text = service.telemetry.prometheus_text();
    let counts: Vec<&str> = text
        .lines()
        .filter(|l| {
            l.starts_with("http_server_request_duration_seconds_count")
                && l.contains(r#"http_route="/pdf""#)
        })
        .collect();
    assert_eq!(counts.len(), 1, "{text}");
    assert!(
        counts[0].contains(r#"http_response_status_code="408""#),
        "{text}"
    );
    assert!(!counts[0].contains("error_type"), "{text}");
}

#[tokio::test]
async fn the_binarys_serve_takes_its_idle_limit_from_docx_to_pdf_idle_timeout() {
    // `main` serves through `serve_configured` and `AppState::from_config`; with the variable at 300 ms a
    // head that stops is cut after 300 ms, not after the 120 s default.
    let config = Config::from_lookup(|name| {
        (name == "DOCX_TO_PDF_IDLE_TIMEOUT").then(|| "300ms".to_owned())
    })
    .unwrap();
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let state = AppState::from_config(readiness.clone(), &telemetry, &config);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (_stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(serve_configured(
        listener,
        router(state),
        readiness,
        async move {
            let _ = stopped.await;
        },
        &config,
    ));
    let mut stream = connect(addr).await;
    stream
        .write_all(b"POST /pdf HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .unwrap();
    let began = Instant::now();
    let answer = read_until_closed(&mut stream).await;
    assert_request_timeout(&answer);
    assert!(began.elapsed() >= IDLE, "cut before the limit");
}
