//! A JPEG is downscaled in memory bounded by its MCU rows, never by its bitmap. A counting global
//! allocator measures the peak heap of `replace_oversized` while it streams a 1-gigapixel baseline JPEG (gray
//! 32 768², a 1 GiB bitmap) out of a DOCX, decodes it at half size by DCT scaling and re-encodes it at 16 384² under
//! the default 256 MiB threshold. Holding the bitmap would take 1 GiB, the halved one 256 MiB; an MCU row is a few
//! hundred KiB. The time is printed too: on an Apple M-series core, 8.5 s optimised (`cargo test --release`), about
//! 100 s in the unoptimised test build, like the PNG's test (`tests/pictures_memory.rs`).
//!
//! The only test in its binary, so no other test's allocations land in the count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

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

/// Bits MSB first, `0xFF` stuffed.
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, bits: u32, count: u32) {
        for i in (0..count).rev() {
            self.acc = self.acc << 1 | (bits >> i & 1);
            self.n += 1;
            if self.n == 8 {
                let byte = self.acc.to_le_bytes()[0];
                self.out.push(byte);
                if byte == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }
}

/// Gray 32 768² baseline, written by hand: a quantiser of 1 everywhere, Annex K's luminance Huffman tables, each
/// block flat (its DC, then end-of-block) and a new gray every 64 rows, so it codes to a few MiB.
fn gigapixel_jpeg() -> Vec<u8> {
    let dc_counts = [0u8, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    // Canonical codes for DC categories 0..=11 from the counts.
    let mut dc_codes = Vec::new();
    let mut code = 0u32;
    for (length, &count) in (1u32..).zip(dc_counts.iter()) {
        for _ in 0..count {
            dc_codes.push((code, length));
            code += 1;
        }
        code <<= 1;
    }
    let mut out = vec![0xFF, 0xD8];
    let mut segment = |marker: u8, data: &[u8]| {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&(u16::try_from(data.len()).unwrap() + 2).to_be_bytes());
        out.extend_from_slice(data);
    };
    let mut dqt = vec![0u8];
    dqt.extend_from_slice(&[1; 64]);
    segment(0xDB, &dqt);
    let mut sof = vec![8];
    sof.extend_from_slice(&u16::try_from(SIDE).unwrap().to_be_bytes());
    sof.extend_from_slice(&u16::try_from(SIDE).unwrap().to_be_bytes());
    sof.extend_from_slice(&[1, 1, 0x11, 0]);
    segment(0xC0, &sof);
    let mut dht = vec![0x00];
    dht.extend_from_slice(&dc_counts);
    dht.extend(0..12u8);
    // An AC table holding only end-of-block, coded `0`.
    dht.push(0x10);
    dht.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    dht.push(0x00);
    segment(0xC4, &dht);
    segment(0xDA, &[1, 1, 0x00, 0, 63, 0]);

    let mut bits = Bits { out, acc: 0, n: 0 };
    let mut predictor = 0i32;
    let blocks = SIDE / 8;
    for by in 0..blocks {
        let level = i32::try_from((by / 8) * 37 % 256).unwrap(); // a new gray every 64 rows
        let dc = 8 * (level - 128);
        for _ in 0..blocks {
            let diff = dc - predictor;
            predictor = dc;
            let size = 32 - diff.unsigned_abs().leading_zeros();
            let (code, length) = dc_codes[size as usize];
            bits.put(code, length);
            let value = if diff < 0 {
                (diff - 1).cast_unsigned()
            } else {
                diff.cast_unsigned()
            };
            bits.put(value & ((1 << size) - 1), size);
            bits.put(0, 1); // end of block
        }
    }
    if bits.n > 0 {
        let pad = 8 - bits.n;
        bits.put((1 << pad) - 1, pad);
    }
    let mut out = bits.out;
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
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
fn a_gigapixel_jpeg_is_downscaled_in_a_few_mib() {
    let document = docx(&gigapixel_jpeg());
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
            width: SIDE / 2,
            height: SIDE / 2
        })
    );
    eprintln!(
        "peak heap while downscaling a {} MiB document holding a 1 GiB JPEG bitmap: {} KiB, in {:.1} s ({})",
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
