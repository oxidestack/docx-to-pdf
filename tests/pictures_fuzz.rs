//! The PNG and JPEG downscales fuzzed on stable Rust, in `cargo test` and so in CI.
//! Seeds are every PNG and JPEG of the corpus documents under 256 KiB and a few made here; each round mutates one
//! (bit flips, interesting values, ranges cut, repeated or spliced from another seed; a PNG's chunk CRCs mended
//! most of the time so the mutation reaches past them) and downscales it under a bound that asks for a factor from 1
//! to past 8. A downscale may refuse anything; it must not panic, and must finish in time bounded by its input.
//!
//! The run is short by default (`DOCX_TO_PDF_FUZZ_SECONDS`, 2 per format) and reproducible (`DOCX_TO_PDF_FUZZ_SEED`).
//! An input that fails is written to `target/pictures-fuzz/` and the test names it. The coverage-guided fuzzing is
//! `fuzz/` (cargo-fuzz, nightly, local).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use docx_to_pdf::num::usize_from_u64;
use docx_to_pdf::pictures::{Kind, downscale, header, jpeg};

#[path = "../src/pictures/jpeg/testenc.rs"]
mod testenc;

/// xorshift64*: enough randomness for mutations, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize_from_u64(self.next() % n.max(1) as u64)
    }
}

fn env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The largest LibreOffice bitmap of a seed, in bytes.
const MAX_SEED_BITMAP: u64 = 256 * 1024;

/// Every PNG and JPEG under 256 KiB in the corpus documents whose bitmap is at most [`MAX_SEED_BITMAP`].
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the name is lowercased before the comparison"
)]
fn corpus_pictures(kind: Kind) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut dirs = vec![PathBuf::from("tests/corpus/documents")];
    while let Some(dir) = dirs.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = read.filter_map(|e| Some(e.ok()?.path())).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            let Ok(mut archive) = zip::ZipArchive::new(file) else {
                continue;
            };
            for i in 0..archive.len() {
                let Ok(mut entry) = archive.by_index(i) else {
                    continue;
                };
                let Ok(name) = entry.name().map(|n| n.to_ascii_lowercase()) else {
                    continue;
                };
                let wanted = match kind {
                    Kind::Png => name.ends_with(".png"),
                    Kind::Jpeg => name.ends_with(".jpg") || name.ends_with(".jpeg"),
                };
                if !wanted || entry.size() > 256 * 1024 {
                    continue;
                }
                let mut bytes = Vec::new();
                // Small bitmaps only: a debug build decodes about a megabyte of them a second.
                let small = |b: &[u8]| header(b).is_some_and(|h| h.bitmap() <= MAX_SEED_BITMAP);
                if entry.read_to_end(&mut bytes).is_ok() && small(&bytes) {
                    out.push(bytes);
                }
            }
        }
    }
    out
}

