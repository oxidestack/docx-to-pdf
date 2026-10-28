//! `/readyz` follows the pool, so a load balancer sends no request to a pod that cannot convert: 503 before the pool has started, 200 once it has,
//! 503 while no instance is alive (the only one died and its replacement is starting), 200 again once the
//! replacement is ready, and 503 for good once the pool shuts down. The instance is a `sh` stand-in.

use std::path::Path;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;
use tower::ServiceExt as _;

/// Ready at once, or after a second while `<markers>/slow-start` exists; answers `ok` to every request.
fn launcher(markers: &Path, profiles: &Path) -> Launcher {
    let script = format!(
        r#"
        if [ -f '{}/slow-start' ]; then sleep 1; fi
        printf 'ready\t1\t%s\n' "$$"
        while read -r line; do : > "$(printf '%s' "$line" | cut -f3)"; printf 'ok\t1\t2\n'; done
        "#,
        markers.display()
    );
    Launcher::with_args(
        "/bin/sh",
        ["-c".to_string(), script, "sh".to_string()],
        profiles,
    )
}

async fn readyz(app: &axum::Router) -> StatusCode {
    let request = Request::get("/readyz").body(Body::empty()).unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

async fn eventually_readyz(app: &axum::Router, status: StatusCode) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while readyz(app).await != status {
        assert!(Instant::now() < deadline, "/readyz never answered {status}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
#[allow(
    unsafe_code,
    reason = "a signal to an instance, which std has no form of"
)]
async fn readyz_is_503_until_the_pool_starts_then_follows_its_instances() {
    let dir = tempfile::tempdir().unwrap();
    let (markers, profiles) = (dir.path().join("markers"), dir.path().join("profiles"));
    std::fs::create_dir(&markers).unwrap();
    std::fs::create_dir(&profiles).unwrap();
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let app = router(AppState::new(readiness.clone(), &telemetry));

    assert_eq!(readyz(&app).await, StatusCode::SERVICE_UNAVAILABLE);
    let pool = Pool::start(
        launcher(&markers, &profiles),
        PoolSettings {
            size: 1,
            wait_timeout: Duration::from_secs(10),
            max_waiting: 4,
            start_timeout: Duration::from_secs(10),
        },
        readiness,
        &telemetry.meter(),
    )
    .await
    .unwrap();
    assert_eq!(readyz(&app).await, StatusCode::OK);

    // The only instance dies; the conversion that meets it waits for the slow replacement.
    std::fs::write(markers.join("slow-start"), "").unwrap();
    let pid = pool.acquire().await.unwrap().pid();
    // SAFETY: SIGKILL to the process group of the instance this test's pool started.
    unsafe { libc::kill(-pid.cast_signed(), libc::SIGKILL) };
    let converting = tokio::spawn({
        let pool = pool.clone();
        let (input, output) = (dir.path().join("in.docx"), dir.path().join("out.pdf"));
        std::fs::write(&input, "x").unwrap();
        async move { pool.convert(&input, &output).await }
    });
    eventually_readyz(&app, StatusCode::SERVICE_UNAVAILABLE).await;
    eventually_readyz(&app, StatusCode::OK).await;
    assert!(converting.await.unwrap().unwrap().retried);

    pool.shutdown(Duration::from_secs(5)).await;
    assert_eq!(readyz(&app).await, StatusCode::SERVICE_UNAVAILABLE);
}
