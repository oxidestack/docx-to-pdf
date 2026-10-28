//! The migration guide, `docs/guide/migration.md`, against the other guide pages and the code.
//!
//! - **Every deliberate difference is one complete row.** Each row of the guide's "Every deliberate difference"
//!   table has a name of its own and fills every column: what moalhaddar/docx-to-pdf does, what this service does,
//!   and why. A reader migrating looks a difference up by its name and needs all three.
//! - **The settings are not duplicated.** The guide has no table row for a setting of the service's own
//!   (`DOCX_TO_PDF_*`, `OTEL_*`, `RUST_LOG`, `TMPDIR`): those are `docs/guide/configuration.md`'s, which it links.
//!   Every variable it names is a row there (held to the code by `tests/config_reference.rs`), and every setting of
//!   moalhaddar/docx-to-pdf it calls "not read" is read nowhere in `src/`.
//! - Every status the client table answers is a status of the service (`200`, or one of `docs/guide/api.md`'s
//!   errors), and every link to a guide page's heading resolves.
//!
//! No LibreOffice needed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const PAGE: &str = "docs/guide/migration.md";
const API: &str = "docs/guide/api.md";
const CONFIGURATION: &str = "docs/guide/configuration.md";

/// The prefixes of the service's own settings: a table row for one of them belongs to the configuration reference.
const OWN_PREFIXES: &[&str] = &["DOCX_TO_PDF_", "OTEL_", "RUST_LOG", "TMPDIR"];

/// The spellings of moalhaddar/docx-to-pdf's settings the service still reads.
const ALIASES: &[&str] = &["SERVER_PORT", "POOL_SIZE", "pool.size"];

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

/// The body rows of the first table after the heading `heading` (the header row and the rule skipped).
fn table<'a>(page: &'a str, heading: &str) -> Vec<Vec<&'a str>> {
    let after = page
        .split(&format!("\n{heading}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("no heading {heading:?}"));
    after
        .lines()
        .skip_while(|l| !l.starts_with('|'))
        .take_while(|l| l.starts_with('|'))
        .skip(2)
        .filter_map(cells)
        .collect()
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

/// The statuses of the API reference's error tables.
fn error_statuses() -> BTreeSet<u16> {
    read(API)
        .lines()
        .filter_map(cells)
        .filter(|row| row.get(1).is_some_and(|c| code(c).is_some()))
        .filter_map(|row| row.first()?.parse().ok())
        .collect()
}

/// `src/**/*.rs` outside the test modules.
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
                w[0].trim() == "#[cfg(test)]"
                    && w[1].trim_start().starts_with("mod ")
                    && w[1].trim_end().ends_with('{')
            })
            .unwrap_or(lines.len());
        all.push_str(&lines[..end].join("\n"));
        all.push('\n');
    }
    all
}

// --- the page ------------------------------------------------------------------------------------------------------

fn page() -> String {
    std::fs::read_to_string(root().join(PAGE))
        .unwrap_or_else(|e| panic!("{PAGE}: {e}; the migration guide is missing"))
}

const DIFFERENCES: &str = "## Every deliberate difference";

/// The columns of the differences table: the difference's name, moalhaddar/docx-to-pdf's behaviour, this service's,
/// and the reason.
const DIFFERENCE_COLUMNS: usize = 4;

fn guide_rows(page: &str) -> BTreeSet<String> {
    table(page, DIFFERENCES)
        .into_iter()
        .map(|row| row[0].to_owned())
        .collect()
}

fn is_own(name: &str) -> bool {
    OWN_PREFIXES.iter().any(|p| name.starts_with(p))
}

#[test]
fn every_deliberate_difference_is_one_complete_row() {
    let page = page();
    let rows = table(&page, DIFFERENCES);
    let mut wrong = Vec::new();
    let mut names = BTreeSet::new();
    for row in &rows {
        let name = row[0];
        if row.len() != DIFFERENCE_COLUMNS {
            wrong.push(format!(
                "{name:?}: {} cells, not {DIFFERENCE_COLUMNS}",
                row.len()
            ));
        } else if let Some(empty) = row.iter().position(|c| c.is_empty()) {
            wrong.push(format!("{name:?}: column {} is empty", empty + 1));
        }
        if !names.insert(name) {
            wrong.push(format!("{name:?}: named twice"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{PAGE}, {DIFFERENCES}:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn the_page_has_no_settings_table_of_its_own_and_names_only_real_settings() {
    let page = page();
    assert!(
        page.contains("](configuration.md)"),
        "{PAGE} links the configuration reference"
    );
    let variables = variables();
    let mut wrong = Vec::new();
    for row in prose(&page).lines().filter_map(cells) {
        if let Some(own) = spans(row[0]).into_iter().find(|s| is_own(s)) {
            wrong.push(format!(
                "a row for `{own}`: the settings table is {CONFIGURATION}'s; link it"
            ));
        }
    }
    for span in spans(&prose(&page)) {
        let name = span.split('=').next().unwrap();
        if (is_own(name) || ALIASES.contains(&name))
            && !name.contains('*')
            && !variables.contains(name)
        {
            wrong.push(format!("`{span}`: no row in {CONFIGURATION}"));
        }
    }
    let source = source();
    for row in table(&page, "### Settings") {
        if !row[1].starts_with("Not read") {
            continue;
        }
        for setting in spans(row[0]) {
            let probe = setting.trim_end_matches('*');
            if source.contains(&format!("\"{probe}")) {
                wrong.push(format!("`{setting}` is called not read, but src/ names it"));
            }
        }
    }
    assert!(wrong.is_empty(), "{PAGE}:\n  {}", wrong.join("\n  "));
}

#[test]
fn every_status_of_the_client_table_is_one_the_service_answers() {
    let page = page();
    let statuses = error_statuses();
    let mut wrong = Vec::new();
    for row in table(&page, "### Statuses") {
        for word in row[2].split(|c: char| !c.is_ascii_digit()) {
            let Ok(status) = word.parse::<u16>() else {
                continue;
            };
            if status >= 100 && status != 200 && !statuses.contains(&status) {
                wrong.push(format!("{status} in {:?}", row[0]));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{PAGE}: statuses {API} does not list: {wrong:#?}"
    );
}

#[test]
fn the_checks_read_what_they_check() {
    // A guard on the readers: an empty reading would let the tests above pass on nothing.
    let rows = guide_rows(&page());
    assert!(rows.len() >= 20, "{rows:#?}");
    assert!(rows.contains("A zip bomb"));
    assert!(variables().contains("DOCX_TO_PDF_MAX_DOCUMENT_SIZE"));
    assert!(variables().contains("pool.size"));
    assert!(error_statuses().contains(&415));
    assert!(!error_statuses().contains(&200));
    let source = source();
    assert!(source.contains("\"SERVER_PORT\""));
    assert!(!source.contains("\"SPRING_"));
    assert_eq!(
        table(&page(), "### Settings").len(),
        prose(&page())
            .split("\n### Settings\n")
            .nth(1)
            .unwrap()
            .split("\n### ")
            .next()
            .unwrap()
            .lines()
            .filter(|l| l.starts_with('|'))
            .count()
            - 2
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
