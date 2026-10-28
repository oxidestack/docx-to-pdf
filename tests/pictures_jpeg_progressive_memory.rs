//! A progressive JPEG is downscaled in memory bounded by its MCU rows, never by its bitmap or its
//! coefficients. A counting global allocator measures the peak heap of `replace_oversized` while it reads a
//! 1-gigapixel progressive gray JPEG (32 768², a 1 GiB bitmap; four scans, successive approximation on DC and on
//! the AC band 1..5) out of a DOCX, indexes its scans into a scratch file, decodes them all a row at a time at half size and re-encodes
//! the picture at 16 384² under the default 256 MiB threshold. Holding the coefficients would take 2 GiB, the
//! halved bitmap 256 MiB, the scale's 4 × 4 corners alone 512 MiB. The time is printed too: on an Apple M-series
//! core, 9.5 s optimised (`cargo test --release`), 87 s in the unoptimised test build, which then spends as long
//! again writing the picture. The narrow band keeps that short (libjpeg's six-scan script over 1..63 took 440 s in
//! all there, at the same peak); the bound does not depend on it.
//!
//! The only test in its binary, so no other test's allocations land in the count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use docx_to_pdf::pictures::{Outcome, downscale::Downscaled, replace_oversized};

#[path = "../src/pictures/jpeg/testenc.rs"]
mod testenc;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
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
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc` is passed on unchanged.
        unsafe { System.dealloc(pointer, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: the caller's contract for `realloc` is passed on unchanged.
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            if size >= layout.size() {
                let now = CURRENT.fetch_add(size - layout.size(), Ordering::Relaxed)
                    + (size - layout.size());
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SIDE: usize = 32768;
const MIB: usize = 1024 * 1024;

/// A block of the picture: a new gray every 64 rows, and in one block column of 16 a few AC coefficients large
/// enough for both AC scans to have bits to code and refine.
fn block(_: usize, bx: usize, by: usize) -> [i16; 64] {
    let mut b = [0i16; 64];
    b[0] = (i16::try_from((by / 8) * 37 % 256).unwrap() - 128) * 8 / 4;
    if bx.is_multiple_of(16) {
        b[1] = 7;
        b[2] = -3;
        b[4] = 1;
        b[5] = -6;
    }
    b
}

/// DC down a bit, the AC band 1..5 down a bit, both refined.
fn script() -> Vec<testenc::Scan> {
    let scan = |ss, se, ah, al| testenc::Scan {
        comps: vec![0],
        ss,
        se,
        ah,
        al,
    };
    vec![
        scan(0, 0, 0, 1),
        scan(1, 5, 0, 1),
        scan(1, 5, 1, 0),
        scan(0, 0, 1, 0),
    ]
}

fn docx(picture: &[u8]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("[Content_Types].xml", stored).unwrap();
    writer.write_all(b"<Types/>").unwrap();
    writer.start_file("word/media/image1.jpeg", stored).unwrap();
    writer.write_all(picture).unwrap();
    writer.finish().unwrap().into_inner()
}

#[test]
fn a_gigapixel_progressive_jpeg_is_downscaled_in_a_few_mib() {
    let frame = testenc::Frame {
        progressive: true,
        // Quantisers of 4: the DC above is a level of `(by / 8) * 37 % 256`.
        quant: vec![[4; 64]],
        ..testenc::frame(SIDE, SIDE, None, 90)
    };
    let picture = testenc::write(&frame, &script(), &[], &block);
    let document = docx(&picture);
    drop(picture);
    let scratch = tempfile::tempdir().unwrap();
    let mut out = tempfile::tempfile().unwrap();

    let baseline = CURRENT.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let started = Instant::now();
    let changed = replace_oversized(
        &mut Cursor::new(&document),
        &mut out,
        docx_to_pdf::config::DEFAULT_PICTURE_MAX_BITMAP,
        scratch.path(),
    )
    .unwrap();
    let took = started.elapsed();
    let peak = PEAK.load(Ordering::Relaxed) - baseline;

    assert_eq!(
        changed[0].outcome,
        Outcome::Downscaled(Downscaled {
            factor: 2,
            width: u32::try_from(SIDE / 2).unwrap(),
            height: u32::try_from(SIDE / 2).unwrap()
        })
    );
    eprintln!(
        "peak heap while downscaling a {} MiB document holding a 1 GiB progressive JPEG bitmap: {} KiB, in {:.1} s ({})",
        document.len() / MIB,
        peak / 1024,
        took.as_secs_f64(),
        if cfg!(debug_assertions) {
            "an unoptimised test build"
        } else {
            "an optimised build"
        }
    );
    assert!(
        peak < 8 * MIB,
        "the downscale's peak heap is {peak} bytes: bounded by MCU rows, it stays in the low MiB"
    );
}
