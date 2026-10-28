//! The API reference, `docs/guide/api.md`, against the code, so a client can rely on every status and example in it:
//!
//! - every status a [`Problem`] can answer has a row in the page's error tables, and every problem `detail` the source
//!   spells out is in them, in backticks; every `detail` the tables name is one the code can answer;
//! - every `curl` example on the page is sent to the service and its answer compared with the `http` block that
//!   follows it: the status, each header shown, and the body shown (JSON by value; `<…>` stands for any value).
//!
//! The examples go through the service's router in process, with a pool of stand-in instances (`/bin/sh` speaking the
//! instance protocol, as `tests/conversion.rs` does) and the default configuration, so they replay anywhere and need
//! no LibreOffice. A stand-in converts any document to a small PDF, and answers "cannot load" to one that holds
//! `BROKEN`. The files the examples name are made by [`example_file`].

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::config::Config;
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{Pool, PoolSettings};
use docx_to_pdf::problem::Problem;
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PAGE: &str = "docs/guide/api.md";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn page() -> String {
    std::fs::read_to_string(root().join(PAGE))
        .unwrap_or_else(|e| panic!("{PAGE}: {e}; the API reference is missing"))
}

// ---------------------------------------------------------------------------------------------------------------
// Every status and every detail.

/// One of each [`Problem`]. The match below is exhaustive, so a new variant does not compile until it is listed
/// here too, and the test then asks the page for its status.
fn every_problem() -> Vec<Problem> {
    let all = vec![
        Problem::BadRequest(""),
        Problem::NotFound,
        Problem::MethodNotAllowed,
        Problem::RequestTimeout(""),
        Problem::TooLarge(""),
        Problem::UnsupportedFormat(""),
        Problem::CannotLoad(""),
        Problem::Unavailable {
            detail: "",
            retry_after: Duration::ZERO,
        },
        Problem::Timeout(""),
        Problem::Internal,
    ];
    for problem in &all {
        match problem {
            Problem::BadRequest(_)
            | Problem::NotFound
            | Problem::MethodNotAllowed
            | Problem::RequestTimeout(_)
            | Problem::TooLarge(_)
            | Problem::UnsupportedFormat(_)
            | Problem::CannotLoad(_)
            | Problem::Unavailable { .. }
            | Problem::Timeout(_)
            | Problem::Internal => {}
        }
    }
    all
}

/// `src/**/*.rs` outside test modules, as `(path, text)`.
fn sources() -> Vec<(PathBuf, String)> {
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
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            let end = lines
                .windows(2)
                .position(|w| {
                    w[0].trim() == "#[cfg(test)]" && w[1].trim_start().starts_with("mod ")
                })
                .unwrap_or(lines.len());
            (path, lines[..end].join("\n"))
        })
        .collect()
}

