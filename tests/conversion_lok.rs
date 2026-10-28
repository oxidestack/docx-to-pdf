//! A conversion with real LibreOffice: `POST /pdf` through the router, on a pool of real instances (this binary
//! in instance mode, hosting LibreOfficeKit). Upstream's four examples answer their PDFs; a truncated DOCX, which
//! the allowlist admits, answers 422 and leaves the instance usable; every conversion's directory is gone.
//! Whether the PDFs are equivalent to moalhaddar/docx-to-pdf's is the scenarios' question
//! (`tests/scenarios/convert-*`). A stuck real instance (its process group stopped with `SIGSTOP`) answers
//! 504 past the conversion timeout, and the next request is served by a fresh instance. moalhaddar/docx-to-pdf's
//! never-ending document, `tdf38575_fullWidthLine.docx`, which hangs its LibreOffice 7.3, converts on LibreOffice 26.8, so it is
//! checked as a 200.
//!
//! LibreOfficeKit does not run on macOS: this runs in `tools/instance-test/` (`run.sh`). Off Linux the
//! LibreOffice tests are ignored with the reason; on a Linux without LibreOffice they print a skip and pass,
//! unless `DOCX_TO_PDF_REQUIRE_LOK=1` (the container sets it).

#[path = "../src/instance/group.rs"]
mod group;

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use docx_to_pdf::allowlist::{Format, sniff_file};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::PROGRAM_DIR;
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;
use docx_to_pdf::upload::Upload;
use group::group_alive;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The instances' stderr and the pool's events, in a failing test's output.
fn log_to_test_output() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_test_writer()
                .with_max_level(tracing::Level::INFO)
                .finish(),
        );
    });
}

fn skip_reason() -> Option<String> {
    log_to_test_output();
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

fn example(n: u32) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("upstream/docx-to-pdf/examples/ex{n}/input.docx"))
}

/// Upstream's ex1 cut where its central directory begins: every entry's local header and data survive,
/// `[Content_Types].xml` (the last entry) among them, so the allowlist admits it; the archive's
/// directory does not.
fn truncated_docx() -> Vec<u8> {
    let whole = std::fs::read(example(1)).unwrap();
    let directory = whole
        .windows(4)
        .position(|w| w == b"PK\x01\x02")
        .expect("a central directory");
    whole[..directory].to_vec()
}

async fn post(app: &Router, document: &[u8]) -> (StatusCode, String, Vec<u8>) {
    let mut body = b"--B0015\r\nContent-Disposition: form-data; name=\"document\"; filename=\"input.docx\"\r\n\r\n".to_vec();
    body.extend_from_slice(document);
    body.extend_from_slice(b"\r\n--B0015--\r\n");
    let request = Request::builder()
        .method(Method::POST)
        .uri("/pdf")
        .header("content-type", "multipart/form-data; boundary=B0015")
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
    (status, content_type, body.to_vec())
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().next().is_none()
}

#[tokio::test]
async fn the_truncated_docx_passes_the_allowlist_so_libreoffice_decides() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("document");
    std::fs::write(&path, truncated_docx()).unwrap();
    assert_eq!(sniff_file(&path).await.unwrap(), Some(Format::Docx));
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "LibreOfficeKit runs on Linux only: tools/instance-test/run.sh"
)]
async fn upstreams_examples_convert_and_a_truncated_docx_is_422() {
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
    upload.spool_root = spool.clone();
    let state = AppState::new(readiness, &telemetry).with_upload(upload);
    state.set_pool(pool.clone());
    let app = router(state);

    for n in 1..=4 {
        let (status, content_type, pdf) = post(&app, &std::fs::read(example(n)).unwrap()).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "ex{n}: {}",
            String::from_utf8_lossy(&pdf)
        );
        assert_eq!(content_type, "application/pdf", "ex{n}");
        assert!(pdf.starts_with(b"%PDF-"), "ex{n}");
        assert!(is_empty(&spool), "ex{n}: the directory is left");
    }

    let (status, content_type, body) = post(&app, &truncated_docx()).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(content_type, "application/problem+json");
    assert!(is_empty(&spool), "the 422's directory is left");

    let (status, _, _) = post(&app, &std::fs::read(example(1)).unwrap()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the instance stays usable after a 422"
    );
    assert_eq!(pool.counts().idle, 1);

    pool.shutdown(Duration::from_secs(5)).await;
    assert!(is_empty(&profiles), "a profile is left");
}

