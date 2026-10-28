//! The corpus (`tests/corpus/README.md`): every document of `tests/corpus/manifest.tsv` converted by
//! oxidestack/docx-to-pdf and checked against moalhaddar/docx-to-pdf and against its own approved rendering.
//!
//! - Against moalhaddar/docx-to-pdf's PDF (`tests/corpus/oracle/`): the page count, the page sizes and the document's words gate;
//!   a difference passes only as a row of `tests/corpus/divergences.tsv`, a known difference with its reason. The
//!   rendered pages are compared too, and the largest differing share is reported, not gated: LibreOffice 26.8 lays
//!   out many documents differently from 7.3 in ways only a person can rate.
//! - Against the rendering baseline (`tests/rendering-baseline/`, the PDFs the user approved): PDF equivalence at
//!   its default thresholds gates. A document without a baseline PDF is reported until the user approves it.
//!
//! Modes, by environment:
//! - `DOCX_TO_PDF_RECORD_ORACLE=http://localhost:48080` (`make corpus-record`): moalhaddar/docx-to-pdf's PDFs become
//!   the goldens; `DOCX_TO_PDF_RECORD_ONLY=a,b` limits it to some documents.
//! - `DOCX_TO_PDF_CORPUS_TARGET=http://127.0.0.1:<port>` (`make image-corpus`): the run against that service, which
//!   must be the image, whose fonts and LibreOffice the baseline was made with. The report goes to `target/corpus/report.md`, the PDFs to
//!   `target/corpus/ours/`.
//! - Neither: nothing to run against, the test says so and passes.

#[path = "oracle/http.rs"]
#[allow(dead_code)]
mod http;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use docx_to_pdf::pdf_equivalence::{
    Finding, Report, Thresholds, compare, compare_files, document_words_difference, poppler,
};
use http::{Part, Request, Target};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn corpus_dir() -> PathBuf {
    root().join("tests/corpus")
}

fn baseline_dir() -> PathBuf {
    root().join("tests/rendering-baseline")
}

/// One row of `tests/corpus/manifest.tsv`.
struct Document {
    name: String,
    /// The input, under `tests/corpus/documents/`.
    path: String,
    /// Where the input comes from: `generated` (the bench's seeded generator), `docx2pdf` (the project author's
    /// fixtures), or a third party, whose documents sit under `documents/third-party/<origin>/` beside its licence.
    origin: String,
    feature: String,
    /// The approved PDF under `tests/rendering-baseline/`, if the user approved one.
    baseline: Option<String>,
}

impl Document {
    fn input(&self) -> PathBuf {
        corpus_dir().join("documents").join(&self.path)
    }

    fn golden(&self) -> PathBuf {
        corpus_dir()
            .join("oracle")
            .join(format!("{}.pdf", self.name))
    }

    /// The divergence that says moalhaddar/docx-to-pdf does not convert this document, if one does.
    fn unconverted<'a>(&self, known: &'a BTreeMap<(String, String), String>) -> Option<&'a String> {
        known.get(&(self.name.clone(), "converts".to_owned()))
    }
}

/// The rows of a TSV file with a header line, as maps from column name to value.
fn tsv(path: &Path) -> Vec<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Vec<&str> = lines.next().expect("a header line").split('\t').collect();
    lines
        .map(|l| {
            header
                .iter()
                .zip(l.split('\t').chain(std::iter::repeat("")))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .collect()
}

fn manifest() -> Vec<Document> {
    tsv(&corpus_dir().join("manifest.tsv"))
        .into_iter()
        .map(|mut row| Document {
            name: row.remove("name").unwrap(),
            path: row.remove("document").unwrap(),
            origin: row.remove("origin").unwrap_or_default(),
            feature: row.remove("feature").unwrap_or_default(),
            baseline: row.remove("baseline").filter(|b| !b.is_empty()),
        })
        .collect()
}

/// The checks against moalhaddar/docx-to-pdf that gate, by their name in `divergences.tsv`.
fn gate_check(finding: &Finding) -> Option<&'static str> {
    match finding {
        Finding::PageCount { .. } => Some("pages"),
        Finding::PageSize { .. } => Some("sizes"),
        Finding::Text { .. } | Finding::Words { .. } => Some("text"),
        Finding::Rendering { .. } => None,
    }
}

/// `(document, check)` → the open divergence that accounts for it. The check `converts` says moalhaddar/docx-to-pdf
/// does not convert the document at all: it has no golden and is checked against the baseline only.
fn divergences() -> BTreeMap<(String, String), String> {
    tsv(&corpus_dir().join("divergences.tsv"))
        .into_iter()
        .map(|mut row| {
            (
                (row.remove("name").unwrap(), row.remove("check").unwrap()),
                row.remove("divergence").unwrap_or_default(),
            )
        })
        .collect()
}

