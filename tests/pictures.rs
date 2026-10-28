//! Pictures past the threshold through the HTTP interface: a DOCX holding a PNG or JPEG past the picture threshold converts with the
//! picture downscaled, or replaced by a placeholder when it cannot be (damaged, interlaced, CMYK), and the answer says so in
//! `Docx-To-Pdf-Pictures`, `docx_to_pdf.pictures` and the log. A document without one reaches the instance byte for
//! byte, and its answer has no such header.
//!
//! The pool is a stand-in whose "PDF" is the document it was handed, so the body is what LibreOffice would load.

use std::io::{Cursor, Read, Write};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pictures::{PNG_PLACEHOLDER, Pictures};
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use docx_to_pdf::upload::Upload;
use http_body_util::BodyExt;
use tower::ServiceExt;

const CT: &[u8] = br#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

/// What the stand-in writes before the document, so the service takes its output for a PDF.
const PDF_PREFIX: &[u8] = b"%PDF-\n";

/// An instance that answers each conversion with its input behind [`PDF_PREFIX`].
fn echo_script() -> String {
    r#"
    printf 'ready\t1\t%s\n' "$$"
    tab="$(printf '\t')"
    while IFS="$tab" read -r cmd input output; do
        { printf '%%PDF-\n'; cat "$input"; } > "$output"
        printf 'ok\t1\t1\n'
    done
    "#
    .to_string()
}

struct Service {
    _dir: tempfile::TempDir,
    app: Router,
    telemetry: Telemetry,
}

impl Service {
    /// At the default picture threshold.
    async fn start() -> Self {
        Self::with_threshold(docx_to_pdf::config::DEFAULT_PICTURE_MAX_BITMAP).await
    }

    async fn with_threshold(max_bitmap: u64) -> Self {
        Self::with_pictures(|meter| Pictures::new(meter, max_bitmap)).await
    }

    async fn with_pictures(
        pictures: impl FnOnce(&opentelemetry::metrics::Meter) -> Pictures,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["profiles", "spool"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let telemetry = TelemetryBuilder::new()
            .with_otlp_from(|_| None)
            .build()
            .unwrap();
        let readiness = Readiness::new();
        let launcher = Launcher::with_args(
            "/bin/sh",
            ["-c".to_string(), echo_script(), "sh".to_string()],
            dir.path().join("profiles"),
        );
        let pool = Pool::start_with_limits(
            launcher,
            PoolSettings {
                size: 1,
                wait_timeout: Duration::from_secs(10),
                max_waiting: 8,
                start_timeout: Duration::from_secs(10),
            },
            InstanceLimits::default(),
            readiness.clone(),
            &telemetry.meter(),
        )
        .await
        .unwrap();
        let mut upload = Upload::new(1024 * 1024);
        upload.spool_root = dir.path().join("spool");
        let state = AppState {
            pictures: pictures(&telemetry.meter()),
            ..AppState::new(readiness, &telemetry).with_upload(upload)
        };
        state.set_pool(pool);
        Self {
            _dir: dir,
            app: router(state),
            telemetry,
        }
    }

    async fn post(&self, document: &[u8]) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut body = b"--B\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.docx\"\r\n\r\n".to_vec();
        body.extend_from_slice(document);
        body.extend_from_slice(b"\r\n--B--\r\n");
        let request = Request::builder()
            .method(Method::POST)
            .uri("/pdf")
            .header("content-type", "multipart/form-data; boundary=B")
            .header("origin", "https://client.example")
            .body(Body::from(body))
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes().to_vec();
        let input = bytes
            .strip_prefix(PDF_PREFIX)
            .map(<[u8]>::to_vec)
            .unwrap_or(bytes);
        (parts.status, parts.headers, input)
    }
}

