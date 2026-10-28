//! A conversion, its timeout and recycling at the HTTP seam: `POST /pdf` through the router, with a pool of stand-in instances
//! (`sh` speaking the instance protocol, so the server's side runs without LibreOffice) so that every path a conversion ends in can be
//! provoked on any platform. The real LibreOffice behind the same router is `tests/conversion_lok.rs`.
//!
//! The stand-in decides by the document's content: `BROKEN` answers `not_loaded`, `EXPORT` answers
//! `export_failed`, `DIE` kills the instance mid-conversion, `SLOW` waits before answering, `NOTPDF` stores a file
//! that is not a PDF, `LARGE <n>` a PDF of `<n>` bytes after its header, `BOMB` turns the instance (`exec`, the same
//! pid) into a Perl that holds 100 MB and sleeps; anything else gets a small PDF naming the input file it was given
//! and the instance's pid.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::num::ToF64;
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use docx_to_pdf::upload::Upload;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn script(markers: &Path) -> String {
    format!(
        r#"
        test -d "$1" || exit 9
        printf 'ready\t1\t%s\n' "$$"
        tab="$(printf '\t')"
        while IFS="$tab" read -r cmd input output; do
            : > '{markers}/started'
            case "$(cat "$input")" in
                *BROKEN*) printf 'not_loaded\tGeneral Error\n' ;;
                *EXPORT*) printf 'export_failed\tGeneral Error\n' ;;
                *DIE*) kill -9 $$ ;;
                *SLOW*) sleep 30; printf 'ok\t1\t1\n' ;;
                *BOMB*) exec perl -e '$x = "x" x 100000000; sleep 30' ;;
                *NOTPDF*) printf 'not a pdf\n' > "$output"; printf 'ok\t1\t1\n' ;;
                *LARGE*)
                    n="$(sed -n 's/.*LARGE \([0-9]*\).*/\1/p' "$input")"
                    {{ printf '%%PDF-1.4\n'; head -c "$n" /dev/zero; }} > "$output"
                    printf 'ok\t1\t1\n' ;;
                *)
                    printf '%%PDF-1.4\n%% input=%s output-dir=%s\n%% pid=%s\n%%%%EOF\n' "$input" "$(dirname "$output")" "$$" > "$output"
                    printf 'ok\t3\t4\n' ;;
            esac
        done
        "#,
        markers = markers.display()
    )
}

struct Service {
    dir: tempfile::TempDir,
    app: Router,
    _telemetry: Telemetry,
    pool: Pool,
}

impl Service {
    async fn start(size: usize) -> Self {
        Self::start_with(size, Duration::from_millis(300), InstanceLimits::default()).await
    }

    async fn start_with(size: usize, wait_timeout: Duration, limits: InstanceLimits) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["profiles", "markers", "spool"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let telemetry = TelemetryBuilder::new().build().unwrap();
        let readiness = Readiness::new();
        let launcher = Launcher::with_args(
            "/bin/sh",
            [
                "-c".to_string(),
                script(&dir.path().join("markers")),
                "sh".to_string(),
            ],
            dir.path().join("profiles"),
        );
        let settings = PoolSettings {
            size,
            wait_timeout,
            max_waiting: if wait_timeout > Duration::from_secs(1) {
                32
            } else {
                0
            },
            start_timeout: Duration::from_secs(10),
        };
        let pool = Pool::start_with_limits(
            launcher,
            settings,
            limits,
            readiness.clone(),
            &telemetry.meter(),
        )
        .await
        .unwrap();
        let mut upload = Upload::new(1024 * 1024);
        upload.spool_root = dir.path().join("spool");
        let state = AppState::new(readiness, &telemetry).with_upload(upload);
        state.set_pool(pool.clone());
        Self {
            app: router(state),
            dir,
            _telemetry: telemetry,
            pool,
        }
    }

    fn spool(&self) -> PathBuf {
        self.dir.path().join("spool")
    }

