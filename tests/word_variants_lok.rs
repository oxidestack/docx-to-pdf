//! The Word-family variants with real LibreOffice: the variants the allowlist admits — DOCM, DOTX, OTT and flat
//! ODT — convert end to end through `POST /pdf` on a pool of real instances, each to the page count and text the
//! moalhaddar/docx-to-pdf gave for the same sample (one page, the sample's sentence): the allowlist admits a
//! variant only where moalhaddar/docx-to-pdf really renders it.
//!
//! LibreOfficeKit does not run on macOS: this runs in `tools/instance-test/`. Off Linux it is ignored
//! with the reason; on a Linux without LibreOffice it prints a skip and passes, unless `DOCX_TO_PDF_REQUIRE_LOK=1`.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use docx_to_pdf::allowlist::{Format, sniff};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::PROGRAM_DIR;
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;
use docx_to_pdf::upload::Upload;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn skip_reason() -> Option<String> {
    let required = std::env::var_os("DOCX_TO_PDF_REQUIRE_LOK").is_some_and(|v| v == "1");
    let missing = if !cfg!(target_os = "linux") {
        Some("LibreOfficeKit runs on Linux only".to_string())
    } else if !Path::new(PROGRAM_DIR).join("libmergedlo.so").exists() {
        Some(format!("no {PROGRAM_DIR}/libmergedlo.so"))
    } else {
        None
    };
    match missing {
        Some(reason) if required => panic!("DOCX_TO_PDF_REQUIRE_LOK=1 but {reason}"),
        other => other,
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A ZIP of stored entries, CRCs and central directory included: a valid package for LibreOffice.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let offset = u32::try_from(out.len()).unwrap();
        let fields = |out: &mut Vec<u8>| {
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version, flags, stored, time, date
            out.extend_from_slice(&crc32(data).to_le_bytes());
            out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
        };
        out.extend_from_slice(b"PK\x03\x04");
        fields(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        central.extend_from_slice(b"PK\x01\x02\x14\x00");
        fields(&mut central);
        central.extend_from_slice(&[0; 6]); // comment length, disk, internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let (at, count) = (
        u32::try_from(out.len()).unwrap(),
        u16::try_from(entries.len()).unwrap(),
    );
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&u32::try_from(central.len()).unwrap().to_le_bytes());
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// The OOXML sample sent to moalhaddar/docx-to-pdf: one paragraph of `text`, main part typed `main`.
fn ooxml(main: &str, text: &str) -> Vec<u8> {
    let types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="{main}"/></Types>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
    );
    zip(&[
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ])
}

const ODF_NAMESPACES: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" office:version="1.3""#;

fn ott(text: &str) -> Vec<u8> {
    let content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {ODF_NAMESPACES}><office:body><office:text><text:p>{text}</text:p></office:text></office:body></office:document-content>"#
    );
    let manifest = r#"<?xml version="1.0" encoding="UTF-8"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.3"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text-template"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#;
    zip(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text-template",
        ),
        ("content.xml", content.as_bytes()),
        ("META-INF/manifest.xml", manifest.as_bytes()),
    ])
}

fn fodt(text: &str) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document {ODF_NAMESPACES} office:mimetype=\"application/vnd.oasis.opendocument.text\"><office:body><office:text><text:p>{text}</text:p></office:text></office:body></office:document>\n"
    )
    .into_bytes()
}

async fn post(app: &Router, document: &[u8]) -> (StatusCode, Vec<u8>) {
    let mut body = b"--B0038\r\nContent-Disposition: form-data; name=\"document\"; filename=\"input.docx\"\r\n\r\n".to_vec();
    body.extend_from_slice(document);
    body.extend_from_slice(b"\r\n--B0038--\r\n");
    let request = Request::builder()
        .method(Method::POST)
        .uri("/pdf")
        .header("content-type", "multipart/form-data; boundary=B0038")
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, body.to_vec())
}

/// The PDF's page count and its text with whitespace collapsed, by poppler (the container carries it).
fn pages_and_text(dir: &Path, pdf: &[u8]) -> (u32, String) {
    let path = dir.join("out.pdf");
    std::fs::write(&path, pdf).unwrap();
    let info = Command::new("pdfinfo").arg(&path).output().unwrap();
    let pages = String::from_utf8_lossy(&info.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|p| p.trim().parse().ok())
        .expect("pdfinfo names the page count");
    let text = Command::new("pdftotext")
        .arg(&path)
        .arg("-")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&text.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (pages, text)
}

/// The documents and texts sent to moalhaddar/docx-to-pdf (the DOCM sent also declared a VBA
/// project type, unused); moalhaddar/docx-to-pdf gave one page each.
fn samples() -> [(Format, Vec<u8>, &'static str); 4] {
    let docm = "Macro enabled sample zebra four one";
    let dotx = "Template sample walrus seven two";
    let ott_text = "Open template sample heron three nine";
    let fodt_text = "Flat open text sample otter five six";
    [
        (
            Format::Docm,
            ooxml(
                "application/vnd.ms-word.document.macroEnabled.main+xml",
                docm,
            ),
            docm,
        ),
        (
            Format::Dotx,
            ooxml(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml",
                dotx,
            ),
            dotx,
        ),
        (Format::Ott, ott(ott_text), ott_text),
        (Format::Fodt, fodt(fodt_text), fodt_text),
    ]
}

/// Each sample is the variant it stands for, its ZIP read through the central directory.
#[test]
fn the_samples_are_the_variants_they_stand_for() {
    for (format, document, _) in samples() {
        let sniffed = sniff(&mut std::io::Cursor::new(&document)).unwrap();
        assert_eq!(sniffed, Some(format));
    }
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "LibreOfficeKit runs on Linux only: tools/instance-test/run.sh"
)]
async fn the_admitted_word_variants_convert_as_the_oracle_converts_them() {
    if let Some(reason) = skip_reason() {
        eprintln!("skipped: {reason}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (profiles, spool) = (dir.path().join("profiles"), dir.path().join("spool"));
    std::fs::create_dir(&profiles).unwrap();
    std::fs::create_dir(&spool).unwrap();
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let settings = PoolSettings {
        size: 1,
        wait_timeout: Duration::from_secs(120),
        max_waiting: 4,
        start_timeout: Duration::from_secs(120),
    };
    let launcher = Launcher::new(env!("CARGO_BIN_EXE_docx-to-pdf"), &profiles);
    let pool = Pool::start(launcher, settings, readiness.clone(), &telemetry.meter())
        .await
        .unwrap();
    let mut upload = Upload::new(10 * 1024 * 1024);
    upload.spool_root = spool;
    let state = AppState::new(readiness, &telemetry).with_upload(upload);
    state.set_pool(pool.clone());
    let app = router(state);

    for (format, document, text) in &samples() {
        let what = format.name();
        let (status, pdf) = post(&app, document).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{what}: {}",
            String::from_utf8_lossy(&pdf)
        );
        assert_eq!(
            pages_and_text(dir.path(), &pdf),
            (1, text.to_string()),
            "{what}"
        );
    }

    pool.shutdown(Duration::from_secs(5)).await;
}