/// A PNG's signature, `IHDR` and an empty `IDAT`: all the header check reads.
fn png(width: u32, height: u32, colour: u8) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend_from_slice(&13u32.to_be_bytes());
    out.extend_from_slice(b"IHDR");
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&[8, colour, 0, 0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"IDAT\0\0\0\0");
    out
}

fn docx(picture: &[u8]) -> Vec<u8> {
    docx_with("word/media/image1.png", picture)
}

fn docx_with(name: &str, picture: &[u8]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in [
        ("[Content_Types].xml", CT),
        ("word/document.xml", b"<w:document/>".as_slice()),
        (name, picture),
    ] {
        writer.start_file(name, deflated).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn entry(zip: &[u8], name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
    let mut content = Vec::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_end(&mut content)
        .unwrap();
    content
}

fn pictures_counted(telemetry: &Telemetry, outcome: &str) -> f64 {
    telemetry
        .prometheus_text()
        .lines()
        .find(|l| {
            l.starts_with("docx_to_pdf_pictures_total{")
                && l.contains(&format!("outcome=\"{outcome}\""))
        })
        .and_then(|l| l.rsplit(' ').next()?.parse().ok())
        .unwrap_or(0.0)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_libreoffice_would_drop_is_replaced_and_the_answer_says_so() {
    let service = Service::start().await;
    let (status, headers, converted) = service.post(&docx(&png(18919, 18919, 2))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=0, replaced=1");
    let exposed = headers["access-control-expose-headers"].to_str().unwrap();
    assert!(
        exposed
            .to_ascii_lowercase()
            .contains("docx-to-pdf-pictures"),
        "{exposed}"
    );
    assert_eq!(entry(&converted, "word/media/image1.png"), PNG_PLACEHOLDER);
    assert_eq!(entry(&converted, "word/document.xml"), b"<w:document/>");
    assert_eq!(pictures_counted(&service.telemetry, "replaced"), 1.0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_document_without_one_reaches_the_instance_as_it_was_sent() {
    let service = Service::start().await;
    let document = docx(&png(4000, 3000, 2));
    let (status, headers, converted) = service.post(&document).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("docx-to-pdf-pictures").is_none());
    assert_eq!(converted, document);
    assert_eq!(pictures_counted(&service.telemetry, "replaced"), 0.0);
}

/// A real RGB PNG of `width × height`, a gradient.
fn real_png(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    let mut writer = encoder.write_header().unwrap();
    let pixels: Vec<u8> = (0..height)
        .flat_map(|y| (0..width).flat_map(move |x| [x.to_le_bytes()[0], y.to_le_bytes()[0], 7]))
        .collect();
    writer.write_image_data(&pixels).unwrap();
    writer.finish().unwrap();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_png_past_the_threshold_is_downscaled_and_the_answer_says_so() {
    // RGB 400 × 300 holds 360 000 bytes; under 100 000 a half (90 000) is enough.
    let service = Service::with_threshold(100_000).await;
    let (status, headers, converted) = service.post(&docx(&real_png(400, 300))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=1, replaced=0");
    let picture = entry(&converted, "word/media/image1.png");
    let reader = png::Decoder::new(Cursor::new(picture)).read_info().unwrap();
    assert_eq!((reader.info().width, reader.info().height), (200, 150));
    assert_eq!(pictures_counted(&service.telemetry, "downscaled"), 1.0);
    assert_eq!(pictures_counted(&service.telemetry, "replaced"), 0.0);
}

/// A real 4:2:0 JPEG of `width × height`, a gradient, at quality `quality`: baseline or progressive, RGB or CMYK.
fn real_jpeg(width: u16, height: u16, quality: u8, progressive: bool, cmyk: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut out, quality);
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
    encoder.set_progressive(progressive);
    let pixels: Vec<u8> = (0..u32::from(height))
        .flat_map(|y| {
            (0..u32::from(width)).flat_map(move |x| [x.to_le_bytes()[0], y.to_le_bytes()[0], 7, 30])
        })
        .collect();
    let (pixels, colour) = if cmyk {
        (pixels, jpeg_encoder::ColorType::Cmyk)
    } else {
        let rgb = pixels.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        (rgb, jpeg_encoder::ColorType::Rgb)
    };
    encoder.encode(&pixels, width, height, colour).unwrap();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jpeg_past_the_threshold_is_downscaled_a_progressive_one_too_and_a_cmyk_one_replaced() {
    // RGB 400 × 300 holds 360 000 bytes; under 100 000 a half (90 000) is enough.
    let service = Service::with_threshold(100_000).await;
    for (progressive, downscaled) in [(false, 1.0), (true, 2.0)] {
        let (status, headers, converted) = service
            .post(&docx_with(
                "word/media/image1.jpeg",
                &real_jpeg(400, 300, 80, progressive, false),
            ))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=1, replaced=0");
        let picture = entry(&converted, "word/media/image1.jpeg");
        let mut decoder = jpeg_decoder::Decoder::new(picture.as_slice());
        decoder.decode().unwrap();
        let info = decoder.info().unwrap();
        assert_eq!((info.width, info.height), (200, 150));
        assert_eq!(
            pictures_counted(&service.telemetry, "downscaled"),
            downscaled
        );
    }

    // CMYK (four components) stays replaced: the downscale reads gray and YCbCr only.
    let (status, headers, converted) = service
        .post(&docx_with(
            "word/media/image1.jpeg",
            &real_jpeg(400, 300, 80, false, true),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=0, replaced=1");
    assert_eq!(
        entry(&converted, "word/media/image1.jpeg"),
        docx_to_pdf::pictures::JPEG_PLACEHOLDER
    );
    assert_eq!(pictures_counted(&service.telemetry, "replaced"), 1.0);
}

/// A downscale that panics, forced here, costs that picture its downscale, never the document.
#[tokio::test(flavor = "multi_thread")]
async fn a_downscale_that_panics_keeps_the_placeholder_and_the_answer_is_200() {
    fn panics(
        _: docx_to_pdf::pictures::Kind,
        _: &mut dyn Read,
        _: &mut dyn Write,
        _: u64,
        _: &std::path::Path,
    ) -> Result<docx_to_pdf::pictures::downscale::Downscaled, String> {
        panic!("a forced panic")
    }
    let service =
        Service::with_pictures(|meter| Pictures::new(meter, 100_000).with_downscale(panics)).await;
    let (status, headers, converted) = service.post(&docx(&real_png(400, 300))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=0, replaced=1");
    assert_eq!(entry(&converted, "word/media/image1.png"), PNG_PLACEHOLDER);
    assert_eq!(pictures_counted(&service.telemetry, "replaced"), 1.0);
}

/// What the service logs, as `fmt` writes it: captured on this thread.
#[derive(Clone, Default)]
struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// A panic anywhere in the pictures step, forced at each of its stages, converts the document as it
/// was received: 200, no `Docx-To-Pdf-Pictures`, `outcome="failed"` counted and an error logged.
#[tokio::test]
async fn a_panic_anywhere_in_the_pictures_step_converts_the_document_as_it_was_received() {
    use docx_to_pdf::pictures::{Fault, Stage};
    fn listing(stage: Stage) {
        assert!(stage != Stage::Listing, "a forced panic");
    }
    fn header(stage: Stage) {
        assert!(stage != Stage::Header, "a forced panic");
    }
    fn rewrite(stage: Stage) {
        assert!(stage != Stage::Rewrite, "a forced panic");
    }
    fn summary(stage: Stage) {
        assert!(stage != Stage::Summary, "a forced panic");
    }
    let logs = Logs::default();
    let writer = logs.clone();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish(),
    );
    let document = docx(&real_png(400, 300));
    for (stage, fault) in [
        ("listing", listing as Fault),
        ("header", header),
        ("rewrite", rewrite),
        ("summary", summary),
    ] {
        let service =
            Service::with_pictures(|meter| Pictures::new(meter, 100_000).with_fault(fault)).await;
        let (status, headers, converted) = service.post(&document).await;
        assert_eq!(status, StatusCode::OK, "{stage}");
        assert!(headers.get("docx-to-pdf-pictures").is_none(), "{stage}");
        assert_eq!(converted, document, "{stage}");
        assert_eq!(
            pictures_counted(&service.telemetry, "failed"),
            1.0,
            "{stage}"
        );
        assert_eq!(
            pictures_counted(&service.telemetry, "downscaled"),
            0.0,
            "{stage}"
        );
    }
    let logs = logs.text();
    let failed: Vec<_> = logs
        .lines()
        .filter(|l| l.contains("ERROR") && l.contains("failed"))
        .collect();
    assert_eq!(failed.len(), 4, "{logs}");
    assert!(
        failed.iter().all(|l| l.contains("a forced panic")),
        "{logs}"
    );
}

/// A DOCX holding each of `pictures`, named `word/media/image<n>.png`.
fn docx_many(pictures: &[Vec<u8>]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer.start_file("[Content_Types].xml", deflated).unwrap();
    writer.write_all(CT).unwrap();
    writer.start_file("word/document.xml", deflated).unwrap();
    writer.write_all(b"<w:document/>").unwrap();
    for (n, picture) in pictures.iter().enumerate() {
        writer
            .start_file(format!("word/media/image{}.png", n + 1), deflated)
            .unwrap();
        writer.write_all(picture).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// The eight-gigapixel picture bomb (`px-8x1024mpx`, eight gray 32 000² PNGs) is
/// eight pictures of 256 MiB each after the planned downscale, which together pass the 2 GiB hard memory limit. It is
/// answered 413 with the memory detail before any picture is decoded, within a second.
#[tokio::test(flavor = "multi_thread")]
async fn a_document_whose_pictures_pass_the_memory_budget_is_413_before_any_is_decoded() {
    static DECODED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn counted(
        kind: docx_to_pdf::pictures::Kind,
        input: &mut dyn Read,
        out: &mut dyn Write,
        bound: u64,
        scratch: &std::path::Path,
    ) -> Result<docx_to_pdf::pictures::downscale::Downscaled, String> {
        DECODED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        docx_to_pdf::pictures::downscale_picture(kind, input, out, bound, scratch)
    }
    let service = Service::with_pictures(|meter| {
        Pictures::new(meter, docx_to_pdf::config::DEFAULT_PICTURE_MAX_BITMAP)
            .with_downscale(counted)
    })
    .await;
    let bomb = docx_many(&vec![png(32000, 32000, 0); 8]);
    let started = std::time::Instant::now();
    let (status, headers, body) = service.post(&bomb).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert!(headers.get("docx-to-pdf-pictures").is_none());
    let body = String::from_utf8(body).unwrap();
    assert!(
        body.contains(docx_to_pdf::conversion::OVER_MEMORY),
        "{body}"
    );
    assert_eq!(DECODED.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Seven of them fit: 1 792 MiB, under what the 2 GiB limit leaves. Each is downscaled as before (these have no
    // pixels, so each is replaced).
    let (status, headers, _) = service
        .post(&docx_many(&vec![png(32000, 32000, 0); 7]))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=0, replaced=7");
    assert_eq!(DECODED.load(std::sync::atomic::Ordering::SeqCst), 7);
}

/// Pictures under the threshold count too, and a picture with alpha costs LibreOffice more than twice its bitmap
/// (RGBA 16 383², under LibreOffice's bound, peaks at 2.4 GiB): four RGBA 8 192² pictures, kept as they are, pass the budget although their bitmaps sum to
/// 1 GiB.
#[tokio::test(flavor = "multi_thread")]
async fn kept_pictures_count_and_alpha_costs_more() {
    let service = Service::start().await;
    let (status, _, _) = service.post(&docx_many(&vec![png(8192, 8192, 6); 4])).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, headers, _) = service.post(&docx_many(&vec![png(8192, 8192, 2); 4])).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("docx-to-pdf-pictures").is_none());
}

/// With no hard memory limit there is no budget: the bomb's pictures are downscaled as before.
#[tokio::test(flavor = "multi_thread")]
async fn without_a_hard_memory_limit_there_is_no_budget() {
    let service = Service::with_pictures(|meter| {
        Pictures::new(meter, docx_to_pdf::config::DEFAULT_PICTURE_MAX_BITMAP).with_hard_memory(0)
    })
    .await;
    let (status, headers, _) = service
        .post(&docx_many(&vec![png(32000, 32000, 0); 8]))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=0, replaced=8");
}

/// A downscale that takes `SLOW` and records how many run at once in `running`/`most`.
macro_rules! slow_downscale {
    ($name:ident, $running:ident, $most:ident) => {
        static $running: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        static $most: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn $name(
            kind: docx_to_pdf::pictures::Kind,
            input: &mut dyn Read,
            out: &mut dyn Write,
            bound: u64,
            scratch: &std::path::Path,
        ) -> Result<docx_to_pdf::pictures::downscale::Downscaled, String> {
            use std::sync::atomic::Ordering::SeqCst;
            let now = $running.fetch_add(1, SeqCst) + 1;
            $most.fetch_max(now, SeqCst);
            std::thread::sleep(Duration::from_millis(400));
            $running.fetch_sub(1, SeqCst);
            docx_to_pdf::pictures::downscale_picture(kind, input, out, bound, scratch)
        }
    };
}

/// The pictures step runs before a request takes an instance, so it holds a bound of its own. Three
/// documents to downscale at once, at a bound of one: all convert, never two downscaled in parallel.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::tuple_array_conversions,
    reason = "the three answers of one join, checked alike"
)]
async fn the_pictures_step_downscales_at_most_its_bound_of_documents_at_once() {
    slow_downscale!(slow, RUNNING, MOST);
    let service = Service::with_pictures(|meter| {
        Pictures::new(meter, 100_000)
            .with_downscale(slow)
            .with_concurrency(1, Duration::from_secs(10))
    })
    .await;
    let document = docx(&real_png(400, 300));
    let (a, b, c) = tokio::join!(
        service.post(&document),
        service.post(&document),
        service.post(&document)
    );
    for (status, headers, _) in [a, b, c] {
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["docx-to-pdf-pictures"], "downscaled=1, replaced=0");
    }
    assert_eq!(MOST.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Past the wait, the excess is refused 503 with `Retry-After`, as the pool refuses it; a document with nothing to
/// downscale does not wait for the bound at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_excess_past_the_wait_is_refused_503_and_a_document_without_oversized_pictures_never_waits()
 {
    slow_downscale!(slow, RUNNING, MOST);
    let service = Service::with_pictures(|meter| {
        Pictures::new(meter, 100_000)
            .with_downscale(slow)
            .with_concurrency(1, Duration::ZERO)
    })
    .await;
    let document = docx(&real_png(400, 300));
    let small = docx(&real_png(100, 100));
    let late = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let refused = service.post(&document).await;
        let small = service.post(&small).await;
        (refused, small)
    };
    let (first, (refused, small_answer)) = tokio::join!(service.post(&document), late);
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(refused.0, StatusCode::SERVICE_UNAVAILABLE);
    assert!(refused.1.get("retry-after").is_some());
    let body = String::from_utf8(refused.2).unwrap();
    assert!(
        body.contains("no instance came free within the wait limit"),
        "{body}"
    );
    assert_eq!(small_answer.0, StatusCode::OK);
    assert_eq!(small_answer.2, small);
    assert_eq!(MOST.load(std::sync::atomic::Ordering::SeqCst), 1);
}
