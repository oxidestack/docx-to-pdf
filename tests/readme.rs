//! The README against the service and the repository.
//!
//! - **The quick start works.** Its `curl` is sent to the service's router in process, with a pool of one stand-in
//!   instance (`/bin/sh` speaking the instance protocol, as `tests/api_reference.rs` does) and the default
//!   configuration, and must answer `200` with a PDF. Its `docker run` names the image this version builds,
//!   `quay.io/oxidestack/docx-to-pdf:<Cargo version>`, and publishes the port the service listens on by default,
//!   the one the Dockerfile exposes, at the port the `curl` calls.
//! - **The documentation list** links every page of `docs/guide/`, the three named by the README's criteria with
//!   their exact text; every relative link resolves.
//! - Every `make <target>` it names is a target of the Makefile.
//!
//! No LibreOffice needed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::config::{Config, DEFAULT_PORT};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::{Telemetry, TelemetryBuilder};
use http_body_util::BodyExt;
use tower::ServiceExt;

const PAGE: &str = "README.md";
const IMAGE: &str = "quay.io/oxidestack/docx-to-pdf";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// The lines of every fenced block with the info string `sh`.
fn sh_blocks(page: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut open: Option<(String, Vec<String>)> = None;
    for line in page.lines() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(info)) => open = Some((info.trim().to_owned(), Vec::new())),
            (Some(_), Some(_)) => {
                let (info, lines) = open.take().unwrap();
                if info == "sh" {
                    out.push(lines);
                }
            }
            (Some((_, lines)), None) => lines.push(line.to_owned()),
            (None, None) => {}
        }
    }
    assert!(open.is_none(), "{PAGE}: a fence is never closed");
    out
}

/// The text of the section under `## <heading>`, up to the next `## `.
fn section<'a>(page: &'a str, heading: &str) -> &'a str {
    let start = page
        .find(&format!("\n## {heading}\n"))
        .unwrap_or_else(|| panic!("{PAGE}: no section {heading:?}"))
        + 1;
    let end = page[start + 3..]
        .find("\n## ")
        .map_or(page.len(), |i| start + 3 + i);
    &page[start..end]
}

/// The one command in the quick start's `sh` blocks that starts with `program`, as words (no quoting is used).
fn quick_start_command(program: &str) -> Vec<String> {
    let page = read(PAGE);
    let commands: Vec<Vec<String>> = sh_blocks(section(&page, "Quick start"))
        .into_iter()
        .flatten()
        .map(|l| l.split_whitespace().map(str::to_owned).collect())
        .filter(|w: &Vec<String>| w.first().is_some_and(|p| p == program))
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "{PAGE}: the quick start has one `{program}` command"
    );
    commands.into_iter().next().unwrap()
}

/// The value after `flag` in `words`.
fn after<'a>(words: &'a [String], flag: &str) -> &'a str {
    let i = words
        .iter()
        .position(|w| w == flag)
        .unwrap_or_else(|| panic!("{words:?}: no {flag}"));
    words
        .get(i + 1)
        .unwrap_or_else(|| panic!("{flag} takes a value"))
}

/// What the quick start's `curl` sends: the field and file of its one `-F`, and the URL.
struct Curl {
    field: String,
    file: String,
    url: String,
}

fn quick_start_curl() -> Curl {
    let words = quick_start_command("curl");
    let mut form = None;
    let mut url = None;
    let mut args = words[1..].iter();
    while let Some(word) = args.next() {
        match word.as_str() {
            "-F" => {
                let value = args.next().expect("-F takes a value");
                let (field, file) = value.split_once("=@").expect("-F field=@file");
                assert!(form.is_none(), "one -F");
                form = Some((field.to_owned(), file.to_owned()));
            }
            "-o" => {
                args.next().expect("-o takes a value");
            }
            w if w.starts_with("http://") => url = Some(w.to_owned()),
            w => panic!("{PAGE}: the replay does not know the curl option {w}"),
        }
    }
    let (field, file) = form.expect("a -F field=@file");
    Curl {
        field,
        file,
        url: url.expect("a URL"),
    }
}

