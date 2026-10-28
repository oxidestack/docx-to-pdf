//! The troubleshooting guide, `docs/guide/troubleshooting.md`, against the other guide pages and the code.
//!
//! - **Every error of the API reference has an entry.** Each error entry is a heading ``### <status> `<detail>` ``; the
//!   set of `(status, detail)` pairs the headings name is exactly the set of the error tables of
//!   `docs/guide/api.md`, which `tests/api_reference.rs` holds to the code. An error added there fails here until it
//!   has an entry, and an entry for an error that no longer exists fails too.
//! - **Every entry names something to look at**: a setting, a metric, a log line or a command to run (a `sh`
//!   block), and each setting, metric and log line is real. A
//!   setting is a row of `docs/guide/configuration.md` (held to the code by `tests/config_reference.rs`); a metric is
//!   a Prometheus name of the metrics table of `docs/guide/operations.md` (held to the code by
//!   `tests/operations_guide.rs`), its labels attributes of that row; a log line, quoted as the JSON field
//!   `"message":"…"` the service writes, is a message the source logs.
//! - Every link to a guide page's heading resolves.
//!
//! No LibreOffice needed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const PAGE: &str = "docs/guide/troubleshooting.md";
const API: &str = "docs/guide/api.md";
const CONFIGURATION: &str = "docs/guide/configuration.md";
const OPERATIONS: &str = "docs/guide/operations.md";

/// The prefixes of the environment variables a page may name; each must be a row of the configuration reference.
const VARIABLE_PREFIXES: &[&str] = &["DOCX_TO_PDF_", "OTEL_", "RUST_LOG", "TMPDIR"];

/// The prefixes of the Prometheus names the service serves.
const METRIC_PREFIXES: &[&str] = &["docx_to_pdf_", "http_server_", "process_"];

/// Labels whose values are open (a status code, a route, a slot), so the metrics table does not list them.
const OPEN_LABELS: &[&str] = &[
    "http_response_status_code",
    "http_route",
    "http_request_method",
    "error_type",
    "format",
    "instance",
];

/// The JSON field a log line's message is in, as the service writes it to standard output.
const LOG_PREFIX: &str = "\"message\":\"";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(page: &str) -> String {
    std::fs::read_to_string(root().join(page)).unwrap_or_else(|e| panic!("{page}: {e}"))
}

// --- reading Markdown ----------------------------------------------------------------------------------------------

/// The page without its fenced blocks.
fn prose(page: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in page.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Every inline code span of `text` (single backticks).
fn spans(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

fn cells(line: &str) -> Option<Vec<&str>> {
    let row = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    Some(row.split('|').map(str::trim).collect())
}

/// The one code span a cell is, if it is exactly that.
fn code(cell: &str) -> Option<&str> {
    cell.strip_prefix('`')?
        .strip_suffix('`')
        .filter(|s| !s.contains('`'))
}

/// GitHub's anchor for a heading: lower case, punctuation dropped, spaces to hyphens.
fn slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// Every anchor a page's headings give, with GitHub's `-1`, `-2` for repeats.
fn anchors(page: &str) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = BTreeSet::new();
    for line in prose(page).lines() {
        let Some(heading) = line.strip_prefix('#').map(|h| h.trim_start_matches('#')) else {
            continue;
        };
        let base = slug(heading.trim());
        let n = seen.entry(base.clone()).or_insert(0);
        out.insert(if *n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        });
        *n += 1;
    }
    out
}

// --- the other pages -----------------------------------------------------------------------------------------------

/// The `(status, detail)` pairs of the API reference's error tables.
fn errors_in_the_api_reference() -> BTreeSet<(u16, String)> {
    read(API)
        .lines()
        .filter_map(cells)
        .filter_map(|row| {
            let status: u16 = row.first()?.parse().ok()?;
            Some((status, code(row.get(1)?)?.to_owned()))
        })
        .collect()
}

/// The variables the configuration reference has a row for, `<SIGNAL>` expanded.
fn variables() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for row in read(CONFIGURATION).lines().filter_map(cells) {
        let Some(name) = row.first().and_then(|c| code(c)) else {
            continue;
        };
        if name.contains("<SIGNAL>") {
            for signal in ["TRACES", "LOGS", "METRICS"] {
                out.insert(name.replace("<SIGNAL>", signal));
            }
        } else {
            out.insert(name.to_owned());
        }
    }
    out
}

