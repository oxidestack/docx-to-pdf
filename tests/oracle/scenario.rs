//! Scenarios, goldens, normalisation and the comparison (`tests/scenarios/README.md`).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use docx_to_pdf::pdf_equivalence::{self, Finding, Thresholds};
use serde::{Deserialize, Serialize};

use crate::http::{self, Part, Response};

/// `tests/scenarios/<name>.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Scenario {
    /// What the scenario shows; read by people only.
    #[allow(dead_code)]
    pub description: String,
    /// What oxidestack/docx-to-pdf still lacks to pass it, in words. While set, replay expects the scenario to fail
    /// and fails when it passes, so the change that lands the behaviour removes the line.
    #[serde(default)]
    pub pending: Option<String>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Step {
    pub method: String,
    pub path: String,
    /// Request headers in order, `[name, value]`.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// A `multipart/form-data` body.
    #[serde(default)]
    pub multipart: Option<Vec<PartSpec>>,
    #[serde(default)]
    pub chunked: bool,
    #[serde(default)]
    pub expect_continue: bool,
    /// Answer headers compared beyond status, `Content-Type` and body: the value, or the absence, must be
    /// moalhaddar/docx-to-pdf's.
    #[serde(default)]
    pub compare: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub note: Option<String>,
}

/// A part: a file from the repository, or a plain field.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PartSpec {
    pub name: String,
    /// A path relative to the repository root.
    #[serde(default)]
    pub file: Option<String>,
    /// The part's file name; the file's own name by default.
    #[serde(default)]
    pub filename: Option<String>,
    /// The part's `Content-Type`; `application/octet-stream` by default, as curl sends a `.docx`.
    #[serde(default)]
    pub content_type: Option<String>,
    /// A plain field's value (no `filename`).
    #[serde(default)]
    pub value: Option<String>,
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

impl Step {
    /// The request this step sends; files are read relative to `root`.
    pub fn request(&self, root: &Path) -> Result<http::Request, String> {
        let mut headers = self.headers.clone();
        let body = match &self.multipart {
            None => None,
            Some(parts) => {
                let parts = parts
                    .iter()
                    .map(|p| p.part(root))
                    .collect::<Result<Vec<_>, _>>()?;
                headers.push((
                    "Content-Type".into(),
                    format!("multipart/form-data; boundary={}", http::BOUNDARY),
                ));
                Some(http::multipart(&parts))
            }
        };
        Ok(http::Request {
            method: self.method.clone(),
            path: self.path.clone(),
            headers,
            body,
            chunked: self.chunked,
            expect_continue: self.expect_continue,
        })
    }
}

impl PartSpec {
    fn part(&self, root: &Path) -> Result<Part, String> {
        match (&self.file, &self.value) {
            (Some(file), None) => {
                let path = root.join(file);
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let filename = self.filename.clone().unwrap_or_else(|| {
                    Path::new(file)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
                Ok(Part::File {
                    name: self.name.clone(),
                    filename,
                    content_type: self
                        .content_type
                        .clone()
                        .unwrap_or_else(|| "application/octet-stream".into()),
                    bytes,
                })
            }
            (None, Some(value)) => Ok(Part::Field {
                name: self.name.clone(),
                value: value.clone(),
            }),
            _ => Err(format!(
                "part {:?}: exactly one of `file` and `value`",
                self.name
            )),
        }
    }
}

/// The placeholder `Date` is normalised to.
pub const DATE: &str = "<date>";

/// Normalisation: values that are time-based by design replaced with placeholders, so an answer recorded once
/// compares with one given later.
pub fn normalise(mut response: Response) -> Response {
    for (name, value) in &mut response.headers {
        if name == "date" {
            *value = DATE.into();
        }
    }
    response
}

/// `tests/scenarios/<name>.golden.json`: moalhaddar/docx-to-pdf's answers, one per step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Golden {
    /// The image the answers came from (`oracle.env`).
    pub recorded_from: String,
    pub results: Vec<GoldenResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenResult {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interim: Vec<u16>,
    pub status: u16,
    /// Every header as `name: value`, names lowercased, in the order received, normalised. Only the ones
    /// the comparison names are compared; the rest are a record.
    pub headers: Vec<String>,
    pub body: GoldenBody,
}

/// A body: text, a PDF kept beside the golden (compared by PDF equivalence), or other bytes as hex.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoldenBody {
    Text(String),
    Pdf(String),
    Hex(String),
}

/// Where a scenario's golden and its PDFs live.
pub struct GoldenFiles {
    pub dir: PathBuf,
    pub name: String,
}

impl GoldenFiles {
    pub fn of(scenario: &Path) -> Self {
        let name = scenario
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_end_matches(".json")
            .to_owned();
        Self {
            dir: scenario.parent().unwrap().to_owned(),
            name,
        }
    }

    pub fn golden(&self) -> PathBuf {
        self.dir.join(format!("{}.golden.json", self.name))
    }

    fn pdf_name(&self, step: usize) -> String {
        format!("{}.{}.golden.pdf", self.name, step + 1)
    }