/// `tdf38575_fullWidthLine.docx`, a LibreOffice regression document (MPL), not in the repository: from
/// `DOCX_TO_PDF_TDF38575`, else `/fixtures/docx/` (run.sh mounts `$DOCX_TO_PDF_FIXTURES` there), else
/// `$DOCX_TO_PDF_FIXTURES/`, a local directory of LibreOffice's DOCX regression documents.
fn tdf38575() -> Option<PathBuf> {
    let name = "tdf38575_fullWidthLine.docx";
    let candidates = [
        std::env::var_os("DOCX_TO_PDF_TDF38575").map(PathBuf::from),
        Some(Path::new("/fixtures/docx").join(name)),
        std::env::var_os("DOCX_TO_PDF_FIXTURES").map(|dir| Path::new(&dir).join(name)),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

/// The conversion timeout on a real instance that cannot finish: its process group stopped (`SIGSTOP`), as a hung
/// LibreOffice would be. Past the conversion timeout: 504, the instance killed with its group (`SIGKILL` ends a
/// stopped process) and its profile deleted, the next request served by a fresh instance, every directory
/// gone. First, tdf38575, which never finishes on moalhaddar/docx-to-pdf, converts here.
#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "LibreOfficeKit runs on Linux only: tools/instance-test/run.sh"
)]
#[allow(
    unsafe_code,
    reason = "a signal to an instance, which std has no form of"
)]
async fn a_stuck_instance_is_504_past_the_timeout_and_the_next_is_served_by_a_fresh_one() {
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
    let timeout = Duration::from_secs(10);
    let limits = InstanceLimits {
        conversion_timeout: timeout,
        ..InstanceLimits::default()
    };
    let launcher = Launcher::new(env!("CARGO_BIN_EXE_docx-to-pdf"), &profiles);
    let pool = Pool::start_with_limits(
        launcher,
        settings,
        limits,
        readiness.clone(),
        &telemetry.meter(),
    )
    .await
    .unwrap();
    let mut upload = Upload::new(10 * 1024 * 1024);
    upload.spool_root = spool.clone();
    let state = AppState::new(readiness, &telemetry).with_upload(upload);
    state.set_pool(pool.clone());
    let app = router(state);
    match tdf38575() {
        Some(fixture) => {
            let began = std::time::Instant::now();
            let (status, _, pdf) = post(&app, &std::fs::read(&fixture).unwrap()).await;
            eprintln!(
                "tdf38575 on LibreOffice 26.8: {status} after {:?}",
                began.elapsed()
            );
            assert_eq!(
                status,
                StatusCode::OK,
                "the oracle hangs on it; 26.8 does not"
            );
            assert!(pdf.starts_with(b"%PDF-"));
        }
        None => eprintln!(
            "tdf38575 part skipped: tdf38575_fullWidthLine.docx is not here (a LibreOffice test document, MPL, \
             kept out of the repository; set DOCX_TO_PDF_TDF38575 or DOCX_TO_PDF_FIXTURES, or mount a directory \
             holding it at /fixtures/docx)"
        ),
    }

    let first = {
        let lease = pool.acquire().await.unwrap();
        lease.pid()
    };
    let group = -first.cast_signed();
    // SAFETY: SIGSTOP to the process group of the instance this test's pool started.
    assert_eq!(unsafe { libc::kill(group, libc::SIGSTOP) }, 0);

    let began = std::time::Instant::now();
    let (status, content_type, body) = post(&app, &std::fs::read(example(1)).unwrap()).await;
    let took = began.elapsed();
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(content_type, "application/problem+json");
    assert!(
        took >= timeout && took < timeout + Duration::from_secs(5),
        "{took:?}"
    );
    assert!(is_empty(&spool), "the 504's directory is left");

    let (status, _, pdf) = post(&app, &std::fs::read(example(1)).unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&pdf));
    let next = pool.acquire().await.unwrap().pid();
    assert_ne!(next, first, "a fresh instance serves the next request");
    // A zombie of the killed group is gone too: a CI job's PID 1 never reaps it.
    assert!(
        !group_alive(first),
        "the timed-out instance's group is gone"
    );
    let text = telemetry.prometheus_text();
    assert!(
        text.lines()
            .any(|l| l.starts_with("docx_to_pdf_instance_starts_total{")
                && l.contains("reason=\"timeout\"")
                && l.ends_with(" 1")),
        "{text}"
    );
    assert_eq!(
        std::fs::read_dir(&profiles).unwrap().count(),
        1,
        "only the replacement's profile"
    );

    pool.shutdown(Duration::from_secs(5)).await;
    assert!(is_empty(&profiles), "a profile is left");
    assert!(is_empty(&spool));
}
