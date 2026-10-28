//! A JPEG's `APPn` and `COM` segments, which the downscale copies into its output, cost memory bounded by
//! what the decoder keeps, not by what the file carries. A counting global allocator measures the peak heap of
//! `jpeg::downscale` on a small picture that carries the largest ICC profile there can be (255 chunks of 65 533
//! bytes, about 16 MiB, kept whole) and 64 MiB of comments, XMP and Photoshop segments (kept up to
//! `KEPT_OTHER_MAX`, the rest skipped unread). Keeping everything would hold over 80 MiB.
//!
//! The only test in its binary, so no other test's allocations land in the count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use docx_to_pdf::pictures::jpeg::{ICC_CHUNKS_MAX, KEPT_OTHER_MAX, downscale};

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

const MIB: usize = 1024 * 1024;
/// The largest payload a segment can have.
const FULL: usize = 65_533;
const SIDE: u16 = 1024;

/// A gray `SIDE`² baseline JPEG, mid-gray: quantisers of 1, a DC table whose only code `0` is category 0, an AC table
/// whose only code `0` is end-of-block, so every block is two zero bits. `segments` go right after `SOI`.
fn jpeg(segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    let mut segment = |marker: u8, data: &[u8]| {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&(u16::try_from(data.len()).unwrap() + 2).to_be_bytes());
        out.extend_from_slice(data);
    };
    for (marker, data) in segments {
        segment(*marker, data);
    }
    segment(0xDB, &[[0u8].as_slice(), &[1; 64]].concat());
    let mut frame = vec![8];
    frame.extend_from_slice(&SIDE.to_be_bytes());
    frame.extend_from_slice(&SIDE.to_be_bytes());
    frame.extend_from_slice(&[1, 1, 0x11, 0]);
    segment(0xC0, &frame);
    let mut one = [0u8; 16];
    one[0] = 1;
    segment(0xC4, &[[0x00].as_slice(), &one, &[0]].concat());
    segment(0xC4, &[[0x10].as_slice(), &one, &[0]].concat());
    segment(0xDA, &[1, 1, 0x00, 0, 63, 0]);
    let blocks = usize::from(SIDE / 8).pow(2);
    out.resize(out.len() + blocks * 2 / 8, 0);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// An output that only counts its bytes, so it allocates nothing.
struct Written(usize);

impl std::io::Write for Written {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn payload(signature: &[u8], fill: u8) -> Vec<u8> {
    let mut data = signature.to_vec();
    data.resize(FULL, fill);
    data
}

#[test]
fn a_jpegs_kept_segments_cost_their_bound_not_their_size() {
    let mut segments = vec![(0xE0, b"JFIF\0\x01\x02\x01\x01\x2c\x01\x2c\0\0".to_vec())];
    for i in 1..=ICC_CHUNKS_MAX {
        let mut chunk = payload(b"ICC_PROFILE\0", u8::try_from(i).unwrap());
        chunk[12] = u8::try_from(i).unwrap();
        chunk[13] = u8::try_from(ICC_CHUNKS_MAX).unwrap();
        segments.push((0xE2, chunk));
    }
    segments.push((0xE1, payload(b"Exif\0\0MM\0\x2a\0\0\0\x08", 0)));
    segments.push((0xEE, b"Adobe\0\x64\0\0\0\0\0".to_vec()));
    let other = 64 * MIB / FULL;
    for i in 0..other {
        let segment = match i % 3 {
            0 => (0xFE, payload(b"", i.to_le_bytes()[0])),
            1 => (
                0xE1,
                payload(b"http://ns.adobe.com/xap/1.0/\0", i.to_le_bytes()[0]),
            ),
            _ => (0xED, payload(b"Photoshop 3.0\0", i.to_le_bytes()[0])),
        };
        segments.push(segment);
    }
    let picture = jpeg(&segments);
    drop(segments);

    let baseline = CURRENT.load(Ordering::Relaxed);
    PEAK.store(baseline, Ordering::Relaxed);
    let mut out = Written(0);
    let done = downscale(picture.as_slice(), &mut out, 512 * 512).unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - baseline;
    assert_eq!(done.factor, 2);

    let rendering = (ICC_CHUNKS_MAX + 3) * FULL;
    eprintln!(
        "peak heap downscaling a {} MiB JPEG with {} KiB of rendering segments: {} KiB (output {} KiB)",
        picture.len() / MIB,
        rendering / 1024,
        peak / 1024,
        out.0 / 1024
    );
    // Rows and buffers are well under a MiB at this size.
    assert!(
        peak < rendering + KEPT_OTHER_MAX + MIB,
        "the downscale's peak heap is {peak} bytes for {} bytes of segments",
        picture.len()
    );
    assert!(out.0 > rendering && out.0 < rendering + KEPT_OTHER_MAX + MIB);
}