    pub fn load(&self) -> Result<Golden, String> {
        let path = self.golden();
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("{}: {e} (record it: make record)", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Writes the golden of `responses`, a PDF body as its own file.
    pub fn write(&self, recorded_from: &str, responses: &[Response]) -> Result<(), String> {
        let mut results = Vec::new();
        for (i, r) in responses.iter().enumerate() {
            let body = if is_pdf(r) {
                let name = self.pdf_name(i);
                let path = self.dir.join(&name);
                std::fs::write(&path, &r.body).map_err(|e| format!("{}: {e}", path.display()))?;
                GoldenBody::Pdf(name)
            } else {
                match String::from_utf8(r.body.clone()) {
                    Ok(text) => GoldenBody::Text(text),
                    Err(_) => GoldenBody::Hex(r.body.iter().fold(String::new(), |mut s, b| {
                        let _ = write!(s, "{b:02x}");
                        s
                    })),
                }
            };
            results.push(GoldenResult {
                interim: r.interim.clone(),
                status: r.status,
                headers: r.headers.iter().map(|(n, v)| format!("{n}: {v}")).collect(),
                body,
            });
        }
        let golden = Golden {
            recorded_from: recorded_from.into(),
            results,
        };
        let path = self.golden();
        let text = serde_json::to_string_pretty(&golden).unwrap() + "\n";
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The golden answer as a [`Response`], the PDF read back from its file.
    pub fn expected(&self, result: &GoldenResult) -> Result<Response, String> {
        let body = match &result.body {
            GoldenBody::Text(t) => t.clone().into_bytes(),
            GoldenBody::Pdf(name) => {
                let path = self.dir.join(name);
                std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?
            }
            GoldenBody::Hex(h) => (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).map_err(|e| e.to_string()))
                .collect::<Result<_, _>>()?,
        };
        let headers = result
            .headers
            .iter()
            .map(|l| {
                let (n, v) = l.split_once(": ").unwrap_or((l.as_str(), ""));
                (n.to_owned(), v.to_owned())
            })
            .collect();
        Ok(Response {
            interim: result.interim.clone(),
            status: result.status,
            headers,
            body,
        })
    }
}

fn is_pdf(r: &Response) -> bool {
    r.header("content-type")
        .is_some_and(|t| t.split(';').next().unwrap_or_default().trim() == "application/pdf")
}

/// Every way `actual` differs from the golden `expected` for `step`: the interim statuses, the status,
/// `Content-Type`, the headers the step names (value or absence), and the body — a PDF by PDF equivalence,
/// anything else byte for byte. Both are normalised first. Empty when they agree.
pub fn compare(step: &Step, expected: &Response, actual: &Response) -> Vec<String> {
    let (expected, actual) = (normalise(expected.clone()), normalise(actual.clone()));
    let mut differences = Vec::new();
    if expected.interim != actual.interim {
        differences.push(format!(
            "interim statuses: expected {:?}, got {:?}",
            expected.interim, actual.interim
        ));
    }
    if expected.status != actual.status {
        differences.push(format!(
            "status: expected {}, got {}",
            expected.status, actual.status
        ));
    }
    let mut names = vec!["content-type".to_owned()];
    names.extend(step.compare.iter().map(|n| n.to_ascii_lowercase()));
    for name in names {
        let (e, a) = (expected.header(&name), actual.header(&name));
        if e != a {
            differences.push(format!("header {name}: expected {e:?}, got {a:?}"));
        }
    }
    if is_pdf(&expected) {
        if is_pdf(&actual) {
            match pdf_equivalence::compare_bytes(
                &expected.body,
                &actual.body,
                &Thresholds::default(),
            ) {
                // Against moalhaddar/docx-to-pdf the page count, the page sizes and the text gate; the rendered
                // pages are reported, not gating, because LibreOffice 26.8 lays out many documents differently from
                // moalhaddar/docx-to-pdf's 7.3 in ways only a person can rate. Rendering gates against
                // oxidestack/docx-to-pdf's own baseline, approved by eye once, not here.
                Ok(report) => {
                    let (rendering, gating): (Vec<_>, Vec<_>) = report
                        .findings
                        .into_iter()
                        .partition(|f| matches!(f, Finding::Rendering { .. }));
                    for finding in &rendering {
                        eprintln!("         reported, not gating: {finding:?}");
                    }
                    if !gating.is_empty() {
                        differences.push(format!(
                            "body: PDFs not equivalent: {gating:?} (reported, not gating: {rendering:?})"
                        ));
                    }
                }
                Err(e) => differences.push(format!("body: cannot compare the PDFs: {e:?}")),
            }
        } else {
            differences.push("body: expected a PDF, the answer is not one".into());
        }
    } else if expected.body != actual.body {
        differences.push(format!(
            "body: expected {:?}, got {:?}",
            excerpt(&expected.body),
            excerpt(&actual.body)
        ));
    }
    differences
}

fn excerpt(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if text.chars().count() > 200 {
        format!("{}…", text.chars().take(200).collect::<String>())
    } else {
        text.into_owned()
    }
}
