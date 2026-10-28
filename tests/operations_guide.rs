//! The operations guide, `docs/guide/operations.md`, against the code.
//!
//! - Its metrics table names every instrument the service registers, and only those, under both the
//!   OpenTelemetry name the source gives and the Prometheus name `/metrics` serves. The instruments are read from
//!   `src/` (each `meter.<kind>("name")` builder and its `with_unit`); the Prometheus spelling is not computed by a
//!   rule alone but rendered by the service's own exporter for an instrument of the same kind and unit.
//! - Its Kubernetes example sets only variables the service reads (the rows of `docs/guide/configuration.md`,
//!   which `tests/config_reference.rs` holds to the code), to values the service accepts, and its numbers follow
//!   the formulas the page gives: memory, `/tmp`, CPU, the termination grace. Its probe paths are routes the
//!   service answers.
//! - Every `DOCX_TO_PDF_…` variable the page names anywhere is one the service reads.
//!
//! No LibreOffice needed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::config::Config;
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;
use tower::ServiceExt as _;

const PAGE: &str = "docs/guide/operations.md";
const CONFIGURATION: &str = "docs/guide/configuration.md";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn page() -> String {
    std::fs::read_to_string(root().join(PAGE))
        .unwrap_or_else(|e| panic!("{PAGE}: {e}; the operations guide is missing"))
}

// --- the instruments in the source --------------------------------------------------------------------------------

/// The instrument builders the service uses. A new kind fails [`prometheus_names`] until it is added there.
const KINDS: &[&str] = &[
    "u64_counter",
    "f64_counter",
    "f64_histogram",
    "u64_histogram",
    "u64_gauge",
    "f64_gauge",
    "i64_up_down_counter",
    "u64_observable_gauge",
    "f64_observable_counter",
    "i64_observable_up_down_counter",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Instrument {
    name: String,
    kind: &'static str,
    unit: Option<String>,
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

/// The text of a source file before its inline test module (`#[cfg(test)] mod tests {`). A test module in a file
/// of its own (`mod tests;`) is skipped by [`source_files`] and ends nothing here.
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

/// The string literal at the start of `text` (after optional whitespace), if there is one.
fn literal(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('"')?;
    rest.find('"').map(|end| &rest[..end])
}

/// Every instrument the service builds: `.<kind>("name")`, and the `.with_unit("…")` before its `.build()`.
fn instruments_in_source() -> BTreeSet<Instrument> {
    let mut files = Vec::new();
    source_files(&root().join("src"), &mut files);
    let mut found = BTreeSet::new();
    for file in files {
        let text = non_test_text(&file);
        for &kind in KINDS {
            let call = format!(".{kind}(");
            for (at, _) in text.match_indices(&call) {
                let after = &text[at + call.len()..];
                let Some(name) = literal(after) else {
                    panic!(
                        "{}: an instrument whose name is not a literal; this test cannot read it",
                        file.display()
                    );
                };
                let chain = &after[..after.find(".build()").unwrap_or(after.len())];
                let unit = chain
                    .find(".with_unit(")
                    .and_then(|u| literal(&chain[u + ".with_unit(".len()..]))
                    .map(str::to_owned);
                found.insert(Instrument {
                    name: name.to_owned(),
                    kind,
                    unit,
                });
            }
        }
    }
    found
}

// --- the Prometheus names, as the service's exporter renders them -------------------------------------------------

/// The Prometheus name of each instrument, read from what the service's exporter renders: `process.*` as the
/// service registers it on every start, every other instrument mirrored, same name, kind and unit, on a fresh
/// telemetry with nothing else registered.
#[allow(
    clippy::collection_is_never_read,
    reason = "it keeps the instruments alive until the export"
)]
fn prometheus_names(instruments: &BTreeSet<Instrument>) -> BTreeMap<String, String> {
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let meter = telemetry.meter();
    let mut keep: Vec<Box<dyn std::any::Any>> = Vec::new();
    for i in instruments {
        if i.name.starts_with("process.") {
            continue;
        }
        let unit = i.unit.clone().unwrap_or_default();
        let name = i.name.clone();
        match i.kind {
            "u64_counter" => meter.u64_counter(name).with_unit(unit).build().add(1, &[]),
            "f64_counter" => meter
                .f64_counter(name)
                .with_unit(unit)
                .build()
                .add(1.0, &[]),
            "f64_histogram" => meter
                .f64_histogram(name)
                .with_unit(unit)
                .build()
                .record(1.0, &[]),
            "u64_histogram" => meter
                .u64_histogram(name)
                .with_unit(unit)
                .build()
                .record(1, &[]),
            "u64_gauge" => meter.u64_gauge(name).with_unit(unit).build().record(1, &[]),
            "f64_gauge" => meter
                .f64_gauge(name)
                .with_unit(unit)
                .build()
                .record(1.0, &[]),
            "i64_up_down_counter" => meter
                .i64_up_down_counter(name)
                .with_unit(unit)
                .build()
                .add(1, &[]),
            "u64_observable_gauge" => keep.push(Box::new(
                meter
                    .u64_observable_gauge(name)
                    .with_unit(unit)
                    .with_callback(|o| o.observe(1, &[]))
                    .build(),
            )),
            other => panic!(
                "{}: an instrument of kind {other} outside process.*; mirror it here",
                i.name
            ),
        }
    }
    let text = telemetry.prometheus_text();
    let families: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|l| l.strip_prefix("# TYPE "))
        .filter_map(|l| l.split_once(' '))
        .collect();
    let mut names = BTreeMap::new();
    for i in instruments {
        // The family is found by the name: dots to underscores, then the unit and counter suffixes the exporter adds.
        let stem = i.name.replace('.', "_");
        let candidates: Vec<&str> = families
            .iter()
            .map(|(family, _)| *family)
            .filter(|family| {
                family.strip_prefix(stem.as_str()).is_some_and(|rest| {
                    ["", "_seconds", "_bytes"].iter().any(|unit| {
                        rest.strip_prefix(unit)
                            .is_some_and(|r| r.is_empty() || r == "_total")
                    })
                })
            })
            .collect();
        assert_eq!(
            candidates.len(),
            1,
            "{}: one family in /metrics for it, found {candidates:?} in\n{text}",
            i.name
        );
        names.insert(i.name.clone(), candidates[0].to_owned());
    }
    names
}

