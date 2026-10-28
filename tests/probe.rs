//! The platform probe over HTTP, which measures the pod's own node for sizing. While `POST /probe` runs, the conversion paths answer 503 and
//! `/readyz` is not ready; afterwards the pool is whole and ready again, and the answer is `sizing.explain()`'s
//! shape, which `bench.py size --platform` takes. The conversion port has no `/probe`. The instance is a `sh`
//! stand-in that takes a tenth of a second per conversion; the real one is measured by `bench.py probe --url`.

use std::path::Path;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::probe::{self, ProbeState};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;
use http_body_util::BodyExt as _;
use serde_json::Value;
use tower::ServiceExt as _;

/// Ready at once; each conversion takes 0.1 s and writes a PDF header as its output.
fn launcher(profiles: &Path) -> Launcher {
    let script = r#"
        printf 'ready\t1\t%s\n' "$$"
        while read -r line; do
            sleep 0.1
            printf '%%PDF-1.4\n' > "$(printf '%s' "$line" | cut -f3)"
            printf 'ok\t1\t2\n'
        done
    "#;
    Launcher::with_args(
        "/bin/sh",
        ["-c".to_string(), script.to_string(), "sh".to_string()],
        profiles,
    )
}

const FORM: &str = "multipart/form-data; boundary=B0071";

/// A multipart body with one `document` file part per entry.
fn documents(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, content) in files {
        body.extend_from_slice(
            format!(
                "--B0071\r\nContent-Disposition: form-data; name=\"document\"; filename=\"{name}\"\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--B0071--\r\n");
    body
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, body.to_vec())
}

async fn readyz(app: &Router) -> StatusCode {
    call(app, Request::get("/readyz").body(Body::empty()).unwrap())
        .await
        .0
}