/// A few pictures made here, for what the corpus may lack: alpha, 16 bits, a palette, gray; 4:2:0, 4:2:2 and
/// restart intervals; JPEGs of several scans: progressive with successive approximation or spectral
/// selection only, sequential with components in separate scans, and baselines made progressive by `jpegtran`.
fn made_pictures(kind: Kind) -> Vec<Vec<u8>> {
    match kind {
        Kind::Png => {
            use png::{BitDepth as D, ColorType as C};
            [
                (C::Rgb, D::Eight, 37, 23),
                (C::Rgba, D::Eight, 40, 17),
                (C::Grayscale, D::Sixteen, 19, 33),
                (C::GrayscaleAlpha, D::Eight, 9, 9),
                (C::Grayscale, D::Two, 30, 7),
                (C::Indexed, D::Four, 21, 13),
                (C::Rgba, D::Sixteen, 12, 12),
            ]
            .into_iter()
            .map(|(colour, depth, width, height): (C, D, u32, u32)| {
                let mut out = Vec::new();
                let mut encoder = png::Encoder::new(&mut out, width, height);
                encoder.set_color(colour);
                encoder.set_depth(depth);
                if colour == C::Indexed {
                    encoder.set_palette((0..48u8).collect::<Vec<_>>());
                    encoder.set_trns(vec![0u8, 128, 255]);
                }
                let mut writer = encoder.write_header().unwrap();
                let bits = colour.samples() * depth as usize;
                let row = (width as usize * bits).div_ceil(8);
                let pixels: Vec<u8> = (0..row * height as usize)
                    .map(|i| u8::try_from(i * 7 % 251).unwrap())
                    .collect();
                writer.write_image_data(&pixels).unwrap();
                writer.finish().unwrap();
                out
            })
            .collect()
        }
        Kind::Jpeg => {
            use jpeg_encoder::SamplingFactor as S;
            let mut out: Vec<Vec<u8>> = [
                (S::F_2_2, false, 0, 61, 45, false),
                (S::F_2_1, false, 2, 33, 70, false),
                (S::F_1_1, true, 0, 50, 9, false),
                (S::F_1_2, false, 1, 17, 17, false),
                (S::F_2_2, false, 0, 40, 24, true),
            ]
            .into_iter()
            .map(|(sampling, gray, restart, width, height, progressive)| {
                let mut out = Vec::new();
                let mut encoder = jpeg_encoder::Encoder::new(&mut out, 80);
                encoder.set_sampling_factor(sampling);
                encoder.set_restart_interval(restart);
                encoder.set_progressive(progressive);
                let (colour, channels) = if gray {
                    (jpeg_encoder::ColorType::Luma, 1)
                } else {
                    (jpeg_encoder::ColorType::Rgb, 3)
                };
                let pixels: Vec<u8> = (0..usize::from(width) * usize::from(height) * channels)
                    .map(|i| u8::try_from(i * 13 % 253).unwrap())
                    .collect();
                encoder.encode(&pixels, width, height, colour).unwrap();
                out
            })
            .collect();
            for (width, height, luma, restart, progressive, script) in [
                (37, 29, None, 3, true, testenc::simple_progression(1)),
                (
                    45,
                    33,
                    Some((2, 2)),
                    0,
                    true,
                    testenc::simple_progression(3),
                ),
                (40, 24, Some((2, 1)), 2, true, testenc::deep_progression(3)),
                (24, 40, Some((1, 2)), 0, true, testenc::spectral(3)),
                (33, 21, Some((2, 2)), 1, false, testenc::separate(3)),
            ] {
                let frame = testenc::Frame {
                    progressive,
                    restart,
                    ..testenc::frame(width, height, luma, 85)
                };
                let blocks = testenc::blocks(&frame, testenc::texture);
                out.push(testenc::write(&frame, &script, &[], &|c, x, y| {
                    blocks.get(c, x, y)
                }));
            }
            let fixtures = Path::new("src/pictures/jpeg/fixtures");
            for name in [
                "pattern-61x71.progressive.jpg",
                "pattern-195x129.progressive-restart.jpg",
                "pattern-195x129.separate.jpg",
            ] {
                out.push(std::fs::read(fixtures.join(name)).unwrap());
            }
            out
        }
    }
}

const INTERESTING: [u8; 8] = [0x00, 0x01, 0x7F, 0x80, 0xFE, 0xFF, 0x10, 0x08];
const INTERESTING_16: [u16; 7] = [0, 1, 2, 0x7FFF, 0x8000, 0xFFFF, 0x0100];

/// One to four mutations of `seed`, another seed `other` there to splice from.
fn mutate(rng: &mut Rng, seed: &[u8], other: &[u8]) -> Vec<u8> {
    let mut bytes = seed.to_vec();
    for _ in 0..=rng.below(4) {
        let len = bytes.len();
        if len < 4 {
            break;
        }
        let at = rng.below(len);
        match rng.below(9) {
            0 => bytes[at] ^= 1 << rng.below(8),
            1 => bytes[at] = INTERESTING[rng.below(INTERESTING.len())],
            2 => bytes[at] = rng.next().to_le_bytes()[0],
            3 if at + 2 <= len => {
                let v = INTERESTING_16[rng.below(INTERESTING_16.len())];
                bytes[at..at + 2].copy_from_slice(&v.to_be_bytes());
            }
            4 => bytes.truncate(at.max(8)),
            5 => {
                let n = rng.below(64.min(len - at)) + 1;
                bytes.drain(at..at + n);
            }
            6 => {
                let n = rng.below(64.min(len - at)) + 1;
                let copy = bytes[at..at + n].to_vec();
                let to = rng.below(bytes.len());
                bytes.splice(to..to, copy);
            }
            7 if !other.is_empty() => {
                let from = rng.below(other.len());
                let n = rng.below(256.min(other.len() - from)) + 1;
                let to = rng.below(len);
                let end = (to + n).min(len);
                bytes.splice(to..end, other[from..from + n].iter().copied());
            }
            _ => {
                // A marker's or chunk's 16-bit length or size field set to something interesting.
                let v = INTERESTING_16[rng.below(INTERESTING_16.len())];
                if let Some(p) = (at..len.saturating_sub(3)).find(|&i| bytes[i] == 0xFF) {
                    bytes[p + 2..p + 4].copy_from_slice(&v.to_be_bytes());
                }
            }
        }
    }
    bytes
}

