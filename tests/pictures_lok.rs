//! Pictures past the threshold with real LibreOffice: a PNG or JPEG past the threshold is downscaled and LibreOffice draws it at
//! its new size; a damaged one is replaced by the placeholder, drawn in the picture's frame; under the threshold the
//! picture is drawn at its own size. The corpus's `px-4mpx` (one 2 000² RGB PNG, a 12 MB bitmap) is converted with
//! the threshold below and above that bitmap, and with its PNG cut in half; the corpus's `tdf81345` with its largest
//! JPEG past the threshold, as it is and made progressive (both halved), and made CMYK (the
//! placeholder). `pdfimages -list` (poppler-utils, in `tools/instance-test/`) reads the PDF.
//!
//! LibreOfficeKit does not run on macOS: this runs in `tools/instance-test/` (`run.sh`) and CI's `test` job.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::PROGRAM_DIR;
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pictures::Pictures;
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

async fn post(app: &Router, document: &[u8]) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut body =
        b"--B\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.docx\"\r\n\r\n"
            .to_vec();
    body.extend_from_slice(document);
    body.extend_from_slice(b"\r\n--B--\r\n");
    let request = Request::builder()
        .method(Method::POST)
        .uri("/pdf")
        .header("content-type", "multipart/form-data; boundary=B")
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    (
        parts.status,
        parts.headers,
        body.collect().await.unwrap().to_bytes().to_vec(),
    )
}

/// `document` with its entry `name` holding `content` instead, every other entry as it was.
fn with_entry(document: &[u8], name: &str, content: &[u8]) -> Vec<u8> {
    use std::io::{Cursor, Read, Write};
    let mut archive = zip::ZipArchive::new(Cursor::new(document)).unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        let entry = file.name().unwrap().into_owned();
        writer.start_file(entry.as_str(), deflated).unwrap();
        writer
            .write_all(if entry == name { content } else { &bytes })
            .unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// The content of `document`'s entry `name`.
fn entry(document: &[u8], name: &str) -> Vec<u8> {
    use std::io::{Cursor, Read};
    let mut archive = zip::ZipArchive::new(Cursor::new(document)).unwrap();
    let mut bytes = Vec::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

/// Width and height of each image `pdfimages -list` lists in `pdf`.
fn images(pdf: &[u8]) -> Vec<(u32, u32)> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.pdf");
    std::fs::write(&path, pdf).unwrap();
    let out = Command::new("pdfimages")
        .arg("-list")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .skip(2)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            (fields.get(2) == Some(&"image"))
                .then(|| (fields[3].parse().unwrap(), fields[4].parse().unwrap()))
        })
        .collect()
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "LibreOfficeKit runs on Linux only: tools/instance-test/run.sh"
)]
async fn libreoffice_draws_the_downscaled_picture_or_the_placeholder_in_its_frame() {
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
    let app = |max_bitmap: u64| {
        let mut upload = Upload::new(10 * 1024 * 1024);
        upload.spool_root = spool.clone();
        let state = AppState {
            pictures: Pictures::new(&telemetry.meter(), max_bitmap),
            ..AppState::new(readiness.clone(), &telemetry).with_upload(upload)
        };
        state.set_pool(pool.clone());
        router(state)
    };
    let document = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/documents/px/px-4mpx.docx"),
    )
    .unwrap();

    // Under 1 MiB: 1 000² (3 MB) is still past it, 500² (750 000 bytes) is not.
    let (status, headers, pdf) = post(&app(1024 * 1024), &document).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&pdf));
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=1, replaced=0");
    assert_eq!(
        images(&pdf),
        [(500, 500)],
        "the picture, a quarter of each side"
    );

    let picture = entry(&document, "word/media/image1.png");
    let damaged = with_entry(
        &document,
        "word/media/image1.png",
        &picture[..picture.len() / 2],
    );
    let (status, headers, pdf) = post(&app(1024 * 1024), &damaged).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&pdf));
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=0, replaced=1");
    assert_eq!(images(&pdf), [(64, 64)], "the placeholder, drawn");

    let (status, headers, pdf) = post(&app(256 * 1024 * 1024), &document).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("docx-to-pdf-pictures").is_none());
    let drawn = images(&pdf);
    assert_eq!(drawn.len(), 1);
    assert!(
        drawn[0].0 * drawn[0].1 >= 4_000_000,
        "the picture itself: {drawn:?}"
    );

    // The corpus's tdf81345 holds a baseline 4:4:4 JPEG of 888 × 552 (a 1 470 528-byte bitmap) beside a
    // smaller JPEG and two PNGs under 1 MiB. Under 1 MiB it is halved by DCT scaling and LibreOffice draws it so.
    let document = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/corpus/documents/third-party/libreoffice/tdf81345.docx"),
    )
    .unwrap();
    let (status, headers, pdf) = post(&app(1024 * 1024), &document).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&pdf));
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=1, replaced=0");
    let drawn = images(&pdf);
    assert!(drawn.contains(&(444, 276)), "the JPEG, halved: {drawn:?}");
    assert!(!drawn.contains(&(888, 552)), "{drawn:?}");

    // Made progressive, it is halved and drawn so too.
    let original = entry(&document, "word/media/image2.jpeg");
    let mut decoder = jpeg_decoder::Decoder::new(original.as_slice());
    let pixels = decoder.decode().unwrap();
    let mut progressive = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut progressive, 90);
    encoder.set_progressive(true);
    encoder
        .encode(&pixels, 888, 552, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    let halved = with_entry(&document, "word/media/image2.jpeg", &progressive);
    let (status, headers, pdf) = post(&app(1024 * 1024), &halved).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&pdf));
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=1, replaced=0");
    let drawn = images(&pdf);
    assert!(
        drawn.contains(&(444, 276)),
        "the progressive JPEG, halved: {drawn:?}"
    );

    // Made CMYK, it cannot be downscaled (the downscale reads gray and YCbCr only): the JPEG placeholder is drawn in its frame.
    let cmyk_pixels: Vec<u8> = pixels
        .chunks(3)
        .flat_map(|p| [255 - p[0], 255 - p[1], 255 - p[2], 0])
        .collect();
    let mut cmyk = Vec::new();
    jpeg_encoder::Encoder::new(&mut cmyk, 90)
        .encode(&cmyk_pixels, 888, 552, jpeg_encoder::ColorType::Cmyk)
        .unwrap();
    let replaced = with_entry(&document, "word/media/image2.jpeg", &cmyk);
    let (status, headers, pdf) = post(&app(1024 * 1024), &replaced).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&pdf));
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=0, replaced=1");
    let drawn = images(&pdf);
    assert!(
        drawn.contains(&(64, 64)),
        "the placeholder, drawn: {drawn:?}"
    );
}