/// The string literal that starts at `text[at]` (a `"`), unescaped, and the index past its closing quote.
fn literal(text: &str, at: usize) -> (String, usize) {
    let bytes = text.as_bytes();
    assert_eq!(bytes[at], b'"');
    let mut out = String::new();
    let mut i = at + 1;
    while bytes[i] != b'"' {
        if bytes[i] == b'\\' {
            i += 1;
            out.push(match bytes[i] {
                b'n' => '\n',
                b't' => '\t',
                other => other as char,
            });
            i += 1;
        } else {
            let c = text[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    (out, i + 1)
}

/// Every string literal in `span`.
fn literals(span: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(at) = span[i..].find('"') {
        let (value, end) = literal(span, i + at);
        out.push(value);
        i = end;
    }
    out
}

/// The value of `const NAME: &str = "…";` anywhere in the source.
fn constant(sources: &[(PathBuf, String)], name: &str) -> Option<String> {
    let needle = format!("const {name}: &str = ");
    sources.iter().find_map(|(_, text)| {
        let at = text.find(&needle)? + needle.len();
        Some(literal(text, at).0)
    })
}

/// The string literals of `fn name(…) { … }` anywhere in the source, up to the first line that is a lone `}`.
fn function_literals(sources: &[(PathBuf, String)], name: &str) -> Option<Vec<String>> {
    let needle = format!("fn {name}(");
    sources.iter().find_map(|(_, text)| {
        let at = text.find(&needle)?;
        let end = text[at..].find("\n}").map_or(text.len(), |e| at + e);
        let body = &text[at..end];
        let open = body.find('{')?;
        Some(literals(&body[open..]))
    })
}

/// Every `detail` the source can put in a problem document:
/// - the argument of `Problem::Variant(…)`: a string literal, or a constant resolved to its value;
/// - the expression of a `detail:` field up to the `retry_after` that follows it: its string literals, and those
///   of each function it calls;
/// - the fixed details of the variants that carry none.
fn details_in_code() -> BTreeSet<String> {
    let sources = sources();
    let mut details = BTreeSet::new();
    for problem in every_problem() {
        let detail = problem.detail();
        if !detail.is_empty() {
            details.insert(detail.to_owned());
        }
    }
    for (path, text) in &sources {
        for variant in [
            "BadRequest",
            "RequestTimeout",
            "TooLarge",
            "UnsupportedFormat",
            "CannotLoad",
            "Timeout",
        ] {
            let needle = format!("Problem::{variant}(");
            for (at, _) in text.match_indices(&needle) {
                let rest = &text[at + needle.len()..];
                let arg = rest.trim_start();
                if arg.starts_with('"') {
                    let start = text.len() - arg.len();
                    details.insert(literal(text, start).0);
                } else if arg.starts_with('_') {
                    // A pattern, `Problem::TooLarge(_)`, not a construction.
                } else {
                    let name: String = arg
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
                        .collect();
                    let name = name.rsplit("::").next().unwrap();
                    if name
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    {
                        let value = constant(&sources, name).unwrap_or_else(|| {
                            panic!(
                                "{}: Problem::{variant}({name}): no const {name}",
                                path.display()
                            )
                        });
                        details.insert(value);
                    } else if !["d", "detail"].contains(&name) {
                        panic!(
                            "{}: Problem::{variant}({name}…): the test cannot tell this detail; teach \
                             details_in_code",
                            path.display()
                        );
                    }
                }
            }
        }
        for (at, _) in text.match_indices("detail: ") {
            let rest = &text[at..];
            let Some(end) = rest.find("retry_after") else {
                continue;
            };
            let span = &rest["detail: ".len()..end];
            // A field's type, a pattern (`detail: d, ..`), or a span past the end of the expression.
            if span.contains(';') || span.trim().starts_with("&'static") {
                continue;
            }
            details.extend(literals(span));
            let mut i = 0;
            while let Some(open) = span[i..].find('(') {
                let before = &span[..i + open];
                let name: String = before
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                if !name.is_empty() && name.chars().next().unwrap().is_ascii_lowercase() {
                    details.extend(function_literals(&sources, &name).unwrap_or_else(|| {
                        panic!("{}: detail: {name}(…): no fn {name}", path.display())
                    }));
                }
                i += open + 1;
            }
        }
    }
    details
}

/// The rows of the page's tables whose first cell is a status code: `(status, the second cell)`.
fn status_rows(page: &str) -> Vec<(u16, String)> {
    page.lines()
        .filter_map(|line| {
            let row = line.trim().strip_prefix('|')?;
            let mut cells = row.split(" | ").map(str::trim);
            let status: u16 = cells.next()?.trim_start_matches('|').trim().parse().ok()?;
            Some((status, cells.next().unwrap_or("").to_owned()))
        })
        .collect()
}

/// The details the page's tables name: the second cell of a status row, held in backticks.
fn details_on_the_page(page: &str) -> BTreeSet<String> {
    status_rows(page)
        .into_iter()
        .filter_map(|(_, cell)| {
            cell.strip_prefix('`')
                .and_then(|c| c.strip_suffix('`'))
                .map(str::to_owned)
        })
        .collect()
}

#[test]
fn every_status_and_detail_the_code_answers_is_on_the_page_and_no_other() {
    let page = page();
    let statuses: BTreeSet<u16> = status_rows(&page).into_iter().map(|(s, _)| s).collect();
    let missing_statuses: Vec<_> = every_problem()
        .iter()
        .map(|p| p.status().as_u16())
        .filter(|s| !statuses.contains(s))
        .collect();

    let code = details_in_code();
    let on_page = details_on_the_page(&page);
    let missing: Vec<_> = code.difference(&on_page).collect();
    let unknown: Vec<_> = on_page.difference(&code).collect();
    assert!(
        missing_statuses.is_empty() && missing.is_empty() && unknown.is_empty(),
        "{PAGE} and the code disagree.\n  statuses with no row: {missing_statuses:?}\n  \
         details the code answers, not in a table: {missing:?}\n  details in a table the code never answers: \
         {unknown:?}"
    );
}

#[test]
fn the_source_reading_finds_every_kind_of_detail() {
    // A guard on the method: if the source reading stopped finding a kind of detail, the comparison above would pass
    // on less.
    let code = details_in_code();
    for detail in [
        // A literal argument.
        "the multipart body is malformed",
        // A constant argument, by path.
        "the document is not DOCX, DOC, ODT or RTF",
        "the conversion did not finish within the conversion timeout",
        // A literal `detail:`.
        "the service is not ready to convert documents yet",
        // A `match` in `detail:`.
        "a probe is already running",
        // A function called in `detail:`.
        "too many requests are already waiting for an instance",
        // A literal after a line break.
        "rounds, busy, cpus and disk_mib are whole numbers within their bounds",
        // A variant's fixed detail.
        "the service failed to answer this request",
        // An escaped quote.
        "the body has no file part named \"document\"",
    ] {
        assert!(code.contains(detail), "{detail:?} not found: {code:#?}");
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The examples.

/// A fenced block of the page: its info string and its lines.
struct Fence {
    info: String,
    lines: Vec<String>,
    line: usize,
}

fn fences(page: &str) -> Vec<Fence> {
    let mut out = Vec::new();
    let mut open: Option<Fence> = None;
    for (n, line) in page.lines().enumerate() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(info)) => {
                open = Some(Fence {
                    info: info.trim().to_owned(),
                    lines: Vec::new(),
                    line: n + 1,
                });
            }
            (Some(_), Some(_)) => out.push(open.take().unwrap()),
            (Some(fence), None) => fence.lines.push(line.to_owned()),
            (None, None) => {}
        }
    }
    assert!(open.is_none(), "{PAGE}: a fence is never closed");
    out
}

/// The words of a shell command line: single and double quotes, no expansion.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' => out.extend(word.take()),
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    w.push(c);
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => w.push(chars.next().unwrap()),
                        c => w.push(c),
                    }
                }
            }
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    out.extend(word);
    out
}

