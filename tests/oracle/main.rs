//! moalhaddar/docx-to-pdf scenarios (`tests/scenarios/README.md`). Each `tests/scenarios/<name>.json` is a
//! list of HTTP steps on the interface. With `DOCX_TO_PDF_RECORD_ORACLE=http://localhost:48080` (`make record`)
//! the steps go to moalhaddar/docx-to-pdf and its answers become `<name>.golden.json` (a PDF body beside it as
//! `<name>.<step>.golden.pdf`). Without it, the steps are replayed against oxidestack/docx-to-pdf — in process, or
//! the running one `DOCX_TO_PDF_SCENARIOS_TARGET=http://127.0.0.1:<port>` names (`make image-scenarios`) — and the
//! answers compared with the goldens after normalisation: status, `Content-Type`, the headers the step names,
//! and the body — a PDF by PDF equivalence, never by bytes: a PDF's bytes change from
//! run to run (dates, IDs, font subsets) and from one LibreOffice version to another.

mod http;
mod scenario;
#[path = "../support/pdf.rs"]
#[allow(dead_code)]
mod test_pdf;

use std::path::{Path, PathBuf};
use std::time::Duration;

use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::PROGRAM_DIR;
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;

use http::{Response, Target};
use scenario::{GoldenFiles, Scenario, compare};

/// The image the goldens come from, as pinned in `oracle.env`.
fn oracle_image() -> String {
    include_str!("../../oracle.env")
        .lines()
        .find_map(|l| l.strip_prefix("ORACLE_IMAGE="))
        .expect("ORACLE_IMAGE in oracle.env")
        .trim()
        .to_owned()
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn scenario_dir() -> PathBuf {
    root().join("tests/scenarios")
}

/// Every scenario file, by name.
fn scenario_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.ends_with(".json") && !name.ends_with(".golden.json")
        })
        .collect();
    files.sort();
    files
}

/// Sends every step of `scenario` to `target` in order.
fn run(target: &Target, scenario: &Scenario) -> Result<Vec<Response>, String> {
    scenario
        .steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let request = step.request(&root())?;
            http::send(target, &request)
                .map_err(|e| format!("step {}: {} {}: {e}", i + 1, step.method, step.path))
        })
        .collect()
}

/// Every difference between the answers of `target` and the golden of the scenario at `path`, or why there is
/// nothing to compare with.
enum Replay {
    Agrees,
    Differs(Vec<String>),
}

fn replay(target: &Target, path: &Path) -> Result<Replay, String> {
    let scenario = Scenario::load(path)?;
    let files = GoldenFiles::of(path);
    let golden = files.load()?;
    if golden.results.len() != scenario.steps.len() {
        return Err(format!(
            "{} steps, {} golden answers: record it again (make record)",
            scenario.steps.len(),
            golden.results.len()
        ));
    }
    let actual = match run(target, &scenario) {
        Ok(actual) => actual,
        Err(e) => return Ok(Replay::Differs(vec![e])),
    };
    let mut differences = Vec::new();
    for (i, ((step, result), actual)) in scenario
        .steps
        .iter()
        .zip(&golden.results)
        .zip(&actual)
        .enumerate()
    {
        let expected = files.expected(result)?;
        differences.extend(
            compare(step, &expected, actual)
                .into_iter()
                .map(|d| format!("step {} ({} {}): {d}", i + 1, step.method, step.path)),
        );
    }
    Ok(if differences.is_empty() {
        Replay::Agrees
    } else {
        Replay::Differs(differences)
    })
}

fn record(target: &Target, path: &Path) -> Result<(), String> {
    let scenario = Scenario::load(path)?;
    let answers: Vec<Response> = run(target, &scenario)?
        .into_iter()
        .map(scenario::normalise)
        .collect();
    GoldenFiles::of(path).write(&oracle_image(), &answers)
}

