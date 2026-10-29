//! The comparison page, `docs/guide/comparison.md`, against the code and the tools that measure.
//!
//! - **The page gives the command that repeats its figures**: `make compare`, which the Makefile has, the same
//!   command its generated block says measured them.
//! - **The page says what was measured**: it carries the block `make evidence` writes from the committed results
//!   (`site/public/benchmarks/results.json`; the site's build holds the block to them), and that block names the
//!   run's date.
//! - **The page says what the figures leave open**: one run a side for a hostile document, the code before the heap
//!   trim, and that Gotenberg's rendering has never been measured.
//! - **The Gotenberg it describes is the one measured**: the version `tools/compare/compare.py` runs.
//! - The paths it names for oxidestack/docx-to-pdf are paths of `docs/guide/api.md`, and every relative link
//!   resolves.
//!
//! No LibreOffice needed.

use std::collections::BTreeSet;
use std::path::PathBuf;

const PAGE: &str = "docs/guide/comparison.md";
const API: &str = "docs/guide/api.md";
const COMPARE: &str = "tools/compare/compare.py";
const RESULTS: &str = "site/public/benchmarks/results.json";
const BEGIN: &str = "<!-- benchmarks:begin: `make evidence` writes this block from site/public/benchmarks/results.json -->";
const END: &str = "<!-- benchmarks:end -->";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The fenced blocks of the page, joined.
fn fences(page: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in page.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Every Markdown link `[text](target)` as (text, target).
fn links(page: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = page;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find("](") else {
            break;
        };
        let text = rest[..close].to_owned();
        let target_start = close + 2;
        let Some(end) = rest[target_start..].find(')') else {
            break;
        };
        out.push((text, rest[target_start..target_start + end].to_owned()));
        rest = &rest[target_start + end..];
    }
    out
}

/// The numbers of `text`: runs of digits joined by `.` (`0.998`, `8.38.1`), with a thousands group written after a
/// space (`1 063`) read as one number (`1063`).
fn numbers(text: &str) -> BTreeSet<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit()
            || (i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'))
        {
            i += 1;
            continue;
        }
        let mut token = String::new();
        loop {
            while i < chars.len() && chars[i].is_ascii_digit() {
                token.push(chars[i]);
                i += 1;
            }
            let digit_at = |j: usize| j < chars.len() && chars[j].is_ascii_digit();
            if i + 1 < chars.len() && chars[i] == '.' && digit_at(i + 1) {
                token.push('.');
                i += 1;
                continue;
            }
            // `1 063`: a space, exactly three digits, then no digit.
            if i + 4 <= chars.len()
                && chars[i] == ' '
                && digit_at(i + 1)
                && digit_at(i + 2)
                && digit_at(i + 3)
                && !digit_at(i + 4)
                && !token.contains('.')
            {
                i += 1;
                continue;
            }
            break;
        }
        // A word that starts with digits (`6248R`) is read as its digits; the letters after them are skipped.
        while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        out.insert(token);
    }
    out
}

/// The page's text with its lines joined, so a phrase the generator wrapped still reads as one.
fn flowed(page: &str) -> String {
    page.replace('\n', " ")
}

#[test]
fn the_page_gives_the_command_that_repeats_the_numbers() {
    let page = read(PAGE);
    assert!(
        fences(&page).lines().any(|l| l.starts_with("make compare")),
        "{PAGE}: no command block runs `make compare`"
    );
    assert!(
        read("Makefile").lines().any(|l| l.starts_with("compare:")),
        "the Makefile has no target compare:"
    );
}

#[test]
fn the_page_says_what_was_measured() {
    let page = read(PAGE);
    let (Some(start), Some(end)) = (page.find(BEGIN), page.find(END)) else {
        panic!("{PAGE}: no block generated from {RESULTS}");
    };
    let block = flowed(&page[start..end]);
    let results: serde_json::Value =
        serde_json::from_str(&read(RESULTS)).unwrap_or_else(|e| panic!("{RESULTS}: {e}"));
    let stamp = results["meta"]["stamp"]
        .as_str()
        .expect("results.json has no meta.stamp");
    let date = format!("{}-{}-{}", &stamp[0..4], &stamp[4..6], &stamp[6..8]);
    for phrase in [
        "Measured by `make compare` on ",
        date.as_str(),
        "## Speed and memory",
        "## Hostile documents",
    ] {
        assert!(
            block.contains(phrase),
            "{PAGE}: the generated block does not say \"{phrase}\""
        );
    }
}

#[test]
fn the_page_says_what_the_figures_leave_open() {
    let page = flowed(&read(PAGE));
    assert!(
        !page.contains("has not yet been run"),
        "{PAGE}: still says the full mode has not been run"
    );
    for phrase in [
        "one run a side",
        "before the heap trim",
        "Gotenberg's rendering has never been measured",
    ] {
        assert!(page.contains(phrase), "{PAGE}: does not say \"{phrase}\"");
    }
}

#[test]
fn the_gotenberg_described_is_the_one_measured() {
    let compare = read(COMPARE);
    let at = compare
        .find("\"Gotenberg ")
        .expect("compare.py names no Gotenberg version");
    let rest = &compare[at + 1..];
    let named = &rest[..rest.find('"').unwrap()];
    let page = read(PAGE);
    assert!(
        flowed(&page).contains(named),
        "{PAGE}: does not describe {named}, the version {COMPARE} runs"
    );
    assert!(
        !numbers(&page).iter().any(|n| n.starts_with("8.")
            && n.matches('.').count() == 2
            && !named.ends_with(n.as_str())),
        "{PAGE}: names a Gotenberg version other than {named}"
    );
}

#[test]
fn the_paths_named_for_oxidestack_are_the_services() {
    let page = read(PAGE);
    let api = read(API);
    let spans: Vec<&str> = page.split('`').skip(1).step_by(2).collect();
    let ours: Vec<&str> = spans
        .iter()
        .copied()
        .filter(|s| s.starts_with("POST /") || s.starts_with("GET /"))
        .filter(|s| !s.contains("/forms/") && !s.contains("/health") && !s.contains("/prometheus/"))
        .collect();
    assert!(
        !ours.is_empty(),
        "{PAGE}: names none of the service's paths"
    );
    for span in ours {
        assert!(
            api.contains(&format!("| `{span}` |")),
            "{PAGE}: `{span}` is not a path of {API}"
        );
    }
}

#[test]
fn every_relative_link_resolves() {
    let page = read(PAGE);
    let broken: Vec<String> = links(&page)
        .into_iter()
        .map(|(_, t)| t)
        .filter(|t| !t.contains("://") && !t.starts_with('#'))
        .filter(|t| {
            !root()
                .join("docs/guide")
                .join(t.split('#').next().unwrap())
                .exists()
        })
        .collect();
    assert!(broken.is_empty(), "{PAGE}: broken links {broken:?}");
}

#[test]
fn the_number_reader_reads_figures_as_the_page_writes_them() {
    let read =
        numbers("0.998 [0.957-1.043], 1 063 MiB, 8.38.1, 7.9 GiB, 1.88×, 6248R, x10, 3 clients");
    let expected: BTreeSet<String> = [
        "0.998", "0.957", "1.043", "1063", "8.38.1", "7.9", "1.88", "6248", "3",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(read, expected);
}