/// A part of `curl -F`.
enum Part {
    File { name: String, file: String },
    Field { name: String, value: String },
}

/// What a `curl` example asks for.
struct Curl {
    method: Method,
    path: String,
    headers: Vec<(String, String)>,
    form: Vec<Part>,
    /// `-i` or `-D -`: the answer's head is printed.
    shows_head: bool,
    /// `-o FILE`: the body goes to a file, not shown.
    body_to_file: bool,
}

/// The `curl` command, in the subset of options the examples use; any other option fails the test, so an example
/// cannot say what the replay does not do.
fn parse_curl(line: &str) -> Curl {
    let words = words(line);
    assert_eq!(words.first().map(String::as_str), Some("curl"), "{line}");
    let mut method = None;
    let mut url = None;
    let mut headers = Vec::new();
    let mut form = Vec::new();
    let mut shows_head = false;
    let mut body_to_file = false;
    let mut args = words[1..].iter();
    while let Some(word) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| panic!("{line}: {word} takes a value"))
                .clone()
        };
        match word.as_str() {
            "-s" | "-S" | "-sS" => {}
            "-i" => shows_head = true,
            "-D" => {
                assert_eq!(value(), "-", "{line}: only -D - is replayed");
                shows_head = true;
            }
            "-o" => {
                value();
                body_to_file = true;
            }
            "-X" => method = Some(Method::from_bytes(value().as_bytes()).unwrap()),
            "-H" => {
                let header = value();
                let (name, v) = header.split_once(':').expect("-H 'Name: value'");
                headers.push((name.trim().to_owned(), v.trim().to_owned()));
            }
            "-F" => {
                let field = value();
                let (name, v) = field.split_once('=').expect("-F name=value");
                form.push(match v.strip_prefix('@') {
                    Some(file) => Part::File {
                        name: name.to_owned(),
                        file: file.to_owned(),
                    },
                    None => Part::Field {
                        name: name.to_owned(),
                        value: v.to_owned(),
                    },
                });
            }
            w if w.starts_with("http://") => url = Some(w.to_owned()),
            w => panic!("{line}: the replay does not know {w}"),
        }
    }
    let url = url.unwrap_or_else(|| panic!("{line}: no URL"));
    let path = url
        .strip_prefix("http://localhost:8080")
        .unwrap_or_else(|| panic!("{line}: the examples use http://localhost:8080"))
        .to_owned();
    let method = method.unwrap_or(if form.is_empty() {
        Method::GET
    } else {
        Method::POST
    });
    Curl {
        method,
        path,
        headers,
        form,
        shows_head,
        body_to_file,
    }
}

