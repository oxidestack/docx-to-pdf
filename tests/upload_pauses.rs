//! An upload whose body pauses converts: no read timeout cuts a body between its bytes. Upstream runs Tomcat with
//! `connection-timeout=30`, in milliseconds, and about 5 % of real uploads on the test stand failed with 500
//! because the body paused for longer. Here a multipart body goes over a real socket, through
//! the production `server::serve` and router, in chunks with pauses of 200 ms to 2 s between them, and gets its
//! PDF. The pool is a stand-in instance (`sh` speaking the instance protocol, as in `tests/conversion.rs`).

use std::net::SocketAddr;
use std::time::Duration;

use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::server::serve;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use docx_to_pdf::upload::Upload;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::{Instant, sleep, timeout};

/// Answers every conversion with a small PDF.
const STAND_IN: &str = r#"
    test -d "$1" || exit 9
    printf 'ready\t1\t%s\n' "$$"
    tab="$(printf '\t')"
    while IFS="$tab" read -r cmd input output; do
        printf '%%PDF-1.4\n%% paused upload\n%%%%EOF\n' > "$output"
        printf 'ok\t1\t1\n'
    done
"#;

struct Service {
    addr: SocketAddr,
    _dir: tempfile::TempDir,
    _telemetry: Telemetry,
    _stop: oneshot::Sender<()>,
}

async fn start() -> Service {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["profiles", "spool"] {
        std::fs::create_dir(dir.path().join(sub)).unwrap();
    }
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let launcher = Launcher::with_args(
        "/bin/sh",
        ["-c".to_string(), STAND_IN.to_string(), "sh".to_string()],
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
    let state = AppState::new(readiness.clone(), &telemetry).with_upload(upload);
    state.set_pool(pool);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    tokio::spawn(serve(
        listener,
        router(state),
        readiness,
        async move {
            let _ = stopped.await;
        },
        Duration::from_secs(1),
    ));
    Service {
        addr,
        _dir: dir,
        _telemetry: telemetry,
        _stop: stop,
    }
}

const HEAD: &[u8] =
    b"--B0059\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.rtf\"\r\n\r\n";
const CLOSE: &[u8] = b"\r\n--B0059--\r\n";

/// The multipart body in the pieces it is sent in: the part's head split in two, then the document in
/// three pieces, then the closing boundary.
fn pieces() -> Vec<Vec<u8>> {
    let document = format!("{{\\rtf1 {}}}", "paused upload ".repeat(2000)).into_bytes();
    let third = document.len() / 3;
    vec![
        HEAD[..20].to_vec(),
        HEAD[20..].to_vec(),
        document[..third].to_vec(),
        document[third..2 * third].to_vec(),
        document[2 * third..].to_vec(),
        CLOSE.to_vec(),
    ]
}

/// The pauses between the pieces: one inside the part's head, the rest inside the document, the longest
/// before the closing boundary.
const PAUSES: [Duration; 5] = [
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_millis(300),
    Duration::from_secs(2),
];

/// Sends `pieces` with `PAUSES` between them, chunked or under a `Content-Length`, and returns the
/// whole answer.
async fn post_with_pauses(addr: SocketAddr, chunked: bool) -> Vec<u8> {
    let pieces = pieces();
    let length: usize = pieces.iter().map(Vec::len).sum();
    let framing = if chunked {
        "Transfer-Encoding: chunked\r\n".to_string()
    } else {
        format!("Content-Length: {length}\r\n")
    };
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.set_nodelay(true).unwrap();
    stream
        .write_all(
            format!(
                "POST /pdf HTTP/1.1\r\nHost: localhost\r\n\
                 Content-Type: multipart/form-data; boundary=B0059\r\n{framing}Connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    for (i, piece) in pieces.iter().enumerate() {
        if chunked {
            stream
                .write_all(format!("{:x}\r\n", piece.len()).as_bytes())
                .await
                .unwrap();
            stream.write_all(piece).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
        } else {
            stream.write_all(piece).await.unwrap();
        }
        stream.flush().await.unwrap();
        if let Some(pause) = PAUSES.get(i) {
            sleep(*pause).await;
        }
    }
    if chunked {
        stream.write_all(b"0\r\n\r\n").await.unwrap();
    }
    let mut answer = Vec::new();
    timeout(Duration::from_secs(10), stream.read_to_end(&mut answer))
        .await
        .expect("an answer after the last piece")
        .unwrap();
    answer
}

fn assert_converted(answer: &[u8], began: Instant) {
    let text = String::from_utf8_lossy(answer);
    assert!(text.starts_with("HTTP/1.1 200 OK\r\n"), "{text}");
    assert!(
        text.to_ascii_lowercase()
            .contains("content-type: application/pdf"),
        "{text}"
    );
    assert!(text.contains("%PDF-1.4\n% paused upload"), "{text}");
    let paused: Duration = PAUSES.iter().sum();
    assert!(
        began.elapsed() >= paused,
        "the answer came before the body was sent"
    );
}

#[tokio::test]
async fn a_body_with_a_content_length_that_pauses_up_to_2_s_converts() {
    let service = start().await;
    let began = Instant::now();
    let answer = post_with_pauses(service.addr, false).await;
    assert_converted(&answer, began);
}

#[tokio::test]
async fn a_chunked_body_that_pauses_up_to_2_s_converts() {
    let service = start().await;
    let began = Instant::now();
    let answer = post_with_pauses(service.addr, true).await;
    assert_converted(&answer, began);
}