/// Converts `input` on `target` through `/pdf`, as a client does.
fn convert(target: &Target, input: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(input).map_err(|e| format!("{}: {e}", input.display()))?;
    let filename = input.file_name().unwrap().to_string_lossy().into_owned();
    let request = Request {
        method: "POST".into(),
        path: "/pdf".into(),
        headers: vec![(
            "Content-Type".into(),
            format!("multipart/form-data; boundary={}", http::BOUNDARY),
        )],
        body: Some(http::multipart(&[Part::File {
            name: "document".into(),
            filename,
            content_type: "application/octet-stream".into(),
            bytes,
        }])),
        chunked: false,
        expect_continue: false,
    };
    let answer = http::send(target, &request).map_err(|e| e.to_string())?;
    if answer.status != 200 {
        return Err(format!(
            "{}: {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        ));
    }
    Ok(answer.body)
}

fn only() -> Option<Vec<String>> {
    std::env::var("DOCX_TO_PDF_RECORD_ONLY")
        .ok()
        .map(|v| v.split(',').map(|s| s.trim().to_owned()).collect())
}

fn record(oracle: &str) {
    let target = Target::from_url(oracle, Duration::from_secs(1200));
    let only = only();
    let known = divergences();
    let mut failed = Vec::new();
    for doc in manifest() {
        if only.as_ref().is_some_and(|o| !o.contains(&doc.name))
            || doc.unconverted(&known).is_some()
        {
            continue;
        }
        let started = Instant::now();
        match convert(&target, &doc.input()) {
            Ok(pdf) => {
                std::fs::write(doc.golden(), &pdf).unwrap();
                println!(
                    "{}: {} bytes in {:.1}s",
                    doc.name,
                    pdf.len(),
                    started.elapsed().as_secs_f64()
                );
            }
            Err(e) => {
                println!("{}: {e}", doc.name);
                failed.push(format!("{}: {e}", doc.name));
            }
        }
    }
    assert!(
        failed.is_empty(),
        "moalhaddar/docx-to-pdf did not convert:\n{}",
        failed.join("\n")
    );
}

/// What one document's run found.
struct Outcome {
    doc: Document,
    pages: Option<usize>,
    /// Against moalhaddar/docx-to-pdf: the gating findings, each with the divergence that accounts for it, if any.
    oracle_gate: Vec<(Finding, Option<String>)>,
    /// Against moalhaddar/docx-to-pdf: the largest differing share of a page's inked pixels and its page.
    oracle_rendering: Option<(f64, usize)>,
    oracle_pages: Option<usize>,
    /// The divergence by which moalhaddar/docx-to-pdf has no PDF of the document to compare with.
    unconverted: Option<String>,
    /// Against the baseline: `None` when there is none, else its report.
    baseline: Option<Report>,
    /// Why the document could not be checked at all.
    error: Option<String>,
}

impl Outcome {
    fn failures(&self) -> Vec<String> {
        let mut f = Vec::new();
        if let Some(e) = &self.error {
            f.push(e.clone());
        }
        for (finding, divergence) in &self.oracle_gate {
            if divergence.is_none() {
                f.push(format!("against moalhaddar/docx-to-pdf: {finding:?}"));
            }
        }
        if let Some(report) = self.baseline.as_ref().filter(|r| !r.is_equivalent()) {
            for finding in &report.findings {
                f.push(format!("against the rendering baseline: {finding:?}"));
            }
        }
        f
    }
}

fn check(
    target: &Target,
    doc: Document,
    out: &Path,
    known: &BTreeMap<(String, String), String>,
) -> Outcome {
    let mut outcome = Outcome {
        doc,
        pages: None,
        oracle_gate: Vec::new(),
        oracle_rendering: None,
        oracle_pages: None,
        unconverted: None,
        baseline: None,
        error: None,
    };
    let ours = out.join(format!("{}.pdf", outcome.doc.name));
    match convert(target, &outcome.doc.input()) {
        Ok(pdf) => std::fs::write(&ours, pdf).unwrap(),
        Err(e) => {
            outcome.error = Some(format!("oxidestack/docx-to-pdf did not convert: {e}"));
            return outcome;
        }
    }
    // Every page with any differing pixel becomes a finding, so the largest share is known even below the
    // threshold.
    let every_pixel = Thresholds {
        max_differing_fraction: 0.0,
        ..Thresholds::default()
    };
    outcome.unconverted = outcome.doc.unconverted(known).cloned();
    let oracle = match outcome.unconverted {
        Some(_) => None,
        None => Some(against_oracle(&outcome.doc.golden(), &ours, &every_pixel)),
    };
    match oracle {
        None => {}
        Some(Ok(report)) => {
            outcome.oracle_pages = Some(report.pages);
            outcome.pages = Some(report.pages);
            for finding in report.findings {
                if let Finding::PageCount { actual, .. } = finding {
                    outcome.pages = Some(actual);
                }
                match (&finding, gate_check(&finding)) {
                    (Finding::Rendering { page, fraction, .. }, None) => {
                        if outcome.oracle_rendering.is_none_or(|(f, _)| *fraction > f) {
                            outcome.oracle_rendering = Some((*fraction, *page));
                        }
                    }
                    (_, Some(check)) => {
                        let divergence = known
                            .get(&(outcome.doc.name.clone(), check.to_owned()))
                            .cloned();
                        outcome.oracle_gate.push((finding, divergence));
                    }
                    (_, None) => {}
                }
            }
        }
        Some(Err(e)) => outcome.error = Some(format!("against moalhaddar/docx-to-pdf: {e}")),
    }
    if let Some(baseline) = &outcome.doc.baseline {
        match compare_files(
            &baseline_dir().join(baseline),
            &ours,
            &Thresholds::default(),
        ) {
            Ok(report) => outcome.baseline = Some(report),
            Err(e) => outcome.error = Some(format!("against the rendering baseline: {e}")),
        }
    }
    outcome
}

/// [`compare`] with moalhaddar/docx-to-pdf's PDF, the text compared as the whole document's words rather than page by
/// page: LibreOffice 26.8 fits a line more or less on a page than 7.3 and may extract a floating table or a footnote
/// elsewhere in the flow, so per page the text differs wherever a break or a float moved, and the page
/// count already says whether a break did.
fn against_oracle(golden: &Path, ours: &Path, t: &Thresholds) -> Result<Report, poppler::Error> {
    let (e, a) = (poppler::read(golden, t.dpi)?, poppler::read(ours, t.dpi)?);
    let mut report = compare(&e, &a, t);
    report
        .findings
        .retain(|f| !matches!(f, Finding::Text { .. }));
    report.findings.extend(document_words_difference(&e, &a));
    Ok(report)
}

fn count(n: Option<usize>) -> String {
    n.map_or_else(|| "–".to_owned(), |n| n.to_string())
}

fn report(outcomes: &[Outcome]) -> String {
    let mut s = String::from(
        "# Corpus report\n\n\
         Against moalhaddar/docx-to-pdf the page count, the page sizes and the text gate (a difference \
         passes as a known divergence); its rendering is reported, not gated. The rendering baseline gates at the \
         default thresholds.\n\n\
         | Document | Feature | Pages (ours / moalhaddar) | Pages, sizes, text vs moalhaddar | Rendering vs moalhaddar \
         (largest share, page) | Rendering baseline |\n|---|---|---|---|---|---|\n",
    );
    for o in outcomes {
        let gate = if let Some(e) = &o.error {
            format!("error: {}", e.replace('|', "\\|").replace('\n', " "))
        } else if let Some(d) = &o.unconverted {
            format!("moalhaddar does not convert it: open divergence ({d})")
        } else if o.oracle_gate.is_empty() {
            "equal".to_owned()
        } else {
            o.oracle_gate
                .iter()
                .map(|(f, d)| {
                    let check = gate_check(f).unwrap_or("?");
                    match d {
                        Some(d) => format!("{check}: open divergence ({d})"),
                        None => format!("{check}: FAILS"),
                    }
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        let rendering = match o.oracle_rendering {
            Some((f, p)) => format!("{:.2}% p{p}", f * 100.0),
            None if o.oracle_pages.is_some() => "0%".to_owned(),
            None => "–".to_owned(),
        };
        let baseline = match (&o.doc.baseline, &o.baseline) {
            (None, _) => "none: awaiting the user's approval".to_owned(),
            (Some(_), Some(r)) if r.is_equivalent() => "equivalent".to_owned(),
            (Some(_), Some(r)) => format!(
                "FAILS: {}",
                r.findings
                    .iter()
                    .map(|f| format!("{f:?}"))
                    .collect::<Vec<_>>()
                    .join("; ")
                    .replace('|', "\\|")
            ),
            (Some(_), None) => "–".to_owned(),
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} / {} | {gate} | {rendering} | {baseline} |",
            o.doc.name,
            o.doc.feature,
            count(o.pages),
            count(o.oracle_pages)
        );
    }
    s
}

fn run(url: &str) {
    let target = Target::from_url(url, Duration::from_secs(600));
    let out_dir = root().join("target/corpus");
    let ours = out_dir.join("ours");
    std::fs::create_dir_all(&ours).unwrap();
    let known = divergences();
    let outcomes: Vec<Outcome> = manifest()
        .into_iter()
        .map(|doc| check(&target, doc, &ours, &known))
        .collect();

    // A divergence row that no longer matches a difference is stale: the difference is gone, so is the row.
    let mut stale: Vec<String> = known
        .keys()
        .filter(|(name, check)| {
            check != "converts"
                && !outcomes.iter().any(|o| {
                    &o.doc.name == name
                        && o.oracle_gate
                            .iter()
                            .any(|(f, _)| gate_check(f) == Some(check.as_str()))
                })
        })
        .map(|(name, check)| format!("{name}: {check}"))
        .collect();
    stale.sort();

    let text = report(&outcomes);
    std::fs::write(out_dir.join("report.md"), &text).unwrap();
    println!("{text}");

    let failures: Vec<String> = outcomes
        .iter()
        .flat_map(|o| o.failures().into_iter().map(|f| format!("{}: {f}", o.doc.name)))
        .chain(stale.into_iter().map(|s| {
            format!("{s}: listed in tests/corpus/divergences.tsv but equal now; remove the row and its divergence")
        }))
        .collect();
    assert!(
        failures.is_empty(),
        "the corpus fails:\n{}",
        failures.join("\n")
    );
}

#[test]
fn corpus() {
    if let Ok(oracle) = std::env::var("DOCX_TO_PDF_RECORD_ORACLE") {
        return record(&oracle);
    }
    match std::env::var("DOCX_TO_PDF_CORPUS_TARGET") {
        Ok(url) => run(&url),
        Err(_) => println!(
            "corpus: no DOCX_TO_PDF_CORPUS_TARGET; the corpus runs against the image (make image-corpus)"
        ),
    }
}

#[test]
fn every_document_has_an_input_and_a_golden() {
    let docs = manifest();
    let known = divergences();
    assert!(!docs.is_empty());
    let mut missing = Vec::new();
    for doc in &docs {
        if !doc.input().is_file() {
            missing.push(doc.input().display().to_string());
        }
        if std::env::var_os("DOCX_TO_PDF_RECORD_ORACLE").is_none()
            && doc.unconverted(&known).is_none()
            && !doc.golden().is_file()
        {
            missing.push(doc.golden().display().to_string());
        }
        if let Some(b) = &doc.baseline
            && !baseline_dir().join(b).is_file()
        {
            missing.push(baseline_dir().join(b).display().to_string());
        }
    }
    assert!(missing.is_empty(), "missing:\n{}", missing.join("\n"));
}

/// The origins that are not third parties: what the bench generates, and the project author's own fixtures.
const FIRST_PARTY: [&str; 2] = ["generated", "docx2pdf"];

#[test]
fn every_third_party_document_sits_beside_its_licence() {
    let documents = corpus_dir().join("documents");
    let docs = manifest();
    let mut wrong = Vec::new();
    for doc in &docs {
        let third_party = !FIRST_PARTY.contains(&doc.origin.as_str());
        let folder = format!("third-party/{}/", doc.origin);
        if doc.origin.is_empty() {
            wrong.push(format!("{}: no origin", doc.name));
        } else if third_party != doc.path.starts_with("third-party/") {
            wrong.push(format!(
                "{}: origin {} at {}",
                doc.name, doc.origin, doc.path
            ));
        } else if third_party && !doc.path.starts_with(&folder) {
            wrong.push(format!("{}: not under {folder}", doc.name));
        } else if third_party && !documents.join(&folder).join("LICENSE").is_file() {
            wrong.push(format!("{}: no {folder}LICENSE", doc.name));
        }
    }
    // Every third-party document in the tree is in the manifest, so none sits there unrecorded.
    let listed: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
    for origin in std::fs::read_dir(documents.join("third-party")).unwrap() {
        let origin = origin.unwrap().path();
        for file in std::fs::read_dir(&origin).unwrap() {
            let file = file.unwrap().path();
            let relative = file
                .strip_prefix(&documents)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if file.extension().is_some_and(|e| e == "docx") && !listed.contains(&relative.as_str())
            {
                wrong.push(format!("{relative}: not in the manifest"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the corpus's origins:\n{}",
        wrong.join("\n")
    );
}