/// The bytes of a file an example names.
fn example_file(name: &str) -> Vec<u8> {
    match name {
        // A real DOCX, so that it passes the format check.
        "report.docx" => {
            std::fs::read(root().join("tests/scenarios/fixtures/comment.docx")).unwrap()
        }
        // A PNG's signature and header: not a document.
        "photo.png" => {
            b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x01\0\0\0\x01\x08\x02\0\0\0".to_vec()
        }
        // An RTF the stand-in instance cannot load.
        "damaged.rtf" => b"{\\rtf1 BROKEN".to_vec(),
        // 11 MiB, over the default 10 MiB limit.
        "scan.docx" => {
            let mut bytes = b"PK\x03\x04".to_vec();
            bytes.resize(11 * 1024 * 1024, 0);
            bytes
        }
        // A DOCX whose document part inflates to 257 MiB of zeros, past the default 256 MiB.
        "bomb.docx" => {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .large_file(true);
            zip.start_file("[Content_Types].xml", options).unwrap();
            zip.write_all(
                br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            )
            .unwrap();
            zip.start_file("word/document.xml", options).unwrap();
            let mib = vec![0u8; 1024 * 1024];
            for _ in 0..257 {
                zip.write_all(&mib).unwrap();
            }
            zip.finish().unwrap().into_inner()
        }
        other => panic!("an example names {other}; add it to example_file"),
    }
}

const BOUNDARY: &str = "------------------------ExampleBoundary";

fn request(curl: &Curl) -> Request<Body> {
    let mut request = Request::builder()
        .method(curl.method.clone())
        .uri(&curl.path);
    for (name, value) in &curl.headers {
        request = request.header(name, value);
    }
    if curl.form.is_empty() {
        return request.body(Body::empty()).unwrap();
    }
    let mut body = Vec::new();
    for part in &curl.form {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        match part {
            Part::File { name, file } => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{file}\"\r\n\
                         Content-Type: application/octet-stream\r\n\r\n"
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(&example_file(file));
            }
            Part::Field { name, value } => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}")
                    .as_bytes(),
            ),
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    request
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header("content-length", body.len())
        .body(Body::from(body))
        .unwrap()
}

/// `<…>`: any value.
fn is_placeholder(value: &str) -> bool {
    value.starts_with('<') && value.ends_with('>')
}

/// Every difference between the answer and the `http` block that shows it.
fn compare(
    curl: &Curl,
    shown: &[String],
    status: StatusCode,
    headers: &axum::http::HeaderMap,
    body: &[u8],
) -> Vec<String> {
    let mut differences = Vec::new();
    let mut lines = shown.iter();
    let status_line = lines.next().expect("a status line");
    let mut words = status_line.splitn(3, ' ');
    assert_eq!(words.next(), Some("HTTP/1.1"), "{status_line}");
    let code: u16 = words.next().unwrap().parse().unwrap();
    let reason = words.next().unwrap_or("");
    if code != status.as_u16() {
        differences.push(format!("status: shown {code}, answered {status}"));
    }
    if StatusCode::from_u16(code).unwrap().canonical_reason() != Some(reason) {
        differences.push(format!(
            "status line: {status_line:?} has not the reason phrase of {code}"
        ));
    }
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(": ").expect("Name: value");
        match headers.get(name).map(|v| v.to_str().unwrap()) {
            None => differences.push(format!("{name}: shown, not answered")),
            Some(got) if !is_placeholder(value) && got != value => {
                differences.push(format!("{name}: shown {value:?}, answered {got:?}"));
            }
            Some(_) => {}
        }
    }
    let shown_body = lines.cloned().collect::<Vec<_>>().join("\n");
    let pdf = headers
        .get("content-type")
        .is_some_and(|v| v == "application/pdf");
    if pdf && !body.starts_with(b"%PDF-") {
        differences.push("the PDF does not start with %PDF-".into());
    }
    if curl.body_to_file {
        assert!(
            shown_body.is_empty(),
            "the body goes to a file, yet a body is shown"
        );
    } else if is_placeholder(shown_body.trim()) {
        if body.is_empty() {
            differences.push("an empty body, a body is shown".into());
        }
    } else if shown_body.starts_with('{') {
        let shown: serde_json::Value =
            serde_json::from_str(&shown_body).expect("the body shown is JSON");
        match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(got) if got == shown => {}
            _ => differences.push(format!(
                "body: shown {shown_body}, answered {}",
                String::from_utf8_lossy(body)
            )),
        }
    } else if !pdf && shown_body.trim_end() != String::from_utf8_lossy(body).trim_end() {
        differences.push(format!(
            "body: shown {shown_body:?}, answered {:?}",
            String::from_utf8_lossy(body)
        ));
    }
    differences
}

