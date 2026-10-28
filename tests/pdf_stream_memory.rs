//! The PDF streamed into the answer, at the HTTP seam: the answer's memory is a buffer, not the PDF. This binary holds one test and
//! counts every heap allocation of the process (a global allocator), so the peak heap growth while a PDF is
//! answered can be compared across PDF sizes. The stand-in instance (`sh` speaking the instance protocol) is a process of
//! its own: its memory is not counted, only the service's.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use docx_to_pdf::app::{AppState, router};
use docx_to_pdf::instance::driver::Launcher;
use docx_to_pdf::pool::{InstanceLimits, Pool, PoolSettings};
use docx_to_pdf::readiness::Readiness;
use docx_to_pdf::telemetry::TelemetryBuilder;
use docx_to_pdf::upload::Upload;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The system allocator, counting the bytes in use and their peak since the last [`reset_peak`].
struct Counting;

static IN_USE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

#[allow(
    unsafe_code,
    reason = "a counting global allocator, forwarding to `System`"
)]
// SAFETY: every call is forwarded to `System` unchanged; the counters only observe sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc` is passed on unchanged.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc` is passed on unchanged.
        unsafe { System.dealloc(pointer, layout) };
        IN_USE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: the caller's contract for `realloc` is passed on unchanged.
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            IN_USE.fetch_sub(layout.size(), Ordering::Relaxed);
            grew(size);
        }
        moved
    }
}

fn grew(size: usize) {
    let now = IN_USE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Starts a new peak at the bytes in use now, and returns them.
fn reset_peak() -> usize {
    let now = IN_USE.load(Ordering::Relaxed);
    PEAK.store(now, Ordering::Relaxed);
    now
}

const SCRIPT: &str = r#"
    test -d "$1" || exit 9
    printf 'ready\t1\t%s\n' "$$"
    tab="$(printf '\t')"
    while IFS="$tab" read -r cmd input output; do
        n="$(sed -n 's/.*LARGE \([0-9]*\).*/\1/p' "$input")"
        { printf '%%PDF-1.4\n'; head -c "$n" /dev/zero; } > "$output"
        printf 'ok\t1\t1\n'
    done
"#;

fn request(size: usize) -> Request<Body> {
    let mut body =
        b"--B0039\r\nContent-Disposition: form-data; name=\"document\"; filename=\"a.rtf\"\r\n\r\n"
            .to_vec();
    body.extend_from_slice(format!("{{\\rtf1 LARGE {size}}}").as_bytes());
    body.extend_from_slice(b"\r\n--B0039--\r\n");
    Request::builder()
        .method(Method::POST)
        .uri("/pdf")
        .header("content-type", "multipart/form-data; boundary=B0039")
        .body(Body::from(body))
        .unwrap()
}

/// The peak heap growth, in bytes, while a PDF of `size` bytes after its header is converted and its body
/// received frame by frame, each frame dropped once counted.
async fn peak_growth(app: &axum::Router, size: usize) -> usize {
    let base = reset_peak();
    let response = app.clone().oneshot(request(size)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut received = 0;
    while let Some(frame) = body.frame().await {
        received += frame.unwrap().into_data().unwrap().len();
    }
    assert_eq!(received, size + b"%PDF-1.4\n".len());
    PEAK.load(Ordering::Relaxed).saturating_sub(base)
}

#[tokio::test]
async fn the_answer_holds_a_buffer_not_the_pdf_whatever_its_size() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["profiles", "spool"] {
        std::fs::create_dir(dir.path().join(sub)).unwrap();
    }
    let telemetry = TelemetryBuilder::new().build().unwrap();
    let readiness = Readiness::new();
    let launcher = Launcher::with_args(
        "/bin/sh",
        ["-c".to_string(), SCRIPT.to_string(), "sh".to_string()],
        dir.path().join("profiles"),
    );
    let settings = PoolSettings {
        size: 1,
        wait_timeout: Duration::from_secs(10),
        max_waiting: 4,
        start_timeout: Duration::from_secs(10),
    };
    let pool = Pool::start_with_limits(
        launcher,
        settings,
        InstanceLimits::default(),
        readiness.clone(),
        &telemetry.meter(),
    )
    .await
    .unwrap();
    let mut upload = Upload::new(1024 * 1024);
    upload.spool_root = dir.path().join("spool");
    let state = AppState::new(readiness, &telemetry).with_upload(upload);
    state.set_pool(pool);
    let app = router(state);

    // A first conversion warms up whatever is allocated once (instruments, lazy statics).
    peak_growth(&app, 1024).await;
    let small = peak_growth(&app, 8 * 1024 * 1024).await;
    let large = peak_growth(&app, 48 * 1024 * 1024).await;

    const BOUND: usize = 2 * 1024 * 1024;
    assert!(small < BOUND, "an 8 MiB PDF grew the heap by {small} bytes");
    assert!(large < BOUND, "a 48 MiB PDF grew the heap by {large} bytes");
    assert!(
        large < small + 512 * 1024,
        "the growth does not follow the PDF's size: {small} bytes for 8 MiB, {large} for 48 MiB"
    );
}
