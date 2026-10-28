//! The service's `process.*` instruments are read at `/metrics`. They follow the
//! OpenTelemetry semantic conventions in name, kind, unit and attributes:
//!
//! | Instrument | Kind | Unit | Attributes | Prometheus |
//! |---|---|---|---|---|
//! | `process.cpu.time` | counter | `s` | `cpu.mode` = `user`, `system` | `process_cpu_time_seconds_total` |
//! | `process.memory.usage` | up-down counter | `By` | — | `process_memory_usage_bytes` (gauge) |
//! | `process.unix.file_descriptor.count` | up-down counter | `{file_descriptor}` | — | `process_unix_file_descriptor_count` (gauge) |

use axum::body::Body;
use axum::http::{Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

async fn metrics_text() -> String {
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let app = router(AppState::new(Readiness::new(), &telemetry));
    let request = Request::get("/metrics").body(Body::empty()).unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(body.to_vec()).unwrap()
}

/// The value of the one sample of `name` whose labels contain every one of `labels`.
fn sample(text: &str, name: &str, labels: &[&str]) -> f64 {
    let found: Vec<f64> = text
        .lines()
        .filter(|l| {
            l.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
        })
        .filter(|l| labels.iter().all(|label| l.contains(label)))
        .map(|l| l.rsplit(' ').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(found.len(), 1, "one sample of {name} {labels:?} in\n{text}");
    found[0]
}

fn type_of<'a>(text: &'a str, name: &str) -> &'a str {
    let prefix = format!("# TYPE {name} ");
    text.lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("no TYPE line for {name} in\n{text}"))
}

/// Spends CPU in user mode until the kernel reports some, and returns that reading.
///
/// Linux measures a process's total CPU time precisely but splits it between user and system by the
/// mode each timer tick lands in, so a test process that has run for a millisecond, less than one
/// tick, can read 0 user seconds with all of it booked as system. The test makes the
/// precondition true instead of assuming it.
fn spend_user_cpu() -> docx_to_pdf::process_metrics::CpuTime {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut x = 0u64;
    loop {
        let cpu = docx_to_pdf::process_metrics::cpu_time().expect("getrusage on Unix");
        if cpu.user > 0.0 {
            return cpu;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no user CPU after 30 s of spinning: {cpu:?}"
        );
        for i in 0..1_000_000u64 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
        }
    }
}

#[tokio::test]
async fn cpu_time_is_a_counter_in_seconds_per_cpu_mode() {
    let spent = spend_user_cpu();
    let text = metrics_text().await;
    assert_eq!(type_of(&text, "process_cpu_time_seconds_total"), "counter");
    for mode in ["user", "system"] {
        let seconds = sample(
            &text,
            "process_cpu_time_seconds_total",
            &[&format!(r#"cpu_mode="{mode}""#)],
        );
        assert!(seconds >= 0.0, "{mode}: {seconds}");
    }
    let user = sample(
        &text,
        "process_cpu_time_seconds_total",
        &[r#"cpu_mode="user""#],
    );
    assert!(user > 0.0, "a running test process has used some user CPU");
    assert!(
        user >= spent.user,
        "the counter never falls below an earlier reading: {user} < {}",
        spent.user
    );
}

#[tokio::test]
async fn memory_usage_is_the_resident_size_in_bytes() {
    let text = metrics_text().await;
    assert_eq!(type_of(&text, "process_memory_usage_bytes"), "gauge");
    let bytes = sample(&text, "process_memory_usage_bytes", &[]);
    // More than a megabyte and less than a terabyte: a resident size, not pages or kilobytes.
    assert!(bytes > 1e6 && bytes < 1e12, "{bytes}");
}

#[tokio::test]
async fn open_file_descriptors_are_counted() {
    let text = metrics_text().await;
    assert_eq!(
        type_of(&text, "process_unix_file_descriptor_count"),
        "gauge"
    );
    let open = sample(&text, "process_unix_file_descriptor_count", &[]);
    // stdin, stdout and stderr at least.
    assert!(open >= 3.0, "{open}");
}

#[tokio::test]
async fn a_new_file_shows_in_the_descriptor_count() {
    let before = sample(
        &metrics_text().await,
        "process_unix_file_descriptor_count",
        &[],
    );
    let files: Vec<_> = (0..64).map(|_| tempfile::tempfile().unwrap()).collect();
    let during = sample(
        &metrics_text().await,
        "process_unix_file_descriptor_count",
        &[],
    );
    drop(files);
    // Other tests run in parallel threads and open descriptors too; 64 at once still shows.
    assert!(during >= before + 32.0, "before {before}, during {during}");
}
