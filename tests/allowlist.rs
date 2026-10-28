//! The allowlist through the HTTP interface: the format is decided by the bytes
//! alone, a refused document is answered 415 before any pool instance is borrowed, and the decision shows in
//! the `document.sniff` span and in `docx_to_pdf.document.size`'s `format`.

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use http_body_util::BodyExt;
use opentelemetry::Value;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

struct Harness {
    app: Router,
    readiness: Readiness,
    telemetry: Telemetry,
    spans: InMemorySpanExporter,
    _guard: tracing::subscriber::DefaultGuard,
}

/// The service's router with its spans captured in memory, on this thread.
fn harness() -> Harness {
    let spans = InMemorySpanExporter::default();
    let telemetry = TelemetryBuilder::new()
        .with_otlp_from(|_| None)
        .with_simple_span_exporter(spans.clone())
        .build()
        .unwrap();
    let guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(telemetry.layers()));
    let readiness = Readiness::new();
    let app = router(AppState::new(readiness.clone(), &telemetry));
    Harness {
        app,
        readiness,
        telemetry,
        spans,
        _guard: guard,
    }
}

/// Posts `content` as the `document` file part, under the file name and part `Content-Type` given.
async fn post(
    app: &Router,
    filename: &str,
    part_type: &str,
    content: &[u8],
) -> (StatusCode, String) {
    let mut body = format!(
        "--B0024\r\nContent-Disposition: form-data; name=\"document\"; filename=\"{filename}\"\r\n\
         Content-Type: {part_type}\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(content);
    body.extend_from_slice(b"\r\n--B0024--\r\n");
    let request = Request::post("/pdf")
        .header("content-type", "multipart/form-data; boundary=B0024")
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

fn attribute(span: &SpanData, key: &str) -> Option<Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

impl Harness {
    fn finished(&self, name: &str) -> Vec<SpanData> {
        self.telemetry.force_flush();
        self.spans
            .get_finished_spans()
            .unwrap()
            .into_iter()
            .filter(|s| s.name == name)
            .collect()
    }

    /// The `format` of each `document.sniff` span so far, in order.
    fn sniffed(&self) -> Vec<String> {
        self.finished("document.sniff")
            .iter()
            .map(|s| {
                attribute(s, "format")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            })
            .collect()
    }
}

fn upstream_example() -> Vec<u8> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("upstream/docx-to-pdf/examples/ex1/input.docx");
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("{}: {e} (is the submodule checked out?)", path.display()))
}

/// A one-entry ZIP, stored: enough of an XLSX for the sniffer.
fn xlsx() -> Vec<u8> {
    ooxml("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml")
}