/// Why oxidestack/docx-to-pdf cannot convert here, if it cannot: LibreOfficeKit runs on Linux with LibreOffice
/// installed (`tools/instance-test/`). `DOCX_TO_PDF_REQUIRE_LOK=1` (the container sets it) makes that a
/// failure.
fn no_libreoffice() -> Option<String> {
    let missing = if !cfg!(target_os = "linux") {
        Some("LibreOfficeKit runs on Linux only".to_string())
    } else if !Path::new(PROGRAM_DIR).join("libmergedlo.so").exists() {
        Some(format!("no {PROGRAM_DIR}/libmergedlo.so"))
    } else {
        None
    };
    let required = std::env::var_os("DOCX_TO_PDF_REQUIRE_LOK").is_some_and(|v| v == "1");
    match missing {
        Some(reason) if required => panic!("DOCX_TO_PDF_REQUIRE_LOK=1 but {reason}"),
        other => other,
    }
}

/// oxidestack/docx-to-pdf on a free local port, served by its own runtime for as long as this lives; with a pool of
/// one real instance where LibreOffice is installed, else answering 503 to every allowed document.
struct Ours {
    _runtime: tokio::runtime::Runtime,
    _profiles: tempfile::TempDir,
    authority: String,
}

impl Ours {
    fn start() -> Self {
        let profiles = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let authority = runtime.block_on(async {
            let telemetry = TelemetryBuilder::new()
                .with_otlp_from(|_| None)
                .build()
                .unwrap();
            let readiness = Readiness::new();
            readiness.mark_ready();
            let state = AppState::new(readiness.clone(), &telemetry);
            if no_libreoffice().is_none() {
                let launcher = Launcher::new(env!("CARGO_BIN_EXE_docx-to-pdf"), profiles.path());
                let settings = PoolSettings {
                    size: 1,
                    wait_timeout: Duration::from_secs(300),
                    max_waiting: 4,
                    start_timeout: Duration::from_secs(120),
                };
                let pool = Pool::start(launcher, settings, readiness, &telemetry.meter())
                    .await
                    .expect("the pool starts");
                state.set_pool(pool);
            }
            let app = router(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let _telemetry = telemetry;
                axum::serve(listener, app).await.unwrap();
            });
            addr.to_string()
        });
        Self {
            _runtime: runtime,
            _profiles: profiles,
            authority,
        }
    }

    fn target(&self) -> Target {
        Target::new(self.authority.clone(), Duration::from_secs(60))
    }
}

/// Where the replay goes: `DOCX_TO_PDF_SCENARIOS_TARGET=http://127.0.0.1:<port>` names a running service (the
/// image, `make image-scenarios`); without it, oxidestack/docx-to-pdf starts in process.
enum Where {
    InProcess,
    Url(Target),
}

impl Where {
    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        match lookup("DOCX_TO_PDF_SCENARIOS_TARGET").filter(|url| !url.is_empty()) {
            // An amd64 image emulated on an arm64 host converts slowly; a step gets five minutes, as recording does.
            Some(url) => Self::Url(Target::from_url(&url, Duration::from_secs(300))),
            None => Self::InProcess,
        }
    }
}