// --- the page ------------------------------------------------------------------------------------------------------

/// The cells of a Markdown table row, trimmed; `None` for a line that is not one.
fn cells(line: &str) -> Option<Vec<&str>> {
    let row = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    Some(row.split('|').map(str::trim).collect())
}

/// The one name in backticks a cell holds.
fn code(cell: &str) -> Option<&str> {
    cell.strip_prefix('`')?
        .strip_suffix('`')
        .filter(|s| !s.contains('`') && !s.contains(' '))
}

/// The metrics table: OpenTelemetry name → Prometheus name.
fn metrics_on_the_page(page: &str) -> BTreeMap<String, String> {
    let section = page
        .split("\n### Metrics\n")
        .nth(1)
        .expect("a '### Metrics' section")
        .split("\n## ")
        .next()
        .unwrap();
    section
        .lines()
        .filter_map(cells)
        .filter_map(|row| Some((code(row.first()?)?, code(row.get(1)?)?)))
        .map(|(otel, prom)| (otel.to_owned(), prom.to_owned()))
        .collect()
}

#[test]
fn the_metrics_table_names_every_instrument_as_the_service_serves_it() {
    let instruments = instruments_in_source();
    assert!(
        instruments
            .iter()
            .any(|i| i.name == "docx_to_pdf.conversions")
            && instruments.iter().any(|i| i.name == "process.cpu.time"),
        "reading the source found too little: {instruments:?}"
    );
    let served = prometheus_names(&instruments);
    let on_page = metrics_on_the_page(&page());
    assert_eq!(
        on_page, served,
        "{PAGE}'s metrics table (left) and the service's instruments as /metrics names them (right) disagree"
    );
}

// --- the Kubernetes example -----------------------------------------------------------------------------------------

/// The page's Deployment: the YAML block that holds `kind: Deployment`.
fn manifest(page: &str) -> String {
    page.split("```yaml\n")
        .skip(1)
        .map(|block| block.split("\n```").next().unwrap())
        .find(|block| block.contains("kind: Deployment"))
        .expect("a ```yaml block with a Deployment")
        .to_owned()
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The container's `env:` entries, name → value, in order.
fn env(manifest: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = manifest.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == "env:")
        .expect("an env: list");
    let depth = indent(lines[start]);
    let mut out = Vec::new();
    let mut name = None;
    for line in &lines[start + 1..] {
        if !line.trim().is_empty() && indent(line) <= depth {
            break;
        }
        let item = line.trim().trim_start_matches("- ");
        if let Some(n) = item.strip_prefix("name: ") {
            name = Some(n.trim().to_owned());
        } else if let Some(v) = item.strip_prefix("value: ") {
            let value = v.trim().trim_matches('"').to_owned();
            out.push((name.take().expect("a value after its name"), value));
        } else {
            panic!("an env entry this test cannot read (valueFrom?): {line}");
        }
    }
    out
}

/// The value of the first `key: value` line in `text`.
fn field<'a>(text: &'a str, key: &str) -> &'a str {
    let prefix = format!("{key}: ");
    text.lines()
        .find_map(|l| {
            l.trim()
                .trim_start_matches("- ")
                .strip_prefix(prefix.as_str())
        })
        .unwrap_or_else(|| panic!("no {key}: in\n{text}"))
        .trim()
        .trim_matches('"')
}

/// `4864Mi` → 4864.
fn mebibytes(value: &str) -> u64 {
    value
        .strip_suffix("Mi")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{value}: not a whole number of Mi"))
}