fn post(path: &str, body: Vec<u8>) -> Request<Body> {
    Request::post(path)
        .header("content-type", FORM)
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn while_the_probe_runs_conversions_get_503_and_afterwards_the_pool_is_whole() {
    let dir = tempfile::tempdir().unwrap();
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let state = AppState::new(readiness.clone(), &telemetry);
    let app = router(state.clone());
    let probe = probe::router(ProbeState::new(state.pool.clone(), &state.upload));
    let pool = Pool::start(
        launcher(dir.path()),
        PoolSettings {
            size: 2,
            wait_timeout: Duration::from_secs(10),
            max_waiting: 4,
            start_timeout: Duration::from_secs(10),
        },
        readiness,
        &telemetry.meter(),
    )
    .await
    .unwrap();
    state.set_pool(pool.clone());
    assert_eq!(readyz(&app).await, StatusCode::OK);

    let rtf: &[u8] = b"{\\rtf1 a probe document}";
    let running = tokio::spawn({
        let probe = probe.clone();
        let body = documents(&[("report-a.rtf", rtf), ("report-b.rtf", rtf)]);
        async move { call(&probe, post("/probe?rounds=2&busy=6&disk_mib=1", body)).await }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while readyz(&app).await == StatusCode::OK {
        assert!(Instant::now() < deadline, "/readyz stayed ready");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let (status, body) = call(&app, post("/pdf", documents(&[("a.rtf", rtf)]))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let problem: Value = serde_json::from_slice(&body).unwrap();
    assert!(
        problem["detail"]
            .as_str()
            .unwrap()
            .contains("measuring its platform"),
        "{problem}"
    );
    let (status, body) = call(&probe, post("/probe", documents(&[("a.rtf", rtf)]))).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "one probe at a time"
    );
    assert!(String::from_utf8_lossy(&body).contains("already running"));

    let (status, body) = running.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(readyz(&app).await, StatusCode::OK, "ready again");
    let counts = pool.counts();
    assert_eq!((counts.idle, counts.busy, counts.starting), (2, 0, 0));
    let (status, _) = call(&app, post("/pdf", documents(&[("a.rtf", rtf)]))).await;
    assert_eq!(status, StatusCode::OK, "converting again");

    let answer: Value = serde_json::from_slice(&body).unwrap();
    let keys: Vec<&str> = answer
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["platform", "parameters", "workload", "limits", "formulas"]
    );
    let s1 = answer["parameters"]["s1"]["value"].as_f64().unwrap();
    assert!((0.05..2.0).contains(&s1), "s1 {s1}");
    let contention = answer["parameters"]["contention"]["value"]
        .as_f64()
        .unwrap();
    assert!(contention > 0.5, "contention {contention}");
    assert!(answer["parameters"]["disk_mbps"]["value"].as_f64().unwrap() > 0.0);
    assert_eq!(answer["platform"]["cpus_probed"], 2);
    assert_eq!(answer["platform"]["pool"], 2);
    assert_eq!(answer["platform"]["conversions"]["alone"], 4);
    assert!(answer["platform"]["by_document_s"]["report-a"].is_number());
    // s1 and S_sat include the HTTP path's CPU, 0.045 × the conversion's s1; the raw values beside.
    let p = &answer["platform"];
    let s1_conversion = p["s1_conversion"].as_f64().expect("s1_conversion");
    let s_sat_conversion = p["s_sat_conversion"].as_f64().expect("s_sat_conversion");
    let http = p["http_cpu_s"].as_f64().expect("http_cpu_s");
    assert_eq!(p["http_cpu_share"], 0.045);
    assert!((http - 0.045 * s1_conversion).abs() <= 0.001, "{p}");
    assert!((s1 - 1.045 * s1_conversion).abs() <= 0.002, "s1 {s1}, {p}");
    let s_sat = p["s_sat"].as_f64().unwrap();
    assert!((s_sat - s_sat_conversion - http).abs() <= 0.002, "{p}");
    assert_eq!(answer["formulas"][0]["here"], "2 vCPU → POOL_SIZE = 2");
    assert_eq!(answer["workload"]["name"], "report");

    feeds_bench_size(&answer, dir.path());
}

/// The answer sizes memory from the pod's own `DOCX_TO_PDF_INSTANCE_HARD_MEMORY`, not the
/// default: a pod with a 1 GiB hard limit reads 1 GiB in the limits, the memory formula and the settings.
#[tokio::test]
async fn the_answer_sizes_memory_from_the_configured_hard_limit() {
    let dir = tempfile::tempdir().unwrap();
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let state = AppState::new(readiness.clone(), &telemetry);
    let probe = probe::router(ProbeState::new(state.pool.clone(), &state.upload));
    let pool = Pool::start_with_limits(
        launcher(dir.path()),
        PoolSettings {
            size: 1,
            wait_timeout: Duration::from_secs(10),
            max_waiting: 1,
            start_timeout: Duration::from_secs(10),
        },
        InstanceLimits {
            hard_memory: 1024 * 1024 * 1024,
            ..InstanceLimits::default()
        },
        readiness,
        &telemetry.meter(),
    )
    .await
    .unwrap();
    state.set_pool(pool);

    let rtf: &[u8] = b"{\\rtf1 a probe document}";
    let body = documents(&[("report-a.rtf", rtf)]);
    let (status, body) = call(&probe, post("/probe?rounds=1&busy=2&disk_mib=0", body)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let answer: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(answer["limits"]["hard_memory_mib"], 1024);
    let formula = |what: &str| -> String {
        answer["formulas"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["what"] == what)
            .unwrap_or_else(|| panic!("no formula {what:?}"))["here"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // One instance at the hard limit plus one poll's growth, over the service's 256 MiB.
    let memory = formula("memory limit (= request)");
    assert!(
        memory.starts_with("256 + 1 × (1024 + 256) = 1536 MiB"),
        "{memory}"
    );
    let settings = formula("timeouts and the queue");
    assert!(
        settings.contains("DOCX_TO_PDF_INSTANCE_HARD_MEMORY=1024MiB"),
        "{settings}"
    );
}

/// `bench.py size hardware --platform FILE` takes the answer unchanged; skipped without `python3`.
fn feeds_bench_size(answer: &Value, dir: &Path) {
    let file = dir.join("platform.json");
    std::fs::write(&file, answer.to_string()).unwrap();
    let bench = Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/bench/bench.py");
    let ran = std::process::Command::new("python3")
        .arg(&bench)
        .args([
            "size",
            "hardware",
            "--cpus",
            "2",
            "--memory",
            // Two instances at the 2 GiB hard limit at once, the worst case the pod must survive: 4.75 GiB.
            "5",
            "--platform",
        ])
        .arg(&file)
        .output();
    let Ok(output) = ran else {
        eprintln!("no python3: bench.py size --platform not tried");
        return;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("DOCX_TO_PDF_POOL_SIZE=2"), "{text}");
}

#[tokio::test]
async fn the_conversion_port_has_no_probe_and_the_probe_refuses_bad_requests() {
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let state = AppState::new(Readiness::new(), &telemetry);
    let app = router(state.clone());
    let probe = probe::router(ProbeState::new(state.pool.clone(), &state.upload));
    let rtf: &[u8] = b"{\\rtf1 x}";

    let (status, _) = call(&app, post("/probe", documents(&[("a.rtf", rtf)]))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(&probe, Request::get("/probe").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = call(&probe, post("/pdf", documents(&[("a.rtf", rtf)]))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &probe,
        post("/probe?rounds=x", documents(&[("a.rtf", rtf)])),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&probe, post("/probe", documents(&[]))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(&probe, post("/probe", documents(&[("a.txt", b"hello")]))).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let (status, _) = call(&probe, post("/probe", documents(&[("a.rtf", rtf)]))).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "no pool booted yet"
    );
}
