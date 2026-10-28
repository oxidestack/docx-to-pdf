//! The pool on the real instance: a pool of two LibreOfficeKit instances converts upstream's examples
//! concurrently, never two on one instance; an instance killed from outside is replaced and the conversion
//! that meets it succeeds by the retry; shutdown leaves no instance and no profile.
//!
//! Runs where LibreOffice is installed, as `tests/instance_lok.rs` does (the container of
//! `tools/instance-test/`): ignored off Linux, a printed skip on a Linux without LibreOffice unless
//! `DOCX_TO_PDF_REQUIRE_LOK=1`.

#[path = "../src/instance/group.rs"]
mod group;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use docx_to_pdf::instance::PROGRAM_DIR;
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::instance::protocol::Answer;
use docx_to_pdf::pool::{Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;
use group::group_alive;

fn skip_reason() -> Option<String> {
    let required = std::env::var_os("DOCX_TO_PDF_REQUIRE_LOK").is_some_and(|v| v == "1");
    let missing = if !cfg!(target_os = "linux") {
        Some("LibreOfficeKit runs on Linux only".to_string())
    } else if !Path::new(PROGRAM_DIR).join("libmergedlo.so").exists() {
        Some(format!("no {PROGRAM_DIR}/libmergedlo.so"))
    } else {
        None
    };
    match missing {
        Some(reason) if required => panic!("DOCX_TO_PDF_REQUIRE_LOK=1 but {reason}"),
        other => other,
    }
}

fn example(n: u32) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("upstream/docx-to-pdf/examples/ex{n}/input.docx"))
}

fn assert_pdf(path: &Path) {
    let pdf = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(pdf.starts_with(b"%PDF-"), "{} is not a PDF", path.display());
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "LibreOfficeKit runs on Linux only: tools/instance-test/run.sh"
)]
#[allow(
    unsafe_code,
    reason = "a signal to an instance, which std has no form of"
)]
async fn a_pool_of_real_instances_converts_concurrently_and_survives_a_killed_instance() {
    if let Some(reason) = skip_reason() {
        eprintln!("SKIPPED a_pool_of_real_instances_…: {reason}");
        return;
    }
    let profiles = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let pool = Pool::start(
        Launcher::new(env!("CARGO_BIN_EXE_docx-to-pdf"), profiles.path()),
        PoolSettings {
            size: 2,
            wait_timeout: Duration::from_secs(120),
            max_waiting: 16,
            start_timeout: Duration::from_secs(120),
        },
        readiness.clone(),
        &telemetry.meter(),
    )
    .await
    .unwrap();
    assert!(readiness.is_ready());

    // Eight conversions of the four examples at once on two instances.
    let in_use = Arc::new(Mutex::new(HashSet::new()));
    let mut tasks = Vec::new();
    for n in 0..8u32 {
        let (pool, in_use) = (pool.clone(), in_use.clone());
        let output = work.path().join(format!("out-{n}.pdf"));
        tasks.push(tokio::spawn(async move {
            let mut lease = pool.acquire().await.unwrap();
            let pid = lease.pid();
            assert!(in_use.lock().unwrap().insert(pid), "{pid} lent twice");
            let answer = lease.convert(&example(n % 4 + 1), &output).await.unwrap();
            in_use.lock().unwrap().remove(&pid);
            assert!(matches!(answer, Answer::Converted { .. }), "{answer:?}");
            assert_pdf(&output);
            pid
        }));
    }
    let mut pids = HashSet::new();
    for task in tasks {
        pids.insert(task.await.unwrap());
    }
    assert_eq!(pids.len(), 2, "both instances served: {pids:?}");

    // Kill a lent instance from outside and give it back; it is the next one lent (the idle list is a
    // stack), so the next conversion meets it dead and succeeds by the retry on the other.
    let lease = pool.acquire().await.unwrap();
    let killed = lease.pid();
    // SAFETY: SIGKILL to the process group of an instance this test's pool started.
    unsafe { libc::kill(-killed.cast_signed(), libc::SIGKILL) };
    drop(lease);
    let output = work.path().join("retried.pdf");
    let conversion = pool.convert(&example(1), &output).await.unwrap();
    assert!(conversion.retried, "the answer came from the retry");
    assert!(matches!(conversion.answer, Answer::Converted { .. }));
    assert_pdf(&output);

    // The replacement starts in the background (the retry ran on the other instance); its start is counted
    // and it joins the idle instances once it is ready.
    let crash_counted = || {
        telemetry.prometheus_text().lines().any(|l| {
            l.starts_with("docx_to_pdf_instance_starts_total") && l.contains("reason=\"crash\"")
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !(crash_counted() && pool.counts().idle == 2) {
        assert!(
            std::time::Instant::now() < deadline,
            "{}",
            telemetry.prometheus_text()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    pool.shutdown(Duration::from_secs(10)).await;
    assert!(!readiness.is_ready());
    for pid in &pids {
        assert!(!group_alive(*pid), "instance {pid} is gone");
    }
    // The replacement may still be starting; it stops itself once it finds the pool closed.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::fs::read_dir(profiles.path()).unwrap().count() > 0 {
        assert!(std::time::Instant::now() < deadline, "profiles left behind");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
