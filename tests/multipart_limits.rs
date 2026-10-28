//! The size limit on the wire, enforced while the body streams in: over a real socket, so that hyper's handling of
//! `Expect: 100-continue` and of chunked bodies is what is tested. A declared length over the limit gets
//! 413 with no `100 Continue`; a chunked body over it gets 413 while the client is still sending.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::num::usize_from_u64;
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use docx_to_pdf::upload::Upload;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const BOUNDARY: &str = "B0011";
const PART_HEAD: &str =
    "--B0011\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.docx\"\r\n\r\n";
const CLOSE: &str = "\r\n--B0011--\r\n";

struct Server {
    address: String,
    _telemetry: Arc<Telemetry>,
}

async fn serve(max_document_size: u64) -> Server {
    let telemetry = Arc::new(
        TelemetryBuilder::new()
            .with_otlp_from(|_| None)
            .build()
            .unwrap(),
    );
    let readiness = Readiness::new();
    readiness.mark_ready();
    let app =
        router(AppState::new(readiness, &telemetry).with_upload(Upload::new(max_document_size)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server {
        address,
        _telemetry: telemetry,
    }
}

fn head(framing: &str, extra: &str) -> String {
    format!(
        "POST /pdf HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/form-data; boundary={BOUNDARY}\r\n\
         {framing}{extra}Connection: close\r\n\r\n"
    )
}

/// The status line of each answer read so far (interim ones included), from the raw bytes.
fn status_lines(raw: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(raw)
        .split("\r\n")
        .filter(|l| l.starts_with("HTTP/1.1 "))
        .map(str::to_owned)
        .collect()
}

/// Reads until the first final (non-1xx) status line has arrived with its headers, or the peer closes.
async fn read_until_final(stream: &mut tokio::net::tcp::OwnedReadHalf) -> Vec<u8> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let has_final = {
            let text = String::from_utf8_lossy(&raw);
            text.split("\r\n\r\n")
                .any(|block| block.starts_with("HTTP/1.1 ") && !block.starts_with("HTTP/1.1 1"))
        };
        if has_final {
            return raw;
        }
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return raw,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
    }
}

#[tokio::test]
async fn expect_continue_with_a_declared_length_over_the_limit_gets_413_and_no_100() {
    let server = serve(10 * 1024 * 1024).await;
    let mut stream = TcpStream::connect(&server.address).await.unwrap();
    let length = 20 * 1024 * 1024;
    stream
        .write_all(
            head(
                &format!("Content-Length: {length}\r\n"),
                "Expect: 100-continue\r\n",
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut raw = Vec::new();
    timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("an answer without the body being sent")
        .unwrap();
    let lines = status_lines(&raw);
    assert_eq!(
        lines,
        ["HTTP/1.1 413 Payload Too Large"],
        "{}",
        String::from_utf8_lossy(&raw)
    );
    assert!(
        String::from_utf8_lossy(&raw).contains("application/problem+json"),
        "{}",
        String::from_utf8_lossy(&raw)
    );
}

#[tokio::test]
async fn expect_continue_under_the_limit_gets_100_then_the_answer() {
    let server = serve(1024 * 1024).await;
    let mut stream = TcpStream::connect(&server.address).await.unwrap();
    // An RTF document: on the allowlist, so it gets as far as the pool.
    let body = format!("{PART_HEAD}{}{CLOSE}", r"{\rtf1}");
    stream
        .write_all(
            head(
                &format!("Content-Length: {}\r\n", body.len()),
                "Expect: 100-continue\r\n",
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut interim = [0u8; 25];
    timeout(Duration::from_secs(10), stream.read_exact(&mut interim))
        .await
        .expect("100 Continue before the body")
        .unwrap();
    assert_eq!(&interim, b"HTTP/1.1 100 Continue\r\n\r\n");
    stream.write_all(body.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .unwrap()
        .unwrap();
    // This router has no pool, so a received document is answered 503.
    assert_eq!(
        status_lines(&raw),
        ["HTTP/1.1 503 Service Unavailable"],
        "{}",
        String::from_utf8_lossy(&raw)
    );
}

/// Sends a chunked document of up to `total` bytes in 64 KiB chunks, counting what was written, until the
/// server stops reading or closes.
fn send_chunked(
    mut writer: tokio::net::tcp::OwnedWriteHalf,
    total: u64,
    sent: Arc<AtomicU64>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let chunk = vec![0u8; 64 * 1024];
        let mut frame = format!("{:x}\r\n", PART_HEAD.len()).into_bytes();
        frame.extend_from_slice(PART_HEAD.as_bytes());
        frame.extend_from_slice(b"\r\n");
        if writer.write_all(&frame).await.is_err() {
            return;
        }
        let mut frame = format!("{:x}\r\n", chunk.len()).into_bytes();
        frame.extend_from_slice(&chunk);
        frame.extend_from_slice(b"\r\n");
        while sent.load(Ordering::SeqCst) < total {
            if writer.write_all(&frame).await.is_err() {
                return;
            }
            sent.fetch_add(chunk.len() as u64, Ordering::SeqCst);
        }
        let _ = writer.write_all(b"0\r\n\r\n").await;
    })
}

#[tokio::test]
async fn a_chunked_upload_over_the_limit_gets_413_while_the_client_is_still_sending() {
    let limit = 1024 * 1024;
    let server = serve(limit).await;
    let mut stream = TcpStream::connect(&server.address).await.unwrap();
    stream
        .write_all(head("Transfer-Encoding: chunked\r\n", "").as_bytes())
        .await
        .unwrap();
    let (mut reader, writer) = stream.into_split();
    let total = 1 << 30;
    let sent = Arc::new(AtomicU64::new(0));
    let sender = send_chunked(writer, total, sent.clone());

    let raw = timeout(Duration::from_secs(30), read_until_final(&mut reader))
        .await
        .expect("an answer before the body ends");
    let sent_when_answered = sent.load(Ordering::SeqCst);
    sender.abort();
    assert_eq!(
        status_lines(&raw),
        ["HTTP/1.1 413 Payload Too Large"],
        "{}",
        String::from_utf8_lossy(&raw)
    );
    assert!(
        sent_when_answered < 64 * limit,
        "the answer came after {sent_when_answered} of {total} bytes"
    );
}

#[tokio::test]
async fn a_content_length_upload_over_the_limit_sent_without_expect_gets_413() {
    let limit = 64 * 1024;
    let server = serve(limit).await;
    let content = vec![b'a'; usize_from_u64(limit) + 1];
    let mut body = PART_HEAD.as_bytes().to_vec();
    body.extend_from_slice(&content);
    body.extend_from_slice(CLOSE.as_bytes());
    // Under the request limit, over the document limit: the body is read and the part is refused.
    assert!((body.len() as u64) < limit + docx_to_pdf::upload::MULTIPART_ALLOWANCE);
    let stream = TcpStream::connect(&server.address).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();
    writer
        .write_all(head(&format!("Content-Length: {}\r\n", body.len()), "").as_bytes())
        .await
        .unwrap();
    let sending = tokio::spawn(async move {
        let _ = writer.write_all(&body).await;
        writer
    });
    let raw = timeout(Duration::from_secs(10), read_until_final(&mut reader))
        .await
        .unwrap();
    let _ = sending.await;
    assert_eq!(
        status_lines(&raw),
        ["HTTP/1.1 413 Payload Too Large"],
        "{}",
        String::from_utf8_lossy(&raw)
    );
}
