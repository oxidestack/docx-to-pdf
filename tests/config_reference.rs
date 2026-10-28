//! The configuration reference, `docs/guide/configuration.md`, against the code: every environment variable the
//! service reads has a row there, and every row names a variable the service reads, so
//! an operator finds every setting there and none that does nothing.
//!
//! What the code reads is gathered three ways, so a setting added by any of them fails this test until it has a row:
//! - live: every name `Config::from_lookup` and `TelemetryBuilder::with_otlp_from` ask their lookup for;
//! - from the source: every `"DOCX_TO_PDF_…"` and `"OTEL_…"` string literal, and every literal name passed to
//!   `env::var`/`env::var_os`, in `src/` outside its test modules;
//! - declared: the variables a library reads on the service's behalf ([`READ_BY_LIBRARIES`]).
//!
//! The page's rows are the first cells of its tables that hold one name in backticks. A name may carry
//! `<SIGNAL>`, which stands for `TRACES`, `LOGS` and `METRICS`.
//!
//! No LibreOffice needed.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use docx_to_pdf::config::Config;
use docx_to_pdf::telemetry::TelemetryBuilder;

const PAGE: &str = "docs/guide/configuration.md";

/// Variables read by a library the service calls, not by a lookup in `src/`; each with the code that reads it.
const READ_BY_LIBRARIES: &[(&str, &str)] = &[
    // tracing-subscriber's `EnvFilter::try_from_default_env` (src/telemetry.rs, `install_subscriber`).
    ("RUST_LOG", "tracing-subscriber"),
    // `std::env::temp_dir()`: instance profiles, the upload spool, picture scratch files.
    ("TMPDIR", "std"),
    // opentelemetry_sdk's resource detectors (src/telemetry.rs, `service_resource`).
    ("OTEL_SERVICE_NAME", "opentelemetry_sdk"),
    ("OTEL_RESOURCE_ATTRIBUTES", "opentelemetry_sdk"),
    // opentelemetry-otlp's exporter builders, for the signals the service turns on.
    ("OTEL_EXPORTER_OTLP_HEADERS", "opentelemetry-otlp"),
    ("OTEL_EXPORTER_OTLP_TRACES_HEADERS", "opentelemetry-otlp"),
    ("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "opentelemetry-otlp"),
    ("OTEL_EXPORTER_OTLP_METRICS_HEADERS", "opentelemetry-otlp"),
    ("OTEL_EXPORTER_OTLP_TIMEOUT", "opentelemetry-otlp"),
    ("OTEL_EXPORTER_OTLP_TRACES_TIMEOUT", "opentelemetry-otlp"),
    ("OTEL_EXPORTER_OTLP_LOGS_TIMEOUT", "opentelemetry-otlp"),
    ("OTEL_EXPORTER_OTLP_METRICS_TIMEOUT", "opentelemetry-otlp"),
];

const SIGNALS: [&str; 3] = ["TRACES", "LOGS", "METRICS"];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every name the configuration and the telemetry set-up ask their lookup for.
fn read_live() -> BTreeSet<String> {
    let asked = RefCell::new(BTreeSet::new());
    let record = |name: &str| {
        asked.borrow_mut().insert(name.to_owned());
    };

    // Nothing set: every variable falls through to the next spelling and then to its default.
    Config::from_lookup(|name| {
        record(name);
        None
    })
    .expect("the defaults are a valid configuration");

    // Nothing set, then a general endpoint: the second asks for the per-signal protocols the first does not reach.
    for endpoint in [None, Some("http://127.0.0.1:9")] {
        let _ = TelemetryBuilder::new().with_otlp_from(|name| {
            record(name);
            (name == "OTEL_EXPORTER_OTLP_ENDPOINT")
                .then_some(endpoint)
                .flatten()
                .map(str::to_owned)
        });
    }
    asked.into_inner()
}

/// `src/**/*.rs`, without the files that are test modules.
fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            source_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && !path
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().starts_with("tests"))
        {
            out.push(path);
        }
    }
}

/// The text of a source file before its inline test module (`#[cfg(test)] mod tests {`); an out-of-line
/// `mod tests;` is only a declaration, and the file goes on after it.
fn non_test_text(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .windows(2)
        .position(|w| {
            w[0].trim() == "#[cfg(test)]"
                && w[1].trim_start().starts_with("mod ")
                && w[1].trim_end().ends_with('{')
        })
        .unwrap_or(lines.len());
    lines[..end].join("\n")
}

/// The name in `text` at `start` up to a closing `"`, if it is one whole name.
fn quoted_name(text: &str, start: usize) -> Option<String> {
    let rest = &text[start..];
    let len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(rest.len());
    (len > 0 && rest[len..].starts_with('"')).then(|| rest[..len].to_owned())
}

/// The variable names the source spells out.
fn read_in_source() -> BTreeSet<String> {
    let mut files = Vec::new();
    source_files(&root().join("src"), &mut files);
    let mut names = BTreeSet::new();
    for file in files {
        let text = non_test_text(&file);
        for prefix in ["\"DOCX_TO_PDF_", "\"OTEL_"] {
            for (at, _) in text.match_indices(prefix) {
                if let Some(name) = quoted_name(&text, at + 1) {
                    names.insert(name);
                }
            }
        }
        for call in ["env::var(\"", "env::var_os(\""] {
            for (at, _) in text.match_indices(call) {
                if let Some(name) = quoted_name(&text, at + call.len()) {
                    names.insert(name);
                }
            }
        }
    }
    names
}

/// The names the page's tables give a row, `<SIGNAL>` expanded.
fn on_the_page() -> BTreeSet<String> {
    let page = std::fs::read_to_string(root().join(PAGE))
        .unwrap_or_else(|e| panic!("{PAGE}: {e}; the configuration reference is missing"));
    let mut names = BTreeSet::new();
    for line in page.lines() {
        let Some(row) = line.trim().strip_prefix('|') else {
            continue;
        };
        let first = row.split('|').next().unwrap_or("").trim();
        let Some(name) = first
            .strip_prefix('`')
            .and_then(|s| s.strip_suffix('`'))
            .filter(|s| !s.contains('`') && !s.contains(' '))
        else {
            continue;
        };
        if name.contains("<SIGNAL>") {
            for signal in SIGNALS {
                names.insert(name.replace("<SIGNAL>", signal));
            }
        } else {
            names.insert(name.to_owned());
        }
    }
    names
}

#[test]
fn every_variable_the_service_reads_has_a_row_and_every_row_is_read() {
    let mut read = read_live();
    read.extend(read_in_source());
    read.extend(READ_BY_LIBRARIES.iter().map(|(name, _)| (*name).to_owned()));
    let page = on_the_page();

    let missing: Vec<_> = read.difference(&page).collect();
    let unread: Vec<_> = page.difference(&read).collect();
    assert!(
        missing.is_empty() && unread.is_empty(),
        "{PAGE} and the code disagree.\n  read by the service, no row on the page: {missing:?}\n  \
         a row on the page, not read by the service: {unread:?}"
    );
}

#[test]
fn the_live_reading_sees_every_spelling() {
    // A guard on the method: if the recording stopped reaching a lookup, the comparison above would pass on less.
    let live = read_live();
    for name in [
        "DOCX_TO_PDF_PORT",
        "SERVER_PORT",
        "POOL_SIZE",
        "pool.size",
        "DOCX_TO_PDF_PROBE_PORT",
        "OTEL_SDK_DISABLED",
        "OTEL_METRICS_EXPORTER",
        "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
    ] {
        assert!(live.contains(name), "{name} was not asked for: {live:?}");
    }
}