/// A row of the operations guide's metrics table.
struct Metric {
    histogram: bool,
    labels: BTreeSet<String>,
    /// The code spans of the row's last cell: the values it lists.
    values: BTreeSet<String>,
}

/// The metrics table of the operations guide, by Prometheus name.
fn metrics() -> BTreeMap<String, Metric> {
    let page = read(OPERATIONS);
    let section = page
        .split("\n### Metrics\n")
        .nth(1)
        .expect("operations.md has a '### Metrics' section")
        .split("\n## ")
        .next()
        .unwrap();
    section
        .lines()
        .filter_map(cells)
        .filter(|row| row.len() == 5)
        .filter_map(|row| {
            let prom = code(row[1])?;
            Some((
                prom.to_owned(),
                Metric {
                    histogram: row[2].starts_with("histogram"),
                    labels: spans(row[3]).iter().map(|a| a.replace('.', "_")).collect(),
                    values: spans(row[4]).into_iter().collect(),
                },
            ))
        })
        .collect()
}

/// `src/**/*.rs` outside the test modules, with each `\`-continued string literal joined onto one line.
fn source() -> String {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && !path
                    .file_stem()
                    .is_some_and(|s| s.to_string_lossy().starts_with("tests"))
            {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root().join("src"), &mut files);
    let mut all = String::new();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let end = lines
            .windows(2)
            .position(|w| {
                // An inline test module; `mod tests;` (a file of its own, skipped above) ends nothing.
                w[0].trim() == "#[cfg(test)]"
                    && w[1].trim_start().starts_with("mod ")
                    && w[1].trim_end().ends_with('{')
            })
            .unwrap_or(lines.len());
        let mut joined = String::new();
        for line in &lines[..end] {
            if let Some(cut) = joined.strip_suffix('\\') {
                joined = cut.to_owned();
                joined.push_str(line.trim_start());
            } else {
                joined.push('\n');
                joined.push_str(line);
            }
        }
        all.push_str(&joined);
    }
    all
}

// --- the page ------------------------------------------------------------------------------------------------------

fn page() -> String {
    std::fs::read_to_string(root().join(PAGE))
        .unwrap_or_else(|e| panic!("{PAGE}: {e}; the troubleshooting guide is missing"))
}

/// The page's `###` sections: the heading and the body up to the next heading.
fn entries(page: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in page.lines() {
        if line.starts_with('#') {
            out.extend(current.take());
            if let Some(heading) = line.strip_prefix("### ") {
                current = Some((heading.to_owned(), String::new()));
            }
        } else if let Some((_, body)) = &mut current {
            body.push_str(line);
            body.push('\n');
        }
    }
    out.extend(current);
    out
}

/// The `(status, detail)` an error entry's heading names: ``### 413 `…` ``.
fn error_heading(heading: &str) -> Option<(u16, String)> {
    let (status, rest) = heading.split_once(' ')?;
    let status: u16 = status.parse().ok()?;
    Some((status, code(rest.trim())?.to_owned()))
}

fn is_variable(span: &str) -> bool {
    VARIABLE_PREFIXES.iter().any(|p| span.starts_with(p))
}

fn is_metric(span: &str) -> bool {
    METRIC_PREFIXES.iter().any(|p| span.starts_with(p))
}

fn is_log(span: &str) -> bool {
    span.starts_with(LOG_PREFIX)
}

/// What is wrong with a metric span, `name` or `name{label="value",…}`, if anything.
fn metric_error(span: &str, metrics: &BTreeMap<String, Metric>) -> Option<String> {
    let (name, labels) = match span.split_once('{') {
        Some((name, rest)) => (name, rest.strip_suffix('}').unwrap_or(rest)),
        None => (span, ""),
    };
    let row = metrics.get(name).or_else(|| {
        ["_count", "_sum", "_bucket"]
            .iter()
            .filter_map(|s| name.strip_suffix(s))
            .find_map(|base| metrics.get(base).filter(|m| m.histogram))
    });
    let Some(row) = row else {
        return Some(format!("{span}: no metric {name} in {OPERATIONS}"));
    };
    for pair in labels.split(',').filter(|p| !p.is_empty()) {
        let Some((label, value)) = pair.split_once('=') else {
            return Some(format!("{span}: {pair:?} is not label=\"value\""));
        };
        let value = value.trim_matches('"');
        if label == "le" && name.ends_with("_bucket") {
            continue;
        }
        if !row.labels.contains(label) {
            return Some(format!("{span}: {name} has no label {label}"));
        }
        if !OPEN_LABELS.contains(&label) && !row.values.contains(value) {
            return Some(format!(
                "{span}: {OPERATIONS} lists no value {value:?} for {name}"
            ));
        }
    }
    None
}

