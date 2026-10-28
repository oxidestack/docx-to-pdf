//! One real instance (this binary in instance mode, hosting LibreOfficeKit) is
//! started, converts a document, is killed from outside, and its death is observed.
//!
//! LibreOfficeKit does not run on macOS, so this runs where LibreOffice is installed: in the
//! container of `tools/instance-test/` (`tools/instance-test/run.sh`). Off Linux the tests are ignored, with the
//! reason in `cargo test`'s output; on a Linux without LibreOffice they print a skip and pass, unless
//! `DOCX_TO_PDF_REQUIRE_LOK=1` (the container sets it) makes a missing LibreOffice a failure.

#[path = "../src/instance/group.rs"]
mod group;

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use docx_to_pdf::instance::PROGRAM_DIR;
use docx_to_pdf::instance::driver::{ConvertError, Instance, Launcher};
use docx_to_pdf::instance::protocol::Answer;
use group::group_alive;

/// Why this cannot run here, if it cannot.
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

fn launcher(profiles: &Path) -> Launcher {
    Launcher::new(env!("CARGO_BIN_EXE_docx-to-pdf"), profiles)
}

fn assert_pdf(path: &Path) {
    let pdf = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(pdf.starts_with(b"%PDF-"), "{} is not a PDF", path.display());
    assert!(
        pdf.windows(5).rev().take(64).any(|w| w == b"%%EOF"),
        "{} has no %%EOF",
        path.display()
    );
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
async fn an_instance_converts_then_dies_when_killed_from_outside() {
    if let Some(reason) = skip_reason() {
        eprintln!("SKIPPED an_instance_converts_then_dies_when_killed_from_outside: {reason}");
        return;
    }
    let profiles = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();

    // Start: the ready line comes after LOK's init.
    let mut instance = Instance::start(&launcher(profiles.path())).await.unwrap();
    let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
    eprintln!(
        "instance {pid}: LOK init {} ms, spawn to ready {:?}",
        instance.ready().init_ms,
        instance.spawn_to_ready()
    );
    let xcu = std::fs::read_to_string(profile.join("user/registrymodifications.xcu")).unwrap();
    assert!(
        xcu.contains(r#"<prop oor:name="UseLocking" oor:op="fuse"><value>false</value></prop>"#)
    );

    // Convert one document; the input is not touched and no lock file is left beside it.
    let input = work.path().join("input.docx");
    std::fs::copy(example(1), &input).unwrap();
    let before = std::fs::read(&input).unwrap();
    let output = work.path().join("output.pdf");
    let answer = instance.convert(&input, &output).await.unwrap();
    assert!(matches!(answer, Answer::Converted { .. }), "{answer:?}");
    assert_pdf(&output);
    assert_eq!(std::fs::read(&input).unwrap(), before, "the input changed");
    let mut names: Vec<String> = std::fs::read_dir(work.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["input.docx", "output.pdf"]);

    // A truncated DOCX is a typed failure, and the instance stays usable.
    let truncated = work.path().join("truncated.docx");
    std::fs::write(&truncated, &before[..before.len() / 2]).unwrap();
    let answer = instance
        .convert(&truncated, &work.path().join("truncated.pdf"))
        .await
        .unwrap();
    assert_eq!(
        answer,
        Answer::NotLoaded("loadComponentFromURL returned an empty reference".into())
    );
    let second = work.path().join("second.pdf");
    std::fs::copy(example(3), work.path().join("ex3.docx")).unwrap();
    let answer = instance
        .convert(&work.path().join("ex3.docx"), &second)
        .await
        .unwrap();
    assert!(matches!(answer, Answer::Converted { .. }), "{answer:?}");
    assert_pdf(&second);

    // Kill the process from outside; the next conversion observes the death.
    // SAFETY: kill(2) on the instance's own pid.
    assert_eq!(unsafe { libc::kill(pid.cast_signed(), libc::SIGKILL) }, 0);
    let died = tokio::time::timeout(
        Duration::from_secs(30),
        instance.convert(&input, &work.path().join("after-kill.pdf")),
    )
    .await
    .expect("the death was not observed");
    match died {
        Err(ConvertError::Dead(Some(status))) => assert_eq!(status.signal(), Some(libc::SIGKILL)),
        other => panic!("expected a dead instance, got {other:?}"),
    }
    assert!(!instance.is_usable());
    assert!(!group_alive(pid), "something of group {pid} is left");
    assert!(!profile.exists(), "the killed instance's profile is left");

    // A fresh instance starts and converts: what a replacement needs.
    let mut fresh = Instance::start(&launcher(profiles.path())).await.unwrap();
    let answer = fresh
        .convert(&input, &work.path().join("fresh.pdf"))
        .await
        .unwrap();
    assert!(matches!(answer, Answer::Converted { .. }), "{answer:?}");
    let fresh_pid = fresh.pid();
    let status = fresh.stop().await.unwrap();
    assert!(status.success(), "{status}");
    assert!(!group_alive(fresh_pid));
    assert_eq!(
        std::fs::read_dir(profiles.path()).unwrap().count(),
        0,
        "a profile is left"
    );
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "LibreOfficeKit runs on Linux only: tools/instance-test/run.sh"
)]
async fn an_instance_killed_with_its_group_mid_conversion_leaves_nothing() {
    if let Some(reason) = skip_reason() {
        eprintln!(
            "SKIPPED an_instance_killed_with_its_group_mid_conversion_leaves_nothing: {reason}"
        );
        return;
    }
    let profiles = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("input.docx");
    std::fs::copy(example(3), &input).unwrap();

    let mut instance = Instance::start(&launcher(profiles.path())).await.unwrap();
    let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
    // Interrupted the way a timeout interrupts it (the first load alone takes far longer than 50 ms), then
    // killed with its group: the conversion timeout's path.
    let interrupted = tokio::time::timeout(
        Duration::from_millis(50),
        instance.convert(&input, &work.path().join("output.pdf")),
    )
    .await;
    assert!(interrupted.is_err(), "the conversion finished within 50 ms");
    assert!(!instance.is_usable());
    let status = instance.kill().await;
    assert_eq!(status.and_then(|s| s.signal()), Some(libc::SIGKILL));
    assert!(!group_alive(pid), "something of group {pid} is left");
    assert!(!profile.exists(), "the killed instance's profile is left");
}

/// A document with a comment loads. Under LibreOfficeKit the load builds a visible view, and Writer's
/// comment window reads `soffice.cfg/modules/swriter/ui/annotation.ui`, which `libreoffice-uiconfig-writer` ships and
/// `libreoffice-writer-nogui` does not pull in; without it every document with a comment answered `NotLoaded`.
#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "LibreOfficeKit runs on Linux only: tools/instance-test/run.sh"
)]
async fn a_document_with_a_comment_converts() {
    if let Some(reason) = skip_reason() {
        eprintln!("SKIPPED a_document_with_a_comment_converts: {reason}");
        return;
    }
    let profiles = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let input = work.path().join("input.docx");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scenarios/fixtures/comment.docx"),
        &input,
    )
    .unwrap();
    let output = work.path().join("output.pdf");

    let mut instance = Instance::start(&launcher(profiles.path())).await.unwrap();
    let answer = instance.convert(&input, &output).await.unwrap();
    assert!(matches!(answer, Answer::Converted { .. }), "{answer:?}");
    assert_pdf(&output);
    instance.stop().await.unwrap();
}