/// Records the goldens when a moalhaddar/docx-to-pdf is named, and otherwise replays every scenario against
/// oxidestack/docx-to-pdf. A scenario marked `pending` must still fail (it is red until what it names lands) and
/// fails the test the moment it passes, so the marker is removed by the change that makes it pass.
#[test]
fn scenarios() {
    let files = scenario_files(&scenario_dir());
    assert!(!files.is_empty(), "no scenarios in tests/scenarios");

    if let Ok(url) = std::env::var("DOCX_TO_PDF_RECORD_ORACLE") {
        let only: Option<Vec<String>> = std::env::var("DOCX_TO_PDF_RECORD_ONLY")
            .ok()
            .map(|s| s.split(',').map(|n| n.trim().to_owned()).collect());
        // moalhaddar/docx-to-pdf is amd64 only and converts slowly emulated on arm64; a step gets five minutes.
        let target = Target::from_url(&url, Duration::from_secs(300));
        let mut failures = Vec::new();
        for path in &files {
            let name = GoldenFiles::of(path).name;
            if only.as_ref().is_some_and(|o| !o.contains(&name)) {
                continue;
            }
            eprintln!("recording {name} from {url}");
            if let Err(e) = record(&target, path) {
                failures.push(format!("{name}: {e}"));
            }
        }
        assert!(
            failures.is_empty(),
            "recording failed:\n{}",
            failures.join("\n")
        );
        return;
    }

    // `DOCX_TO_PDF_SCENARIOS_STRICT=1` ignores `pending`: every scenario must pass.
    let strict = std::env::var_os("DOCX_TO_PDF_SCENARIOS_STRICT").is_some();
    // A running service converts or fails on its own: nothing is skipped against it.
    let (_ours, target, no_libreoffice) = match Where::from_lookup(|name| std::env::var(name).ok())
    {
        Where::Url(target) => {
            eprintln!("replaying against http://{}", target.authority);
            (None, target, None)
        }
        Where::InProcess => {
            let ours = Ours::start();
            let target = ours.target();
            (Some(ours), target, no_libreoffice())
        }
    };
    let mut failures = Vec::new();
    for path in &files {
        let name = GoldenFiles::of(path).name;
        let pending = match Scenario::load(path) {
            Ok(s) => s.pending.filter(|_| !strict),
            Err(e) => {
                failures.push(e);
                continue;
            }
        };
        // A scenario whose golden holds a PDF needs a conversion: where oxidestack/docx-to-pdf cannot convert, it
        // is skipped with the reason, as the LibreOfficeKit tests are; it runs in `tools/instance-test/`.
        if let (Some(reason), None) = (&no_libreoffice, &pending)
            && GoldenFiles::of(path).load().is_ok_and(|g| {
                g.results.iter().any(|r| {
                    r.headers
                        .iter()
                        .any(|h| h.starts_with("content-type: application/pdf"))
                })
            })
        {
            eprintln!("skip     {name}: needs a conversion and {reason}");
            continue;
        }
        match (replay(&target, path), pending) {
            (Err(e), _) => failures.push(format!("{name}: {e}")),
            (Ok(Replay::Agrees), None) => eprintln!("pass     {name}"),
            (Ok(Replay::Agrees), Some(pending)) => failures.push(format!(
                "{name}: passes now; remove `\"pending\": \"{pending}\"` from it"
            )),
            (Ok(Replay::Differs(d)), None) => {
                failures.push(format!("{name}:\n  {}", d.join("\n  ")));
            }
            (Ok(Replay::Differs(d)), Some(pending)) => {
                eprintln!("red      {name} (pending: {pending}): {}", d.join("; "));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

// The harness itself, without the moalhaddar/docx-to-pdf.

mod harness {
    use super::*;
    use scenario::Step;

    fn step(json: &str) -> Step {
        serde_json::from_str(json).unwrap()
    }

    fn answer(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Response {
        Response {
            interim: vec![],
            status,
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            body: body.to_vec(),
        }
    }

    fn preflight() -> Step {
        step(
            r#"{"method": "OPTIONS", "path": "/pdf", "compare": ["access-control-allow-origin", "access-control-allow-headers"]}"#,
        )
    }

    fn golden() -> Response {
        answer(
            200,
            &[
                ("vary", "Origin"),
                ("access-control-allow-origin", "*"),
                ("date", "Wed, 21 Oct 2026 09:13:40 GMT"),
                ("content-length", "0"),
            ],
            b"",
        )
    }

    /// `DOCX_TO_PDF_SCENARIOS_TARGET` sends the replay to a running service, such as the image, so the scenarios
    /// check what a deployment runs and not only the code in process.
    #[test]
    fn the_replay_goes_to_the_named_url_or_else_in_process() {
        let named = |name: &str| {
            (name == "DOCX_TO_PDF_SCENARIOS_TARGET").then(|| "http://127.0.0.1:48123/".to_owned())
        };
        match Where::from_lookup(named) {
            Where::Url(target) => {
                assert_eq!(target.authority, "127.0.0.1:48123");
                assert_eq!(target.timeout, Duration::from_secs(300));
            }
            Where::InProcess => panic!("a named target replays in process"),
        }
        assert!(matches!(Where::from_lookup(|_| None), Where::InProcess));
        assert!(matches!(
            Where::from_lookup(|_| Some(String::new())),
            Where::InProcess
        ));
    }

    #[test]
    fn an_identical_answer_agrees() {
        assert_eq!(
            compare(&preflight(), &golden(), &golden()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn date_and_headers_not_named_are_not_compared() {
        let mut ours = golden();
        ours.headers.retain(|(n, _)| n != "vary");
        ours.headers[1].1 = "Thu, 22 Oct 2026 10:00:00 GMT".into();
        ours.headers.push(("x-request-id".into(), "abc".into()));
        ours.headers.push(("traceparent".into(), "00-…".into()));
        ours.headers.push(("content-length".into(), "0".into()));
        assert_eq!(
            compare(&preflight(), &golden(), &ours),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_changed_status_fails() {
        let mut ours = golden();
        ours.status = 403;
        let d = compare(&preflight(), &golden(), &ours);
        assert!(d.len() == 1 && d[0].starts_with("status"), "{d:?}");
    }

    #[test]
    fn a_missing_interim_continue_fails() {
        let mut golden = golden();
        golden.interim = vec![100];
        let d = compare(&preflight(), &golden, &self::golden());
        assert!(d.len() == 1 && d[0].starts_with("interim"), "{d:?}");
    }

    #[test]
    fn a_changed_named_header_fails() {
        let mut ours = golden();
        ours.headers[1].1 = "http://a.example".into();
        let d = compare(&preflight(), &golden(), &ours);
        assert!(
            d.len() == 1 && d[0].contains("access-control-allow-origin"),
            "{d:?}"
        );
    }

    #[test]
    fn a_named_header_the_oracle_lacks_must_be_absent() {
        let mut ours = golden();
        ours.headers
            .push(("access-control-allow-headers".into(), "content-type".into()));
        let d = compare(&preflight(), &golden(), &ours);
        assert!(
            d.len() == 1 && d[0].contains("access-control-allow-headers"),
            "{d:?}"
        );
    }

    #[test]
    fn a_changed_content_type_fails_unnamed() {
        let golden = answer(200, &[("content-type", "text/plain")], b"ok");
        let ours = answer(200, &[("content-type", "application/json")], b"ok");
        let d = compare(&step(r#"{"method": "GET", "path": "/"}"#), &golden, &ours);
        assert!(d.len() == 1 && d[0].contains("content-type"), "{d:?}");
    }

    #[test]
    fn a_changed_text_body_fails() {
        let golden = answer(200, &[], b"ok");
        let ours = answer(200, &[], b"ko");
        let d = compare(&step(r#"{"method": "GET", "path": "/"}"#), &golden, &ours);
        assert!(d.len() == 1 && d[0].starts_with("body"), "{d:?}");
    }

    fn pdf_answer(text: &str) -> Response {
        let pdf = test_pdf::write(&[test_pdf::text_page(test_pdf::A4, &[(72.0, 760.0, text)])]);
        answer(200, &[("content-type", "application/pdf")], &pdf)
    }

    #[test]
    fn a_pdf_body_is_compared_by_equivalence_not_bytes() {
        let convert = step(r#"{"method": "POST", "path": "/pdf"}"#);
        let golden = pdf_answer("Hello, scenario");
        let mut ours = pdf_answer("Hello, scenario");
        // Other bytes, the same document: a comment before the end.
        ours.body
            .splice(9..9, b"% produced elsewhere\n".iter().copied());
        assert_ne!(golden.body, ours.body);
        assert_eq!(compare(&convert, &golden, &ours), Vec::<String>::new());

        let d = compare(&convert, &golden, &pdf_answer("Goodbye, scenario"));
        assert!(d.len() == 1 && d[0].contains("not equivalent"), "{d:?}");

        let d = compare(&convert, &golden, &answer(404, &[], b""));
        assert!(d.iter().any(|d| d.contains("not one")), "{d:?}");
    }

    /// Against moalhaddar/docx-to-pdf, pages, sizes and text gate; the rendering is reported only, since LibreOffice
    /// 26.8 lays out differently from 7.3 in ways only a person can rate.
    #[test]
    fn a_pdf_that_differs_only_in_rendering_agrees_and_one_with_another_page_size_does_not() {
        let convert = step(r#"{"method": "POST", "path": "/pdf"}"#);
        let golden = pdf_answer("Hello, scenario");
        let moved = test_pdf::write(&[test_pdf::text_page(
            test_pdf::A4,
            &[(300.0, 300.0, "Hello, scenario")],
        )]);
        let moved = answer(200, &[("content-type", "application/pdf")], &moved);
        assert_eq!(compare(&convert, &golden, &moved), Vec::<String>::new());

        let letter = test_pdf::write(&[test_pdf::text_page(
            test_pdf::LETTER,
            &[(72.0, 760.0, "Hello, scenario")],
        )]);
        let letter = answer(200, &[("content-type", "application/pdf")], &letter);
        let d = compare(&convert, &golden, &letter);
        assert!(d.len() == 1 && d[0].contains("PageSize"), "{d:?}");
    }

    /// Record from a server and replay against it, end to end: the paths oxidestack/docx-to-pdf has today.
    #[test]
    fn a_scenario_recorded_from_a_server_replays_against_it_and_a_tampered_golden_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probes.json");
        std::fs::write(
            &path,
            r#"{"description": "probes", "steps": [
                {"method": "GET", "path": "/livez", "compare": ["content-length"]},
                {"method": "GET", "path": "/readyz", "headers": [["Origin", "http://a.example"]]},
                {"method": "HEAD", "path": "/livez"}
            ]}"#,
        )
        .unwrap();

        let ours = Ours::start();
        record(&ours.target(), &path).unwrap();
        let files = GoldenFiles::of(&path);
        let golden = files.load().unwrap();
        assert_eq!(golden.recorded_from, oracle_image());
        assert_eq!(golden.results[0].status, 200);
        assert!(
            golden.results[0]
                .headers
                .contains(&"date: <date>".to_owned())
        );

        let fresh = Ours::start();
        assert!(matches!(replay(&fresh.target(), &path), Ok(Replay::Agrees)));

        let tamper = |edit: &dyn Fn(&mut serde_json::Value), expect: &str| {
            let mut json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(files.golden()).unwrap()).unwrap();
            let original = json.clone();
            edit(&mut json);
            std::fs::write(files.golden(), json.to_string()).unwrap();
            match replay(&fresh.target(), &path) {
                Ok(Replay::Differs(d)) => {
                    assert!(d.iter().any(|d| d.contains(expect)), "{expect}: {d:?}");
                }
                _ => panic!("a golden tampered for {expect} still agrees"),
            }
            std::fs::write(files.golden(), original.to_string()).unwrap();
        };
        tamper(&|g| g["results"][1]["status"] = 503.into(), "status");
        tamper(
            &|g| {
                let headers = g["results"][0]["headers"].as_array_mut().unwrap();
                for h in headers.iter_mut() {
                    if h.as_str().unwrap().starts_with("content-length") {
                        *h = "content-length: 3".into();
                    }
                }
            },
            "content-length",
        );
        tamper(&|g| g["results"][0]["body"]["text"] = "ko".into(), "body");

        // A header no step names changes nothing: Spring's `Vary`, our `x-request-id`.
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(files.golden()).unwrap()).unwrap();
        json["results"][1]["headers"]
            .as_array_mut()
            .unwrap()
            .push("vary: Origin".into());
        std::fs::write(files.golden(), json.to_string()).unwrap();
        assert!(matches!(replay(&fresh.target(), &path), Ok(Replay::Agrees)));
    }
}
