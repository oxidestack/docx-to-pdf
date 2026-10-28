//! A PNG is downscaled in memory bounded by its rows, never by its bitmap. A counting global allocator
//! measures the peak heap of `replace_oversized` while it streams a 1-gigapixel PNG (gray 32 768², a 1 GiB bitmap)
//! out of a DOCX and halves it to 16 384² under the default 256 MiB threshold. Holding the bitmap would take 1 GiB;
//! a row is 32 KiB.
//!
//! The only test in its binary, so no other test's allocations land in the count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use docx_to_pdf::pictures::{Outcome, downscale::Downscaled, replace_oversized};

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

const SIDE: u32 = 32768;
const MIB: usize = 1024 * 1024;

/// Gray 32 768², streamed out a row at a time: each row one value, so it compresses to a few MiB.
fn gigapixel_png() -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, SIDE, SIDE);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_compression(png::Compression::Balanced);
    let mut header = encoder.write_header().unwrap();
    let mut stream = header.stream_writer().unwrap();
    let mut row = vec![0u8; SIDE as usize];
    for y in 0..SIDE {
        row.fill((y / 64).to_le_bytes()[0]);
        stream.write_all(&row).unwrap();
    }
    stream.finish().unwrap();
    header.finish().unwrap();
    out
}

fn docx(picture: &[u8]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer.start_file("[Content_Types].xml", stored).unwrap();
    writer.write_all(b"<Types/>").unwrap();
    writer.start_file("word/media/image1.png", stored).unwrap();
    writer.write_all(picture).unwrap();
    writer.finish().unwrap().into_inner()
}

#[test]
fn a_gigapixel_png_is_downscaled_in_a_few_mib() {
    let document = docx(&gigapixel_png());
    let scratch = tempfile::tempdir().unwrap();
    let mut out = tempfile::tempfile().unwrap();

    let baseline = CURRENT.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let changed = replace_oversized(
        &mut Cursor::new(&document),
        &mut out,
        docx_to_pdf::config::DEFAULT_PICTURE_MAX_BITMAP,
        scratch.path(),
    )
    .unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - baseline;

    assert_eq!(
        changed[0].outcome,
        Outcome::Downscaled(Downscaled {
            factor: 2,
            width: SIDE / 2,
            height: SIDE / 2
        })
    );
    eprintln!(
        "peak heap while downscaling a {} MiB document holding a 1 GiB bitmap: {} KiB",
        document.len() / MIB,
        peak / 1024
    );
    assert!(
        peak < 8 * MIB,
        "the downscale's peak heap is {peak} bytes: bounded by rows, it stays in the low MiB"
    );
}