    fn spooled(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.spool())
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }

    fn request(document: &[u8]) -> Request<Body> {
        let mut body = b"--B0015\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.docx\"\r\n\r\n".to_vec();
        body.extend_from_slice(document);
        body.extend_from_slice(b"\r\n--B0015--\r\n");
        Request::builder()
            .method(Method::POST)
            .uri("/pdf")
            .header("content-type", "multipart/form-data; boundary=B0015")
            .header("origin", "http://a.example")
            .body(Body::from(body))
            .unwrap()
    }

    async fn post(&self, document: &[u8]) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let response = self
            .app
            .clone()
            .oneshot(Self::request(document))
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, body.to_vec())
    }

    async fn metrics(&self) -> String {
        let request = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body.to_vec()).unwrap()
    }
}

/// The value of the first `/metrics` line of `name` whose labels contain every one of `labels`.
fn sample(text: &str, name: &str, labels: &[&str]) -> Option<f64> {
    text.lines()
        .filter(|l| l.starts_with(&format!("{name}{{")))
        .find(|l| labels.iter().all(|label| l.contains(label)))
        .and_then(|l| l.rsplit(' ').next())
        .and_then(|v| v.parse().ok())
}

fn conversions(text: &str, outcome: &str) -> f64 {
    sample(
        text,
        "docx_to_pdf_conversions_total",
        &[&format!(r#"outcome="{outcome}""#)],
    )
    .unwrap_or(0.0)
}

fn durations(text: &str, outcome: &str) -> f64 {
    sample(
        text,
        "docx_to_pdf_conversion_duration_seconds_count",
        &[&format!(r#"outcome="{outcome}""#)],
    )
    .unwrap_or(0.0)
}

/// The pid of the stand-in instance that wrote the PDF.
fn pid_of(pdf: &[u8]) -> u32 {
    let text = String::from_utf8_lossy(pdf);
    text.split("pid=")
        .nth(1)
        .and_then(|s| s.lines().next())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("no pid in {text}"))
}

fn starts(text: &str, reason: &str) -> f64 {
    sample(
        text,
        "docx_to_pdf_instance_starts_total",
        &[&format!(r#"reason="{reason}""#)],
    )
    .unwrap_or(0.0)
}

/// Waits until `condition` holds on a `/metrics` scrape, and returns the last scrape.
async fn metrics_until(service: &Service, what: &str, condition: impl Fn(&str) -> bool) -> String {
    let mut metrics = String::new();
    for _ in 0..400 {
        metrics = service.metrics().await;
        if condition(&metrics) {
            return metrics;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never: {what}\n{metrics}");
}

/// Every test's log lines in this binary: a global subscriber, since a scoped one misses callsites whose
/// interest another test thread cached first.
#[derive(Clone, Default)]
struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

static LOG: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn global() -> &'static Self {
        LOG.get_or_init(|| {
            let log = Self::default();
            let writer = log.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .finish();
            tracing::subscriber::set_global_default(subscriber).unwrap();
            log
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn problem(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

#[tokio::test]
async fn an_allowed_document_answers_200_with_its_pdf_named_by_its_format_in_its_own_directory() {
    let service = Service::start(1).await;
    let (status, headers, body) = service.post(b"{\\rtf1 hello}").await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers["content-type"], "application/pdf");
    assert_eq!(headers["access-control-allow-origin"], "*");
    let text = String::from_utf8_lossy(&body);
    assert!(text.starts_with("%PDF-"), "{text}");
    let input = text
        .split("input=")
        .nth(1)
        .and_then(|s| s.split(' ').next())
        .unwrap();
    assert!(input.ends_with("/document.rtf"), "{text}");
    let output_dir = text
        .split("output-dir=")
        .nth(1)
        .and_then(|s| s.lines().next())
        .unwrap();
    assert_eq!(Path::new(input).parent().unwrap(), Path::new(output_dir));
    assert!(Path::new(input).starts_with(service.spool()), "{text}");

    assert!(service.spooled().is_empty(), "{:?}", service.spooled());
    let metrics = service.metrics().await;
    assert_eq!(conversions(&metrics, "success"), 1.0, "{metrics}");
    assert!(
        sample(
            &metrics,
            "docx_to_pdf_conversions_total",
            &[r#"outcome="success""#, r#"retried="false""#]
        )
        .is_some(),
        "{metrics}"
    );
    assert_eq!(durations(&metrics, "success"), 1.0, "{metrics}");
    assert_eq!(
        sample(
            &metrics,
            "docx_to_pdf_conversion_duration_seconds_sum",
            &[r#"outcome="success""#]
        ),
        Some(0.007),
        "LibreOffice's own time, load + export: {metrics}"
    );
    assert_eq!(
        sample(&metrics, "docx_to_pdf_pdf_size_bytes_sum", &[]),
        Some(body.len().to_f64()),
        "{metrics}"
    );
}

#[tokio::test]
async fn a_document_the_backend_cannot_load_is_422_and_its_directory_is_gone() {
    let service = Service::start(1).await;
    let (status, headers, body) = service.post(b"{\\rtf1 BROKEN}").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(headers["access-control-allow-origin"], "*");
    let problem = problem(&body);
    assert_eq!(problem["status"], 422);
    // The answer does not name the backend, so it survives a change of backend and does not advertise it.
    assert_eq!(
        problem["detail"], "the document could not be loaded",
        "{problem}"
    );
    assert!(
        !problem["detail"]
            .as_str()
            .unwrap()
            .contains("General Error"),
        "the backend's text stays in the log: {problem}"
    );
    assert_eq!(service.spooled(), [] as [std::path::PathBuf; 0]);
    let metrics = service.metrics().await;
    assert_eq!(conversions(&metrics, "not_loaded"), 1.0, "{metrics}");
    assert_eq!(durations(&metrics, "not_loaded"), 1.0, "{metrics}");

    // The instance stays usable.
    let (status, _, _) = service.post(b"{\\rtf1 fine}").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_failed_export_is_500_counted_as_error() {
    let service = Service::start(1).await;
    let (status, _, body) = service.post(b"{\\rtf1 EXPORT}").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(problem(&body)["status"], 500);
    assert_eq!(service.spooled(), [] as [std::path::PathBuf; 0]);
    let metrics = service.metrics().await;
    assert_eq!(conversions(&metrics, "error"), 1.0, "{metrics}");
    assert_eq!(durations(&metrics, "error"), 1.0, "{metrics}");
}

#[tokio::test]
async fn an_instance_that_dies_on_the_retry_too_is_500_dead_instance_retried() {
    let service = Service::start(2).await;
    let (status, _, body) = service.post(b"{\\rtf1 DIE}").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(problem(&body)["status"], 500);
    assert_eq!(service.spooled(), [] as [std::path::PathBuf; 0]);
    let metrics = service.metrics().await;
    assert_eq!(
        sample(
            &metrics,
            "docx_to_pdf_conversions_total",
            &[r#"outcome="dead_instance""#, r#"retried="true""#]
        ),
        Some(1.0),
        "{metrics}"
    );
    assert_eq!(durations(&metrics, "dead_instance"), 1.0, "{metrics}");
}

#[tokio::test]
async fn no_free_instance_within_the_wait_is_503_with_retry_after() {
    let service = Service::start(1).await;
    let lease = service.pool.acquire().await.unwrap();
    let (status, headers, body) = service.post(b"{\\rtf1 hello}").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(headers.contains_key("retry-after"));
    let problem = problem(&body);
    assert_eq!(problem["status"], 503);
    // The answer does not name the backend.
    assert_eq!(
        problem["detail"], "too many requests are already waiting for an instance",
        "{problem}"
    );
    assert_eq!(service.spooled(), [] as [std::path::PathBuf; 0]);
    drop(lease);
}

#[tokio::test]
async fn a_refused_document_is_counted_as_unsupported() {
    let service = Service::start(1).await;
    let (status, _, _) = service.post(b"plain text").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let metrics = service.metrics().await;
    assert_eq!(conversions(&metrics, "unsupported"), 1.0, "{metrics}");
}

/// The client goes away mid-conversion, the request future is dropped; the conversion counts as
/// `cancelled`, its instance is killed and replaced (reason `cancelled`), its directory is gone.
#[tokio::test]
async fn a_client_that_disconnects_mid_conversion_cancels_it() {
    let log = Captured::global();
    let service = Service::start(1).await;
    let started = service.dir.path().join("markers/started");
    let app = service.app.clone();
    let request = tokio::spawn(app.oneshot(Service::request(b"{\\rtf1 SLOW}")));
    for _ in 0..200 {
        if started.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(started.exists(), "the instance never got the conversion");
    request.abort();
    let _ = request.await;
    let aborted = std::time::Instant::now();

    let mut metrics = String::new();
    for _ in 0..200 {
        metrics = service.metrics().await;
        let replaced = sample(
            &metrics,
            "docx_to_pdf_instance_starts_total",
            &[r#"reason="cancelled""#],
        );
        if replaced == Some(1.0) && service.spooled().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(conversions(&metrics, "cancelled"), 1.0, "{metrics}");
    assert_eq!(durations(&metrics, "cancelled"), 1.0, "{metrics}");
    assert_eq!(
        sample(
            &metrics,
            "docx_to_pdf_instance_starts_total",
            &[r#"reason="cancelled""#]
        ),
        Some(1.0),
        "{metrics}"
    );
    assert!(service.spooled().is_empty(), "{:?}", service.spooled());

    // The replacement serves the next conversion, at once: not after the 30 s the abandoned one would take.
    let (status, _, _) = service.post(b"{\\rtf1 after}").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        aborted.elapsed() < Duration::from_secs(10),
        "{:?}",
        aborted.elapsed()
    );

    // A log records it: the conversion cancelled, its instance killed and replaced.
    let log = log.text();
    assert!(
        log.contains("the client went away before the answer"),
        "{log}"
    );
    assert!(
        log.contains("instance retired") && log.contains("reason=\"cancelled\""),
        "{log}"
    );
}

/// A conversion past `DOCX_TO_PDF_CONVERSION_TIMEOUT` answers 504 within the timeout plus a small
/// margin; its instance is killed with its group and replaced (reason `timeout`); its directory is gone; the
/// next request is served by a fresh instance.
#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn a_conversion_past_the_timeout_is_504_and_the_next_is_served_by_a_fresh_instance() {
    // The timeout bounds every conversion on the pool, the request after the replacement's too, so it sits far
    // above what a small document takes on a loaded machine under coverage instrumentation (500 ms was
    // passed there) and far below the 30 s the SLOW document sleeps.
    let timeout = Duration::from_secs(5);
    let service = Service::start_with(
        1,
        Duration::from_secs(10),
        InstanceLimits {
            conversion_timeout: timeout,
            ..InstanceLimits::default()
        },
    )
    .await;
    // The instance's pid through a lease: no conversion, so nothing to race the timeout before the SLOW one.
    let before = service.pool.acquire().await.unwrap().pid();

    let began = std::time::Instant::now();
    let (status, headers, body) = service.post(b"{\\rtf1 SLOW}").await;
    let took = began.elapsed();
    assert_eq!(
        status,
        StatusCode::GATEWAY_TIMEOUT,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert_eq!(problem(&body)["status"], 504);
    assert!(took >= timeout, "{took:?}");
    assert!(took < timeout + Duration::from_secs(2), "{took:?}");
    assert!(service.spooled().is_empty(), "{:?}", service.spooled());

    let metrics = metrics_until(&service, "the replacement counted as timeout", |m| {
        starts(m, "timeout") == 1.0
    })
    .await;
    assert_eq!(conversions(&metrics, "timeout"), 1.0, "{metrics}");
    assert_eq!(durations(&metrics, "timeout"), 1.0, "{metrics}");
    assert_eq!(conversions(&metrics, "cancelled"), 0.0, "{metrics}");
    assert_eq!(starts(&metrics, "cancelled"), 0.0, "{metrics}");

    let fresh = service.pool.acquire().await.unwrap().pid();
    assert_ne!(fresh, before, "the timed-out instance was replaced");
    let (status, _, pdf) = service.post(b"{\\rtf1 after}").await;
    assert_eq!(status, StatusCode::OK);
    // The PDF names the pid the lease read: the request is served by the fresh instance, not the timed-out one.
    assert_eq!(
        pid_of(&pdf),
        fresh,
        "a fresh instance serves the next request"
    );
}

/// A conversion whose instance passes `DOCX_TO_PDF_INSTANCE_HARD_MEMORY` answers
/// 413 with its fixed detail at once, not after the conversion timeout; the instance is killed and replaced (reason
/// `memory_limit`), the conversion counted `memory_limit` and not retried, its directory gone, a log line gives the
/// reason; the next request is served by a fresh instance.
#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn a_conversion_above_the_hard_memory_limit_is_413_and_the_next_is_served_by_a_fresh_instance()
 {
    let log = Captured::global();
    let service = Service::start_with(
        1,
        Duration::from_secs(10),
        InstanceLimits {
            max_memory: 0,
            hard_memory: 50 * 1024 * 1024,
            ..InstanceLimits::default()
        },
    )
    .await;
    let (status, _, pdf) = service.post(b"{\\rtf1 before}").await;
    assert_eq!(status, StatusCode::OK);
    let before = pid_of(&pdf);

    let began = std::time::Instant::now();
    let (status, headers, body) = service.post(b"{\\rtf1 BOMB}").await;
    let took = began.elapsed();
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(headers["access-control-allow-origin"], "*");
    let problem = problem(&body);
    assert_eq!(problem["status"], 413);
    assert_eq!(problem["type"], "about:blank");
    assert_eq!(
        problem["detail"],
        "the document needs more memory to convert than the service allows"
    );
    assert!(took < Duration::from_secs(10), "{took:?}");
    assert!(service.spooled().is_empty(), "{:?}", service.spooled());

    let metrics = metrics_until(&service, "the replacement counted as memory_limit", |m| {
        starts(m, "memory_limit") == 1.0
    })
    .await;
    assert_eq!(conversions(&metrics, "memory_limit"), 1.0, "{metrics}");
    assert_eq!(durations(&metrics, "memory_limit"), 1.0, "{metrics}");
    assert_eq!(starts(&metrics, "crash"), 0.0, "not retried: {metrics}");
    assert_eq!(starts(&metrics, "cancelled"), 0.0, "{metrics}");
    let text = log.text();
    assert!(
        text.contains("the instance passed the hard memory limit during a conversion")
            && text.contains("reason=\"memory_limit\""),
        "{text}"
    );

    let (status, _, pdf) = service.post(b"{\\rtf1 after}").await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        pid_of(&pdf),
        before,
        "a fresh instance serves the next request"
    );
}

/// An instance is recycled after `DOCX_TO_PDF_INSTANCE_MAX_CONVERSIONS`; no request fails across
/// it, one after another or many at once.
#[tokio::test]
#[allow(
    clippy::float_cmp,
    reason = "the values are exact: whole counts and durations set by the test"
)]
async fn recycling_after_n_conversions_fails_no_request() {
    let service = Service::start_with(
        1,
        Duration::from_secs(10),
        InstanceLimits {
            max_conversions: 2,
            ..InstanceLimits::default()
        },
    )
    .await;
    let mut pids = Vec::new();
    for n in 0..5 {
        let (status, _, pdf) = service.post(format!("{{\\rtf1 doc {n}}}").as_bytes()).await;
        assert_eq!(status, StatusCode::OK, "request {n}");
        pids.push(pid_of(&pdf));
    }
    assert_eq!(pids[0], pids[1]);
    assert_ne!(pids[1], pids[2]);
    assert_eq!(pids[2], pids[3]);
    assert_ne!(pids[3], pids[4]);
    metrics_until(&service, "two recycles", |m| starts(m, "recycle") == 2.0).await;

    // Many at once, through recycles on two instances.
    let service = Service::start_with(
        2,
        Duration::from_secs(10),
        InstanceLimits {
            max_conversions: 1,
            ..InstanceLimits::default()
        },
    )
    .await;
    let requests: Vec<_> = (0..12)
        .map(|n| {
            let app = service.app.clone();
            tokio::spawn(async move {
                let request = Service::request(format!("{{\\rtf1 doc {n}}}").as_bytes());
                app.oneshot(request).await.unwrap().status()
            })
        })
        .collect();
    for request in requests {
        assert_eq!(request.await.unwrap(), StatusCode::OK);
    }
    let metrics = metrics_until(&service, "a recycle per conversion", |m| {
        starts(m, "recycle") == 12.0
    })
    .await;
    assert_eq!(conversions(&metrics, "success"), 12.0, "{metrics}");
    assert_eq!(service.spooled(), [] as [std::path::PathBuf; 0]);
}

#[tokio::test]
async fn before_the_pool_has_booted_an_allowed_document_is_503() {
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let app = router(AppState::new(Readiness::new(), &telemetry));
    let response = app
        .oneshot(Service::request(b"{\\rtf1 hello}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().contains_key("retry-after"));
}

/// The most a response frame carries: the streaming buffer.
const FRAME_LIMIT: usize = 64 * 1024;

/// A PDF is answered from its file in the conversion's directory, a bounded frame at a time; the
/// directory stays while the body is under way and is gone once the body has been sent.
#[tokio::test]
async fn a_large_pdf_is_streamed_from_its_file_in_bounded_frames_and_its_directory_goes_after_the_body()
 {
    let service = Service::start(1).await;
    let size = 4 * 1024 * 1024;
    let total = size + b"%PDF-1.4\n".len();
    let response = service
        .app
        .clone()
        .oneshot(Service::request(
            format!("{{\\rtf1 LARGE {size}}}").as_bytes(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/pdf");
    assert_eq!(response.headers()["content-length"], total.to_string());
    let mut body = response.into_body();

    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(first.starts_with(b"%PDF-"), "{:?}", &first[..16]);
    assert!(
        first.len() <= FRAME_LIMIT,
        "a first frame of {}",
        first.len()
    );
    assert_eq!(
        service.spooled().len(),
        1,
        "the PDF is still read from its directory while the body is under way"
    );

    let mut received = first.len();
    let mut frames = 1;
    while let Some(frame) = body.frame().await {
        let data = frame.unwrap().into_data().unwrap();
        assert!(data.len() <= FRAME_LIMIT, "a frame of {}", data.len());
        received += data.len();
        frames += 1;
    }
    assert_eq!(received, total);
    assert!(frames >= total / FRAME_LIMIT, "{frames} frames");
    assert!(
        service.spooled().is_empty(),
        "the directory is gone once the body has been sent: {:?}",
        service.spooled()
    );
    let metrics = service.metrics().await;
    assert_eq!(
        sample(&metrics, "docx_to_pdf_pdf_size_bytes_sum", &[]),
        Some(total.to_f64()),
        "{metrics}"
    );
}

/// The client goes away in the middle of the body; the body is dropped with the connection and the
/// conversion's directory goes with it.
#[tokio::test]
async fn a_client_that_disconnects_mid_body_leaves_no_directory() {
    let service = Service::start(1).await;
    let response = service
        .app
        .clone()
        .oneshot(Service::request(b"{\\rtf1 LARGE 2097152}"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(first.starts_with(b"%PDF-"));
    assert_eq!(service.spooled().len(), 1, "the body is under way");
    drop(body);
    assert!(service.spooled().is_empty(), "{:?}", service.spooled());

    // The same over a real connection: the client closes its socket after the first bytes of the body.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = service.app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let request = Service::request(b"{\\rtf1 LARGE 33554432}");
    let (parts, request_body) = request.into_parts();
    let request_body = request_body.collect().await.unwrap().to_bytes();
    let mut head = format!(
        "POST /pdf HTTP/1.1\r\nhost: {address}\r\ncontent-type: {}\r\ncontent-length: {}\r\n\r\n",
        parts.headers["content-type"].to_str().unwrap(),
        request_body.len()
    )
    .into_bytes();
    head.extend_from_slice(&request_body);
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    stream.write_all(&head).await.unwrap();
    let mut first = vec![0u8; 4096];
    let read = stream.read(&mut first).await.unwrap();
    assert!(
        String::from_utf8_lossy(&first[..read]).starts_with("HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&first[..read])
    );
    assert_eq!(service.spooled().len(), 1, "the body is under way");
    drop(stream);
    for _ in 0..200 {
        if service.spooled().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(service.spooled().is_empty(), "{:?}", service.spooled());
    server.abort();
}

/// An output that is not a PDF is found from its first bytes, before any byte of the body is
/// sent: 500 as before, and its directory is gone.
#[tokio::test]
async fn an_output_that_is_not_a_pdf_is_500_before_any_byte_of_it_is_sent() {
    let service = Service::start(1).await;
    let (status, headers, body) = service.post(b"{\\rtf1 NOTPDF}").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(headers["content-type"], "application/problem+json");
    assert_eq!(problem(&body)["status"], 500);
    assert!(service.spooled().is_empty(), "{:?}", service.spooled());
    let metrics = service.metrics().await;
    assert_eq!(conversions(&metrics, "error"), 1.0, "{metrics}");
}