/// A one-entry ZIP whose `[Content_Types].xml` declares `main`.
fn ooxml(main: &str) -> Vec<u8> {
    let data = format!(r#"<Types><Override PartName="/main.xml" ContentType="{main}"/></Types>"#);
    one_entry_zip(b"[Content_Types].xml", data.as_bytes())
}

/// A one-entry ZIP, stored.
fn one_entry_zip(name: &[u8], data: &[u8]) -> Vec<u8> {
    let mut zip = Vec::new();
    zip.extend_from_slice(b"PK\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00");
    zip.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    zip.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    zip.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(name);
    zip.extend_from_slice(data);
    zip
}

#[tokio::test]
async fn a_refused_document_is_415_and_never_borrows_an_instance() {
    let h = harness();
    h.readiness.mark_ready();
    let refused: [(&str, &str, Vec<u8>); 4] = [
        (
            "notes.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            b"plain text".to_vec(),
        ),
        ("empty.docx", "application/octet-stream", Vec::new()),
        ("report.docx", "application/msword", xlsx()),
        ("x.rtf", "application/rtf", vec![0xA5; 4096]),
    ];
    for (filename, part_type, content) in &refused {
        let (status, body) = post(&h.app, filename, part_type, content).await;
        assert_eq!(
            status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{filename}: {body}"
        );
        assert!(
            body.contains("the document is not DOCX, DOC, ODT or RTF"),
            "{body}"
        );
        assert!(!body.contains(filename), "the answer names no file: {body}");
    }
    assert_eq!(h.sniffed(), ["unsupported"; 4]);
    assert!(
        h.finished("pool.acquire").is_empty(),
        "a refused document never reaches the pool"
    );
}

#[tokio::test]
async fn the_refusal_does_not_wait_for_the_pool() {
    // Before the pool is ready an allowed document is turned away for want of an instance (503); a refused
    // one is still 415, because the allowlist is decided before the pool is consulted.
    let h = harness();
    let (status, _) = post(&h.app, "a.docx", "application/octet-stream", b"text").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (status, _) = post(
        &h.app,
        "a.docx",
        "application/octet-stream",
        &upstream_example(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn the_file_name_and_part_content_type_play_no_part() {
    let h = harness();
    h.readiness.mark_ready();
    // A DOCX named .txt and typed text/plain passes the allowlist (and, with no pool here, is answered 503).
    let (status, body) = post(&h.app, "notes.txt", "text/plain", &upstream_example()).await;
    assert_ne!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    // An RTF named .pdf and typed application/pdf too.
    let (status, body) = post(&h.app, "x.pdf", "application/pdf", b"{\\rtf1 hi}").await;
    assert_ne!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    assert_eq!(h.sniffed(), ["docx", "rtf"]);
}

#[tokio::test]
async fn the_document_size_carries_the_format() {
    let h = harness();
    let docx = upstream_example();
    post(&h.app, "a", "application/octet-stream", &docx).await;
    post(&h.app, "a", "application/octet-stream", b"text").await;
    let request = Request::get("/metrics").body(Body::empty()).unwrap();
    let response = h.app.clone().oneshot(request).await.unwrap();
    let text = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    let sum = |format: &str| {
        text.lines()
            .find(|l| {
                l.starts_with("docx_to_pdf_document_size_bytes_sum{")
                    && l.contains(&format!(r#"format="{format}""#))
            })
            .and_then(|l| l.rsplit(' ').next())
            .map(str::to_owned)
    };
    assert_eq!(sum("docx"), Some(docx.len().to_string()), "{text}");
    assert_eq!(sum("unsupported"), Some("4".to_owned()), "{text}");
}

/// The Word-family variants moalhaddar/docx-to-pdf really converts pass the allowlist under their own names; the RTF
/// behind a byte-order mark, which moalhaddar/docx-to-pdf typesets as plain text, and a flat ODS stay 415:
/// a variant joins the allowlist only where moalhaddar/docx-to-pdf really renders it.
#[tokio::test]
async fn the_word_variants_the_oracle_converts_pass_and_their_near_misses_do_not() {
    let h = harness();
    h.readiness.mark_ready();
    let flat = |mimetype: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" office:mimetype="{mimetype}"><office:body/></office:document>"#
        )
        .into_bytes()
    };
    let admitted = [
        ooxml("application/vnd.ms-word.document.macroEnabled.main+xml"),
        ooxml("application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml"),
        one_entry_zip(
            b"mimetype",
            b"application/vnd.oasis.opendocument.text-template",
        ),
        flat("application/vnd.oasis.opendocument.text"),
    ];
    for content in &admitted {
        let (status, body) = post(&h.app, "a.docx", "application/octet-stream", content).await;
        assert_ne!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    }
    let refused = [
        b"\xEF\xBB\xBF{\\rtf1\\ansi hello}".to_vec(),
        flat("application/vnd.oasis.opendocument.spreadsheet"),
        ooxml("application/vnd.ms-excel.sheet.macroEnabled.main+xml"),
        one_entry_zip(
            b"mimetype",
            b"application/vnd.oasis.opendocument.spreadsheet-template",
        ),
    ];
    for content in &refused {
        let (status, body) = post(&h.app, "a.docx", "application/octet-stream", content).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    }
    assert_eq!(
        h.sniffed(),
        [
            "docm",
            "dotx",
            "ott",
            "fodt",
            "unsupported",
            "unsupported",
            "unsupported",
            "unsupported"
        ]
    );
}