/// The stand-in instance: ready at once; any document becomes a small PDF.
const STAND_IN: &str = r#"
    test -d "$1" || exit 9
    printf 'ready\t1\t%s\n' "$$"
    tab="$(printf '\t')"
    while IFS="$tab" read -r cmd input output; do
        printf '%%PDF-1.4\n%%%%EOF\n' > "$output"; printf 'ok\t3\t4\n'
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
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the names are this repository's own and lowercase"
)]
async fn the_quick_start_curl_returns_a_pdf() {
    let curl = quick_start_curl();
    let path = curl
        .url
        .strip_prefix(&format!("http://localhost:{DEFAULT_PORT}"))
        .unwrap_or_else(|| panic!("{}: not the service's default port", curl.url))
        .to_owned();
    // The file the quick start names stands for any Word document; a real DOCX passes the format check.
    assert!(
        curl.file.ends_with(".docx"),
        "{}: a DOCX in the quick start",
        curl.file
    );
    let document = std::fs::read(root().join("tests/scenarios/fixtures/comment.docx")).unwrap();

    const BOUNDARY: &str = "------------------------QuickStartBoundary";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n",
        curl.field, curl.file
    )
    .into_bytes();
    body.extend_from_slice(&document);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let request = Request::post(&path)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .header("content-length", body.len())
        .body(Body::from(body))
        .unwrap();

    let profiles = tempfile::tempdir().unwrap();
    let (app, _telemetry) = service(profiles.path()).await;
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response.headers().get("content-type").cloned();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(content_type.unwrap(), "application/pdf");
    assert!(body.starts_with(b"%PDF-"), "the answer is a PDF");
}

#[test]
fn the_quick_start_docker_run_is_this_version_on_the_default_port() {
    let words = quick_start_command("docker");
    assert_eq!(words.get(1).map(String::as_str), Some("run"), "{words:?}");
    assert_eq!(
        words.last().unwrap(),
        &format!("{IMAGE}:{}", env!("CARGO_PKG_VERSION")),
        "{PAGE}: the quick start runs the image of this version"
    );

    let (host, container) = after(&words, "-p")
        .split_once(':')
        .expect("-p host:container");
    assert_eq!(
        container,
        DEFAULT_PORT.to_string(),
        "-p publishes the service's default port"
    );
    let dockerfile = read("Dockerfile");
    assert!(
        dockerfile
            .lines()
            .any(|l| l.trim() == format!("EXPOSE {DEFAULT_PORT}")),
        "the Dockerfile exposes the default port {DEFAULT_PORT}"
    );
    let curl = quick_start_curl();
    assert!(
        curl.url.starts_with(&format!("http://localhost:{host}/")),
        "{}: the curl calls the port docker run publishes ({host})",
        curl.url
    );
}

/// Every Markdown link of the page: (text, target).
fn links(page: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = page;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find("](") else { break };
        if rest[..close].contains('[') {
            continue;
        }
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

#[test]
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the names are this repository's own and lowercase"
)]
fn the_documentation_list_links_every_guide_page() {
    let page = read(PAGE);
    let list = section(&page, "Documentation");
    for line in [
        "- [Configuration](docs/guide/configuration.md) — every environment variable, its default and its effect",
        "- [API reference](docs/guide/api.md) — the endpoints, the formats accepted, every status and its detail",
        "- [Operations](docs/guide/operations.md) — the container, health checks, Kubernetes, telemetry and metrics, \
         limits, sizing and fonts",
    ] {
        // An item wrapped at 120 columns continues on lines indented by two spaces.
        assert!(
            list.replace("\n  ", " ").lines().any(|l| l == line),
            "{PAGE}: the documentation list has no line\n{line}"
        );
    }

    let linked: Vec<String> = links(list).into_iter().map(|(_, t)| t).collect();
    let mut pages: Vec<String> = std::fs::read_dir(root().join("docs/guide"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    pages.sort();
    let missing: Vec<&String> = pages
        .iter()
        .filter(|p| !linked.contains(&format!("docs/guide/{p}")))
        .collect();
    assert!(
        missing.is_empty(),
        "{PAGE}: the documentation list does not link {missing:?}"
    );
}

#[test]
fn every_relative_link_resolves() {
    let page = read(PAGE);
    let broken: Vec<String> = links(&page)
        .into_iter()
        .map(|(_, t)| t)
        .filter(|t| !t.contains("://") && !t.starts_with('#'))
        .filter(|t| !root().join(t.split('#').next().unwrap()).exists())
        .collect();
    assert!(broken.is_empty(), "{PAGE}: links to nothing: {broken:?}");
}

#[test]
fn every_make_target_named_exists() {
    let page = read(PAGE);
    let makefile = read("Makefile");
    let mut named = 0;
    let mut rest = page.as_str();
    while let Some(i) = rest.find("`make ") {
        rest = &rest[i + 6..];
        let target: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        assert!(
            makefile
                .lines()
                .any(|l| l.starts_with(&format!("{target}:"))),
            "{PAGE}: `make {target}` is not a target of the Makefile"
        );
        named += 1;
    }
    assert!(named >= 2, "{PAGE}: only {named} make targets found");
}

#[test]
fn the_link_reading_finds_text_and_target() {
    assert_eq!(
        links("a [b](c.md) and [`d`](e#f) [x] y"),
        [
            ("b".to_owned(), "c.md".to_owned()),
            ("`d`".to_owned(), "e#f".to_owned())
        ]
    );
}