/// The variables the service reads: `docs/guide/configuration.md`'s rows, `<SIGNAL>` expanded.
fn read_by_the_service() -> BTreeSet<String> {
    let page = std::fs::read_to_string(root().join(CONFIGURATION)).unwrap();
    let mut names = BTreeSet::new();
    for row in page.lines().filter_map(cells) {
        let Some(name) = row.first().and_then(|c| code(c)) else {
            continue;
        };
        for signal in ["TRACES", "LOGS", "METRICS"] {
            names.insert(name.replace("<SIGNAL>", signal));
        }
    }
    names
}

#[test]
fn the_kubernetes_example_sets_only_variables_the_service_reads() {
    let read = read_by_the_service();
    let env = env(&manifest(&page()));
    assert_ne!(env, [] as [(String, String); 0]);
    for (name, _) in &env {
        assert!(
            read.contains(name),
            "the example sets {name}, which the service does not read"
        );
    }
}

#[test]
fn the_kubernetes_example_is_a_configuration_the_service_starts_with_and_follows_the_formulas() {
    let manifest = manifest(&page());
    let env: BTreeMap<String, String> = env(&manifest).into_iter().collect();
    let config = Config::from_lookup(|name| env.get(name).cloned())
        .unwrap_or_else(|e| panic!("the example's settings are refused: {e}"));

    const MIB: u64 = 1024 * 1024;
    assert_eq!(
        field(&manifest, "containerPort"),
        config.port.to_string(),
        "the container port is the service's port"
    );
    assert_eq!(
        field(&manifest, "cpu"),
        config.pool_size.to_string(),
        "the CPU request is the pool size"
    );
    let pool = config.pool_size as u64;
    let memory = 256 + pool * (config.instance_hard_memory.div_ceil(MIB) + 256);
    let limits = manifest.split("limits:").nth(1).expect("resources limits");
    assert_eq!(
        mebibytes(field(limits, "memory")),
        memory,
        "the memory limit"
    );
    let requests = manifest
        .split("requests:")
        .nth(1)
        .expect("resources requests");
    assert_eq!(
        mebibytes(field(requests, "memory")),
        memory,
        "the memory request is the limit"
    );
    let tmp = pool * (3 * config.max_document_size).max(256 * MIB).div_ceil(MIB);
    assert_eq!(mebibytes(field(&manifest, "sizeLimit")), tmp, "/tmp's size");

    let pre_stop: u64 = field(manifest.split("preStop:").nth(1).unwrap(), "seconds")
        .parse()
        .unwrap();
    let grace: u64 = field(&manifest, "terminationGracePeriodSeconds")
        .parse()
        .unwrap();
    assert_eq!(
        grace,
        pre_stop + config.shutdown_grace.as_secs() + 40,
        "terminationGracePeriodSeconds is the preStop sleep + the shutdown grace + 40"
    );

    for needed in [
        "readOnlyRootFilesystem: true",
        "runAsNonRoot: true",
        "allowPrivilegeEscalation: false",
        "drop: [ALL]",
        "mountPath: /tmp",
        "enableServiceLinks: false",
    ] {
        assert!(manifest.contains(needed), "the example lacks {needed}");
    }
}

#[tokio::test]
async fn the_kubernetes_example_checks_paths_the_service_answers() {
    let manifest = manifest(&page());
    let paths: BTreeSet<&str> = manifest
        .match_indices("path: ")
        .map(|(at, _)| {
            manifest[at + "path: ".len()..]
                .split([',', '}', '\n'])
                .next()
                .unwrap()
                .trim()
        })
        .collect();
    assert_eq!(
        paths,
        BTreeSet::from(["/livez", "/readyz"]),
        "the checks' paths"
    );

    let telemetry = TelemetryBuilder::new().build().unwrap();
    let app = router(AppState::new(Readiness::new(), &telemetry));
    for path in paths {
        let request = Request::get(path).body(Body::empty()).unwrap();
        let status = app.clone().oneshot(request).await.unwrap().status();
        // `/readyz` is 503 here (no pool); what matters is that the route exists.
        assert!(
            status == StatusCode::OK || status == StatusCode::SERVICE_UNAVAILABLE,
            "GET {path}: {status}"
        );
    }
}

#[test]
fn every_service_variable_the_page_names_is_read() {
    let read = read_by_the_service();
    let page = page();
    let named: BTreeSet<&str> = page
        .match_indices("`DOCX_TO_PDF_")
        .map(|(at, _)| {
            let rest = &page[at + 1..];
            &rest[..rest.find('`').unwrap()]
        })
        .collect();
    assert!(named.len() > 10, "too few found: {named:?}");
    let unread: Vec<_> = named.iter().filter(|n| !read.contains(**n)).collect();
    assert!(
        unread.is_empty(),
        "{PAGE} names variables the service does not read: {unread:?}"
    );
}
