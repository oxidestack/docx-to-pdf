//! A deflate bomb through the HTTP interface: a DOCX whose parts inflate past the inflated-size limit
//! is answered 413 before it is sniffed or meets the pool, whether its directory declares the sizes or understates
//! them; a document within the limit, and a ZIP outside the allowlist, go on as before.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use docx_to_pdf::upload::{INFLATED, Upload};
use http_body_util::BodyExt;
use opentelemetry_sdk::trace::InMemorySpanExporter;
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;

const MIB: usize = 1024 * 1024;

const DOCX_CT: &str = r#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
const XLSX_CT: &str = r#"<Types><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;

struct Harness {
    app: Router,
    telemetry: Telemetry,
    spans: InMemorySpanExporter,
    _guard: tracing::subscriber::DefaultGuard,
}

/// The service's router, ready but with no pool (an allowed document within the limit gets 503), receiving as
/// `upload` says.
fn harness(upload: Upload) -> Harness {
    let spans = InMemorySpanExporter::default();
    let telemetry = TelemetryBuilder::new()
        .with_otlp_from(|_| None)
        .with_simple_span_exporter(spans.clone())
        .build()
        .unwrap();
    let guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(telemetry.layers()));
    let readiness = Readiness::new();
    readiness.mark_ready();
    let app = router(AppState::new(readiness, &telemetry).with_upload(upload));
    Harness {
        app,
        telemetry,
        spans,
        _guard: guard,
    }
}

impl Harness {
    fn finished(&self, name: &str) -> usize {
        self.telemetry.force_flush();
        self.spans
            .get_finished_spans()
            .unwrap()
            .iter()
            .filter(|s| s.name == name)
            .count()
    }
}

async fn post(app: &Router, content: &[u8]) -> (StatusCode, String, String) {
    let mut body =
        b"--B0084\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.docx\"\r\n\
                     Content-Type: application/octet-stream\r\n\r\n"
            .to_vec();
    body.extend_from_slice(content);
    body.extend_from_slice(b"\r\n--B0084--\r\n");
    let request = Request::post("/pdf")
        .header("content-type", "multipart/form-data; boundary=B0084")
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        content_type,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

/// A ZIP of deflated `entries`; each declares the uncompressed size `declare` gives it from its real one.
fn zip(entries: &[(&str, &[u8])], declare: impl Fn(usize) -> u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let packed = miniz_oxide::deflate::compress_to_vec(data, 6);
        let offset = u32::try_from(out.len()).unwrap();
        let mut sizes = u32::try_from(packed.len()).unwrap().to_le_bytes().to_vec();
        sizes.extend_from_slice(&declare(data.len()).to_le_bytes());
        out.extend_from_slice(b"PK\x03\x04\x14\0\0\0\x08\0\0\0\0\0\0\0\0\0");
        out.extend_from_slice(&sizes);
        out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&packed);
        central.extend_from_slice(b"PK\x01\x02\x14\0\x14\0\0\0\x08\0\0\0\0\0\0\0\0\0");
        central.extend_from_slice(&sizes);
        central.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let directory = u32::try_from(out.len()).unwrap();
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&u16::try_from(entries.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&u16::try_from(entries.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&u32::try_from(central.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

/// The pages bomb in small (at full size, a 7.4 MiB DOCX that wrote 1.5 GiB to `/tmp` while LibreOffice
/// loaded it): one paragraph repeated to `size` bytes of `word/document.xml`.
fn paragraphs(size: usize) -> Vec<u8> {
    let p = b"<w:p><w:r><w:t>The same paragraph again.</w:t></w:r></w:p>";
    p.repeat(size / p.len() + 1)[..size].to_vec()
}

fn assert_refused(h: &Harness, (status, content_type, body): (StatusCode, String, String)) {
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(content_type, "application/problem+json");
    let problem: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(problem["status"], 413);
    assert_eq!(problem["type"], "about:blank");
    assert_eq!(problem["detail"], INFLATED);
    assert_eq!(
        h.finished("document.sniff"),
        0,
        "refused before it is sniffed"
    );
    assert_eq!(h.finished("pool.acquire"), 0, "refused before the pool");
}

#[tokio::test]
async fn a_docx_whose_directory_declares_past_the_default_limit_is_413() {
    // 300 MiB declared, past the default's 256 MiB; the file is a few hundred bytes, so the size limit never sees it.
    let h = harness(Upload::new(10 * MIB as u64));
    let bomb = zip(
        &[
            ("[Content_Types].xml", DOCX_CT.as_bytes()),
            ("word/document.xml", &paragraphs(4096)),
        ],
        |real| {
            if real == 4096 {
                300 * u32::try_from(MIB).unwrap()
            } else {
                u32::try_from(real).unwrap()
            }
        },
    );
    assert_refused(&h, post(&h.app, &bomb).await);
}

#[tokio::test]
async fn a_docx_that_inflates_past_the_limit_is_413_whatever_its_directory_declares() {
    let mut upload = Upload::new(10 * MIB as u64);
    upload.max_inflated_size = MIB as u64;
    let parts: [(&str, &[u8]); 2] = [
        ("[Content_Types].xml", DOCX_CT.as_bytes()),
        ("word/document.xml", &paragraphs(3 * MIB)),
    ];
    let honest = zip(&parts, |real| u32::try_from(real).unwrap());
    let understated = zip(&parts, |_| 1);
    assert!(honest.len() < 64 * 1024, "{} bytes", honest.len());
    let h = harness(upload);
    assert_refused(&h, post(&h.app, &honest).await);
    assert_refused(&h, post(&h.app, &understated).await);
}

#[tokio::test]
async fn a_docx_within_the_limit_goes_on_to_the_allowlist_and_the_pool() {
    // No pool: an allowed document meets 503 there, after its `document.sniff`.
    let h = harness(Upload::new(10 * MIB as u64));
    let document = zip(
        &[
            ("[Content_Types].xml", DOCX_CT.as_bytes()),
            ("word/document.xml", &paragraphs(3 * MIB)),
        ],
        |real| u32::try_from(real).unwrap(),
    );
    let (status, _, body) = post(&h.app, &document).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(h.finished("document.sniff"), 1);
}

#[tokio::test]
async fn a_zip_outside_the_allowlist_is_415_however_far_it_inflates() {
    let mut upload = Upload::new(10 * MIB as u64);
    upload.max_inflated_size = MIB as u64;
    let h = harness(upload);
    let xlsx = zip(
        &[
            ("[Content_Types].xml", XLSX_CT.as_bytes()),
            ("xl/worksheets/sheet1.xml", &paragraphs(3 * MIB)),
        ],
        |real| u32::try_from(real).unwrap(),
    );
    let (status, _, body) = post(&h.app, &xlsx).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
}
