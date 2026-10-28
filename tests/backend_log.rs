//! Text the backend writes or gives reaches the logs only as a field under a
//! neutral message of oxidestack/docx-to-pdf. A line on an instance's stderr is logged as "the backend wrote to
//! stderr" with the line in `backend_output`; the instance's start-failure line is logged as "the backend did not
//! start" with the backend's error text (such as `dlerror()`'s) in `backend_error`. Neither text is ever a message, so the
//! log's messages do not reveal which backend runs.
//!
//! The instance is a stand-in (`sh` writing to stderr), so this runs everywhere.

use std::time::Duration;

use docx_to_pdf::instance::driver::{Instance, Launcher, STDERR_TARGET};
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use opentelemetry::logs::{AnyValue, Severity};
use opentelemetry_sdk::logs::{InMemoryLogExporter, in_memory_exporter::LogDataWithResource};
use tracing_subscriber::layer::SubscriberExt;

/// A line such as the backend writes to stderr while it runs; it names the backend.
const BACKEND_LINE: &str = "warn:vcl.fonts:42:42:LibreOffice could not open a font";

/// The error text a failed start gives, such as `dlerror()`'s; it names the backend's library.
const START_ERROR: &str =
    "/usr/lib/libreoffice/program/libmergedlo.so: cannot open shared object file";

struct Harness {
    logs: InMemoryLogExporter,
    _telemetry: Telemetry,
    dir: tempfile::TempDir,
    _guard: tracing::subscriber::DefaultGuard,
}

fn harness() -> Harness {
    let logs = InMemoryLogExporter::default();
    let telemetry = TelemetryBuilder::new()
        .with_otlp_from(|_| None)
        .with_simple_log_exporter(logs.clone())
        .build()
        .unwrap();
    let guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(telemetry.layers()));
    Harness {
        logs,
        _telemetry: telemetry,
        dir: tempfile::tempdir().unwrap(),
        _guard: guard,
    }
}

impl Harness {
    fn launcher(&self, script: &str) -> Launcher {
        Launcher::with_args("/bin/sh", ["-c", script, "sh"], self.dir.path())
    }

    /// The emitted log records, once one has `key` among its attributes (stderr is read by its own task).
    async fn records_once(&self, key: &str) -> Vec<LogDataWithResource> {
        for _ in 0..200 {
            let logs = self.logs.get_emitted_logs().unwrap();
            if logs.iter().any(|l| attribute(l, key).is_some()) {
                return logs;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "no log record with {key}: {:#?}",
            self.logs.get_emitted_logs().unwrap()
        );
    }
}

fn attribute(log: &LogDataWithResource, key: &str) -> Option<String> {
    log.record
        .attributes_iter()
        .find(|(k, _)| k.as_str() == key)
        .map(|(_, v)| match v {
            AnyValue::String(s) => s.as_str().to_string(),
            other => format!("{other:?}"),
        })
}

fn body(log: &LogDataWithResource) -> String {
    match log.record.body() {
        Some(AnyValue::String(s)) => s.as_str().to_string(),
        other => format!("{other:?}"),
    }
}

/// No record's message contains the backend's text, whichever record it is.
fn no_message_carries(logs: &[LogDataWithResource], text: &str) {
    for log in logs {
        assert!(
            !body(log).contains(text),
            "a message carries {text:?}: {log:#?}"
        );
    }
}

#[tokio::test]
async fn a_backend_stderr_line_is_logged_under_our_message() {
    let harness = harness();
    let script = format!(
        r#"
        printf '%s\n' '{BACKEND_LINE}' >&2
        printf 'ready\t1\t%s\n' "$$"
        read -r _
        "#
    );
    let instance = Instance::start(&harness.launcher(&script)).await.unwrap();
    let logs = harness.records_once("backend_output").await;
    let log = logs
        .iter()
        .find(|l| attribute(l, "backend_output").is_some())
        .unwrap();
    assert_eq!(body(log), "the backend wrote to stderr");
    assert_eq!(
        attribute(log, "backend_output").as_deref(),
        Some(BACKEND_LINE)
    );
    assert_eq!(
        log.record.target().map(std::convert::AsRef::as_ref),
        Some(STDERR_TARGET)
    );
    assert_eq!(log.record.severity_number(), Some(Severity::Info));
    no_message_carries(&logs, BACKEND_LINE);
    drop(instance);
}

#[tokio::test]
async fn a_start_failure_is_logged_under_our_message_with_the_backend_error() {
    let harness = harness();
    let script = format!(
        r"
        printf '%s\n' 'instance: the backend did not start: {START_ERROR}' >&2
        exit 70
        "
    );
    let started = Instance::start(&harness.launcher(&script)).await;
    assert!(started.is_err(), "the stand-in never reports ready");
    let logs = harness.records_once("backend_error").await;
    let log = logs
        .iter()
        .find(|l| attribute(l, "backend_error").is_some())
        .unwrap();
    assert_eq!(body(log), "the backend did not start");
    assert_eq!(
        attribute(log, "backend_error").as_deref(),
        Some(START_ERROR)
    );
    assert_eq!(
        log.record.target().map(std::convert::AsRef::as_ref),
        Some(STDERR_TARGET)
    );
    assert_eq!(log.record.severity_number(), Some(Severity::Error));
    assert!(
        logs.iter()
            .all(|l| attribute(l, "backend_output").is_none()),
        "the start-failure line is not also logged as plain output: {logs:#?}"
    );
    no_message_carries(&logs, START_ERROR);
}