#[test]
fn every_error_of_the_api_reference_has_an_entry_and_no_other() {
    let api = errors_in_the_api_reference();
    assert!(
        api.len() > 20,
        "{API}: the error tables were not read: {api:?}"
    );
    let page = page();
    let on_page: BTreeSet<_> = entries(&page)
        .iter()
        .filter_map(|(h, _)| error_heading(h))
        .collect();
    let missing: Vec<_> = api.difference(&on_page).collect();
    let unknown: Vec<_> = on_page.difference(&api).collect();
    assert!(
        missing.is_empty() && unknown.is_empty(),
        "{PAGE} and {API} disagree.\n  errors with no entry: {missing:#?}\n  entries for no error: {unknown:#?}"
    );
}

#[test]
fn every_entry_names_a_setting_a_metric_a_log_line_or_a_command() {
    let page = page();
    let bare: Vec<_> = entries(&page)
        .into_iter()
        .filter(|(_, body)| {
            !body.contains("\n```sh\n")
                && !spans(&prose(body))
                    .iter()
                    .any(|s| is_variable(s) || is_metric(s) || is_log(s))
        })
        .map(|(h, _)| h)
        .collect();
    assert!(
        bare.is_empty(),
        "{PAGE}: entries that name no setting, metric, log line or command: {bare:#?}"
    );
}

#[test]
fn every_setting_metric_and_log_line_the_page_names_exists() {
    let text = prose(&page());
    let variables = variables();
    let metrics = metrics();
    let source = source();
    let mut wrong = Vec::new();
    for span in spans(&text) {
        if is_variable(&span) {
            let name = span.split('=').next().unwrap();
            if !variables.contains(name) {
                wrong.push(format!("{span}: no row in {CONFIGURATION}"));
            }
        } else if is_metric(&span) {
            wrong.extend(metric_error(&span, &metrics));
        } else if let Some(message) = span.strip_prefix(LOG_PREFIX) {
            let message = message.strip_suffix('"').unwrap_or(message);
            if !source.contains(&format!("\"{message}\"")) {
                wrong.push(format!("{span}: no such log message in src/"));
            }
        }
    }
    assert!(wrong.is_empty(), "{PAGE}:\n  {}", wrong.join("\n  "));
}

#[test]
fn the_checks_read_what_they_check() {
    // A guard on the readers: an empty reading would let the tests above pass on nothing.
    assert!(variables().contains("DOCX_TO_PDF_INSTANCE_HARD_MEMORY"));
    assert!(variables().contains("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"));
    let metrics = metrics();
    assert!(
        metric_error(
            "docx_to_pdf_conversions_total{outcome=\"memory_limit\"}",
            &metrics
        )
        .is_none()
    );
    assert!(
        metric_error(
            "docx_to_pdf_conversions_total{outcome=\"nonsense\"}",
            &metrics
        )
        .is_some()
    );
    assert!(metric_error("docx_to_pdf_pool_wait_duration_seconds_count", &metrics).is_none());
    assert!(metric_error("docx_to_pdf_pool_size_count", &metrics).is_some());
    let source = source();
    // A message continued over two lines with `\`.
    assert!(source.contains(
        "\"the instance passed the hard memory limit during a conversion; killing it, the request fails\""
    ));
    assert_eq!(
        error_heading("413 `the document or the request is over the size limit`"),
        Some((
            413,
            "the document or the request is over the size limit".to_owned()
        ))
    );
}

#[test]
fn every_link_to_a_heading_resolves() {
    let page = page();
    let mut broken = Vec::new();
    for (at, _) in page.match_indices("](") {
        let target = &page[at + 2..];
        let target = &target[..target.find(')').unwrap()];
        if target.starts_with("http") {
            continue;
        }
        let (file, anchor) = target.split_once('#').unwrap_or((target, ""));
        let text = if file.is_empty() {
            page.clone()
        } else {
            match std::fs::read_to_string(root().join("docs/guide").join(file)) {
                Ok(text) => text,
                Err(e) => {
                    broken.push(format!("{target}: {e}"));
                    continue;
                }
            }
        };
        if !anchor.is_empty() && !anchors(&text).contains(anchor) {
            broken.push(format!("{target}: no such heading"));
        }
    }
    assert!(
        broken.is_empty(),
        "{PAGE}: broken links:\n  {}",
        broken.join("\n  ")
    );
}