/// Recomputes every chunk's CRC of the PNG `bytes`, as far as its chunks can be walked.
fn mend_png_crcs(bytes: &mut [u8]) {
    let mut at = 8;
    while at + 12 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let Some(end) = (at + 8)
            .checked_add(length)
            .filter(|&e| e + 4 <= bytes.len())
        else {
            return;
        };
        let crc = crc32fast::hash(&bytes[at + 4..end]);
        bytes[end..end + 4].copy_from_slice(&crc.to_be_bytes());
        at = end + 4;
    }
}

/// Downscales `bytes` and fails with where they were kept when it panics or takes longer than `limit`.
fn check(kind: Kind, bytes: &[u8], shift: u32, limit: Duration, label: &str) -> Result<(), String> {
    let started = Instant::now();
    let mut bound = 0;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bound = bound_for(shift, bytes);
        downscale_one(kind, bytes, bound)
    }));
    let took = started.elapsed();
    let failure = match result {
        Err(_) => Some("panicked".to_owned()),
        Ok(_) if took > limit => Some(format!("took {took:?} for {} bytes", bytes.len())),
        Ok(_) => None,
    };
    let Some(failure) = failure else {
        return Ok(());
    };
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pictures-fuzz");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{label}.bin"));
    std::fs::write(&path, bytes).unwrap();
    Err(format!(
        "{failure}: bound {bound}, input kept at {}",
        path.display()
    ))
}

fn downscale_one(kind: Kind, bytes: &[u8], bound: u64) -> Result<(), String> {
    match kind {
        Kind::Png => downscale::png(bytes, std::io::sink(), bound)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        Kind::Jpeg => jpeg::downscale(bytes, std::io::sink(), bound)
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

/// A shift of the bitmap that asks for a factor of 1, 2, 4, 8, 16 or 64.
fn shift(rng: &mut Rng) -> u32 {
    [0u32, 2, 4, 6, 8, 12][rng.below(6)]
}

/// The bitmap the picture `bytes` claims shifted right by `shift`, or a small bound when it claims nothing readable.
fn bound_for(shift: u32, bytes: &[u8]) -> u64 {
    let bitmap = header(bytes).map_or(1 << 16, |h| h.bitmap());
    (bitmap >> shift).max(1)
}

fn fuzz(kind: Kind) {
    let seconds: u64 = env("DOCX_TO_PDF_FUZZ_SECONDS", 2);
    let start_seed: u64 = env("DOCX_TO_PDF_FUZZ_SEED", 0x0095);
    let mut seeds = corpus_pictures(kind);
    let from_corpus = seeds.len();
    seeds.extend(made_pictures(kind));
    let limit = Duration::from_secs(env("DOCX_TO_PDF_FUZZ_LIMIT_SECONDS", 10));
    let mut rng = Rng(start_seed | 1);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut failures = Vec::new();
    // Every seed unmutated first: they must all go through.
    for (i, seed) in seeds.iter().enumerate() {
        let shift = shift(&mut rng);
        if let Err(e) = check(kind, seed, shift, limit, &format!("{kind:?}-seed-{i}")) {
            failures.push(e);
        }
    }
    let mut rounds = 0u64;
    while Instant::now() < deadline && failures.len() < 10 {
        let seed = &seeds[rng.below(seeds.len())];
        let other = &seeds[rng.below(seeds.len())];
        let mut bytes = mutate(&mut rng, seed, other);
        if kind == Kind::Png && rng.below(4) != 0 {
            mend_png_crcs(&mut bytes);
        }
        let shift = shift(&mut rng);
        let label = format!("{kind:?}-{start_seed:x}-{rounds}");
        if let Err(e) = check(kind, &bytes, shift, limit, &label) {
            failures.push(e);
        }
        rounds += 1;
    }
    eprintln!(
        "{kind:?}: {} seeds ({from_corpus} from the corpus), {rounds} mutated rounds in {seconds} s",
        seeds.len()
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn the_png_downscale_survives_mutated_pictures() {
    fuzz(Kind::Png);
}

#[test]
fn the_jpeg_downscale_survives_mutated_pictures() {
    fuzz(Kind::Jpeg);
}