/// The stand-in instance: ready at once; `BROKEN` cannot be loaded, anything else becomes a small PDF.
const STAND_IN: &str = r#"
    test -d "$1" || exit 9
    printf 'ready\t1\t%s\n' "$$"
    tab="$(printf '\t')"
    while IFS="$tab" read -r cmd input output; do
        case "$(cat "$input")" in
            *BROKEN*) printf 'not_loaded\tGeneral Error\n' ;;
            *) printf '%%PDF-1.4\n%%%%EOF\n' > "$output"; printf 'ok\t3\t4\n' ;;
        esac
    done
"#;

/// The service as the binary builds it from the default configuration, its pool of one stand-in instance.
async fn service(dir: &Path) -> (Router, Telemetry) {
    let config = Config::from_lookup(|_| None).unwrap();
    let telemetry = TelemetryBuilder::new()
        .with_otlp_from(|_| None)
        .build()
        .unwrap();
    let readiness = Readiness::new();
    let launcher = Launcher::with_args(
        "/bin/sh",
        ["-c".to_string(), STAND_IN.to_string(), "sh".to_string()],
        dir.to_path_buf(),
    );
    let settings = PoolSettings {
        start_timeout: Duration::from_secs(10),
        ..PoolSettings::from_config(&config)
    };
    let pool = Pool::start(launcher, settings, readiness.clone(), &telemetry.meter())
        .await
        .expect("the stand-in pool starts");
    let state = AppState::from_config(readiness, &telemetry, &config);
    state.set_pool(pool);
    (router(state), telemetry)
}

#[tokio::test]
async fn every_example_answers_as_the_page_shows() {
    let page = page();
    let fences = fences(&page);
    let profiles = tempfile::tempdir().unwrap();
    let (app, _telemetry) = service(profiles.path()).await;

    let mut replayed = 0;
    let mut failures = Vec::new();
    for (i, fence) in fences.iter().enumerate() {
        if fence.info != "sh" || !fence.lines.iter().any(|l| l.starts_with("curl ")) {
            continue;
        }
        assert_eq!(
            fence.lines.len(),
            1,
            "line {}: one curl command a block",
            fence.line
        );
        let line = &fence.lines[0];
        let curl = parse_curl(line);
        assert!(
            curl.shows_head,
            "line {}: -i or -D -, so that the answer shows its head",
            fence.line
        );
        let shown = fences
            .get(i + 1)
            .filter(|f| f.info == "http")
            .unwrap_or_else(|| panic!("line {}: no http block after the example", fence.line));

        let response = app.clone().oneshot(request(&curl)).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let differences = compare(&curl, &shown.lines, status, &headers, &body);
        if !differences.is_empty() {
            failures.push(format!(
                "line {}: {line}\n    {}",
                fence.line,
                differences.join("\n    ")
            ));
        }
        replayed += 1;
    }
    assert!(replayed >= 10, "only {replayed} examples found on {PAGE}");
    assert!(
        failures.is_empty(),
        "{PAGE}: examples that answer otherwise:\n{}",
        failures.join("\n")
    );
}

#[test]
fn the_curl_reading_takes_quotes_and_refuses_what_it_does_not_replay() {
    let curl = parse_curl(
        "curl -sS -i -X OPTIONS -H 'Origin: https://a.example' -H \"X-A: b c\" -F document=@report.docx -F n=v \
         http://localhost:8080/pdf",
    );
    assert_eq!(curl.method, Method::OPTIONS);
    assert_eq!(curl.path, "/pdf");
    assert_eq!(
        curl.headers,
        [
            ("Origin".to_owned(), "https://a.example".to_owned()),
            ("X-A".to_owned(), "b c".to_owned())
        ]
    );
    assert!(
        matches!(&curl.form[0], Part::File { name, file } if name == "document" && file == "report.docx")
    );
    assert!(matches!(&curl.form[1], Part::Field { name, value } if name == "n" && value == "v"));
    assert!(
        std::panic::catch_unwind(|| parse_curl("curl --compressed http://localhost:8080/pdf"))
            .is_err()
    );
}
