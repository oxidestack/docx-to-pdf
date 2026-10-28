//! A JPEG with several scans (progressive, or sequential with its components in separate scans)
//! downscales exactly as the same coefficients coded in one sequential scan do. The pictures are made by
//! [`super::testenc`], itself checked against `jpeg-decoder`.

use super::testenc::{self, Frame, Scan};
use super::*;
use crate::num::ToF64;
use crate::pictures::Header;

fn bitmap(width: usize, height: usize, gray: bool) -> u64 {
    Header {
        kind: Kind::Jpeg,
        width: u32::try_from(width).unwrap(),
        height: u32::try_from(height).unwrap(),
        bits: if gray { 8 } else { 24 },
    }
    .bitmap()
}

fn shrink(source: &[u8], bound: u64) -> Result<(Downscaled, Vec<u8>), Refused> {
    let mut out = Vec::new();
    let done = downscale(source, &mut out, bound)?;
    Ok((done, out))
}

/// Pixels as `jpeg-decoder` decodes them at full size.
fn full(bytes: &[u8]) -> Vec<u8> {
    jpeg_decoder::Decoder::new(bytes).decode().unwrap()
}

/// Frames of every sampling at sizes that are and are not whole MCUs.
fn frames() -> Vec<Frame> {
    vec![
        testenc::frame(203, 117, None, 90),
        testenc::frame(203, 117, Some((2, 2)), 90),
        testenc::frame(96, 80, Some((2, 1)), 85),
        testenc::frame(61, 72, Some((1, 2)), 95),
        testenc::frame(77, 51, Some((1, 1)), 75),
    ]
}

/// The scripts beside the sequential one: name, progressive, scans.
fn scripts(n: usize) -> Vec<(&'static str, bool, Vec<Scan>)> {
    vec![
        ("separate", false, testenc::separate(n)),
        ("simple progression", true, testenc::simple_progression(n)),
        ("spectral selection", true, testenc::spectral(n)),
        ("deep progression", true, testenc::deep_progression(n)),
    ]
}

/// Every frame, restart interval and script: the frame and blocks, the sequential picture, and the scripted one.
fn cases() -> Vec<(String, Frame, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for frame in frames() {
        for restart in [0, 5] {
            let frame = Frame {
                restart,
                ..frame.clone()
            };
            let blocks = testenc::blocks(&frame, testenc::texture);
            let get = |c, x, y| blocks.get(c, x, y);
            let n = frame.components.len();
            let base = testenc::write(&frame, &testenc::sequential(n), &[], &get);
            for (name, progressive, script) in scripts(n) {
                let scripted = Frame {
                    progressive,
                    ..frame.clone()
                };
                let bytes = testenc::write(&scripted, &script, &[], &get);
                let what = format!(
                    "{}x{} {:?} restart {restart}: {name}",
                    frame.width,
                    frame.height,
                    frame
                        .components
                        .iter()
                        .map(|c| (c.h, c.v))
                        .collect::<Vec<_>>()
                );
                out.push((what, frame.clone(), base.clone(), bytes));
            }
        }
    }
    out
}

#[test]
fn the_test_encoder_writes_what_jpeg_decoder_reads_as_the_sequential_picture() {
    for (what, _, base, bytes) in cases() {
        assert!(full(&bytes) == full(&base), "{what}");
    }
}

#[test]
fn a_jpeg_of_several_scans_downscales_as_its_sequential_twin() {
    for (what, frame, base, bytes) in cases() {
        let gray = frame.components.len() == 1;
        for factor in [2usize, 4, 8, 16] {
            let bound = bitmap(
                frame.width.div_ceil(factor),
                frame.height.div_ceil(factor),
                gray,
            );
            let (done, want) = shrink(&base, bound).unwrap();
            assert_eq!(done.factor as usize, factor, "{what}");
            let got =
                shrink(&bytes, bound).unwrap_or_else(|e| panic!("{what} factor {factor}: {e}"));
            assert!(
                got.0 == done && got.1 == want,
                "{what} factor {factor}: output differs"
            );
        }
    }
}

fn corpus(document: &str, entry: &str) -> Vec<u8> {
    use std::io::Read as _;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/documents")
        .join(document);
    let mut archive = ::zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut bytes = Vec::new();
    archive
        .by_name(entry)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/pictures/jpeg/fixtures")
            .join(name),
    )
    .unwrap()
}

/// Baseline JPEGs transcoded without loss by libjpeg-turbo's `jpegtran` (`-copy all -progressive`, one with
/// `-restart 3B`, one with `-scans` putting each component in a sequential scan of its own, one `-grayscale` first):
/// an encoder independent of this crate's, with its own per-scan Huffman tables. The baselines are `cjpeg`'s, of
/// pictures `fixtures/make-fixtures.py` draws (4:2:0 at odd sizes and qualities 75 and 95, 4:4:4 at 98 behind a
/// comment and Adobe's segment), and the corpus's text-1p thumbnail. Each downscales to the very picture its
/// baseline source does.
#[test]
fn jpegs_made_progressive_by_jpegtran_downscale_as_their_sources() {
    let pairs = [
        (
            "pattern-61x71.progressive.jpg",
            fixture("pattern-61x71.jpg"),
        ),
        (
            "pattern-130x194.progressive.jpg",
            fixture("pattern-130x194.jpg"),
        ),
        (
            "pattern-195x129.progressive-restart.jpg",
            fixture("pattern-195x129.jpg"),
        ),
        (
            "pattern-195x129.separate.jpg",
            fixture("pattern-195x129.jpg"),
        ),
        (
            "pattern-257x131.progressive.jpg",
            fixture("pattern-257x131.jpg"),
        ),
        (
            "thumbnail.progressive.jpg",
            corpus("text/text-1p.docx", "docProps/thumbnail.jpeg"),
        ),
        (
            "pattern-130x194.gray.progressive.jpg",
            fixture("pattern-130x194.gray.jpg"),
        ),
    ];
    for (name, source) in pairs {
        let transcoded = fixture(name);
        assert!(
            full(&transcoded) == full(&source),
            "{name}: jpegtran kept the picture"
        );
        let h = crate::pictures::header(&source).unwrap();
        for factor in [2u32, 4, 8] {
            let bound = Header {
                width: h.width.div_ceil(factor),
                height: h.height.div_ceil(factor),
                ..h
            }
            .bitmap();
            let (done, want) = shrink(&source, bound).unwrap();
            let (got_done, got) = shrink(&transcoded, bound)
                .unwrap_or_else(|e| panic!("{name} factor {factor}: {e}"));
            assert_eq!((got_done, done.factor), (done, factor), "{name}");
            assert!(
                full(&got) == full(&want),
                "{name} factor {factor}: another picture"
            );
        }
    }
}

/// `bytes` with `segment` (marker and payload) inserted before its `nth` `SOS` (from 0).
fn before_scan(bytes: &[u8], nth: usize, marker: u8, payload: &[u8]) -> Vec<u8> {
    // Entropy-coded data never holds `FF DA`: a data `FF` is followed by `00`.
    let at = (0..bytes.len() - 1)
        .filter(|&i| bytes[i] == 0xFF && bytes[i + 1] == 0xDA)
        .nth(nth)
        .unwrap();
    let mut out = bytes[..at].to_vec();
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&(u16::try_from(payload.len()).unwrap() + 2).to_be_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&bytes[at..]);
    out
}

fn gray_frame() -> Frame {
    testenc::frame(64, 48, None, 90)
}

fn scripted(frame: &Frame, progressive: bool, script: &[Scan]) -> Vec<u8> {
    let frame = Frame {
        progressive,
        ..frame.clone()
    };
    let blocks = testenc::blocks(&frame, testenc::texture);
    testenc::write(&frame, script, &[], &|c, x, y| blocks.get(c, x, y))
}

fn refusal(bytes: &[u8]) -> String {
    shrink(bytes, bitmap(32, 24, true)).unwrap_err().to_string()
}

#[test]
fn a_progressive_scan_libjpeg_rejects_is_refused_as_damaged() {
    let frame = gray_frame();
    let bad = |ss, se, ah, al| {
        let mut script = testenc::simple_progression(1);
        script.push(Scan {
            comps: vec![0],
            ss,
            se,
            ah,
            al,
        });
        refusal(&scripted(&frame, true, &script))
    };
    for (ss, se, ah, al) in [
        (0, 5, 0, 0),
        (5, 3, 0, 0),
        (1, 64, 0, 0),
        (1, 63, 3, 1),
        (1, 63, 0, 14),
    ] {
        let why = bad(ss, se, ah, al);
        assert_eq!(
            why, "damaged: a bad progressive scan",
            "{ss}..{se} {ah}/{al}"
        );
    }
    let rgb = testenc::frame(64, 48, Some((2, 2)), 90);
    let mut script = testenc::simple_progression(3);
    script.push(Scan {
        comps: vec![1, 2],
        ss: 1,
        se: 63,
        ah: 0,
        al: 0,
    });
    assert_eq!(
        refusal(&scripted(&rgb, true, &script)),
        "damaged: a bad progressive scan"
    );
}

#[test]
fn a_picture_missing_a_scan_it_needs_is_refused() {
    let rgb = testenc::frame(64, 48, Some((2, 2)), 90);
    // Progressive, chroma without its first DC scan.
    let script: Vec<Scan> = testenc::spectral(3)
        .into_iter()
        .filter(|s| !(s.ss == 0 && s.comps == [2]))
        .collect();
    assert_eq!(
        refusal(&scripted(&rgb, true, &script)),
        "damaged: a component without its first DC scan"
    );
    // Sequential: a component in no scan, or in two.
    let mut script = testenc::separate(3);
    script.pop();
    assert_eq!(
        refusal(&scripted(&rgb, false, &script)),
        "damaged: a component in no scan"
    );
    let mut script = testenc::separate(3);
    script.push(script[0].clone());
    assert_eq!(
        refusal(&scripted(&rgb, false, &script)),
        "damaged: a component in two sequential scans"
    );
}

#[test]
fn more_than_256_scans_are_refused() {
    let frame = gray_frame();
    let script = |n: usize| -> Vec<Scan> {
        let mut out = vec![Scan {
            comps: vec![0],
            ss: 0,
            se: 0,
            ah: 0,
            al: 0,
        }];
        // The same one-coefficient band coded again and again: legal for the decoder, which keeps the last.
        out.extend((1..n).map(|_| Scan {
            comps: vec![0],
            ss: 1,
            se: 1,
            ah: 0,
            al: 0,
        }));
        out
    };
    assert!(shrink(&scripted(&frame, true, &script(256)), bitmap(32, 24, true)).is_ok());
    assert_eq!(
        refusal(&scripted(&frame, true, &script(257))),
        format!("more than {} scans", index::MAX_SCANS)
    );
}

#[test]
fn tables_between_scans_are_followed_and_a_quantiser_changed_after_its_scan_refused() {
    let frame = gray_frame();
    let bytes = scripted(&frame, true, &testenc::simple_progression(1));
    let (_, want) = shrink(&bytes, bitmap(32, 24, true)).unwrap();
    let dqt = |id: u8, value: u8| [vec![id], vec![value; 64]].concat();
    // Table 0 redefined after the first scan: the component using it has been read with the old one.
    assert_eq!(
        refusal(&before_scan(&bytes, 1, 0xDB, &dqt(0, 3))),
        "damaged: a quantisation table changed after its component's scan"
    );
    // Defined again the same, or another table: nothing changes.
    let same = [
        vec![0u8],
        frame.quant[0]
            .iter()
            .map(|&v| u8::try_from(v).unwrap())
            .collect(),
    ]
    .concat();
    for (marker, payload) in [
        (0xDB, same),
        (0xDB, dqt(2, 9)),
        (0xFE, b"a comment".to_vec()),
    ] {
        let (_, got) = shrink(
            &before_scan(&bytes, 3, marker, &payload),
            bitmap(32, 24, true),
        )
        .unwrap();
        assert!(got == want, "{marker:#x}");
    }
    // A restart interval defined between scans holds from the next scan: here one the encoder did not use, so the
    // scan after it misses its markers.
    let why = refusal(&before_scan(&bytes, 1, 0xDD, &[0, 2]));
    assert!(why.starts_with("damaged: RST0 expected"), "{why}");
}

#[test]
fn a_progressive_picture_cut_between_scans_downscales_from_the_scans_it_has() {
    let frame = testenc::frame(96, 64, Some((2, 2)), 90);
    let bytes = scripted(&frame, true, &testenc::simple_progression(3));
    let at = (0..bytes.len() - 1)
        .filter(|&i| bytes[i] == 0xFF && bytes[i + 1] == 0xDA)
        .nth(6)
        .unwrap();
    let (done, out) = shrink(&bytes[..at], bitmap(48, 32, false)).unwrap();
    assert_eq!(done.factor, 2);
    let (_, whole) = shrink(&bytes, bitmap(48, 32, false)).unwrap();
    let (a, b) = (full(&out), full(&whole));
    let mean = a
        .iter()
        .zip(&b)
        .map(|(x, y)| f64::from(x.abs_diff(*y)))
        .sum::<f64>()
        / a.len().to_f64();
    assert!(
        mean < 8.0,
        "the first six scans hold most of the picture: mean difference {mean}"
    );
}

#[test]
fn the_scan_copy_is_made_in_the_scratch_directory_it_is_given() {
    // A scratch directory that does not exist fails the multi-scan picture, which copies its scans
    // there, and not the single-scan one, which streams without a copy.
    let frame = testenc::frame(96, 64, Some((2, 2)), 90);
    let progressive = scripted(&frame, true, &testenc::simple_progression(3));
    let baseline = scripted(&frame, false, &testenc::sequential(3));
    let bound = bitmap(48, 32, false);
    let scratch = tempfile::tempdir().unwrap();
    let missing = scratch.path().join("missing");
    let into = |bytes: &[u8], dir: &std::path::Path| {
        downscale_in(bytes, &mut Vec::new(), bound, dir).map(|done| done.factor)
    };
    assert!(matches!(
        into(&progressive, &missing),
        Err(Refused::Output(_))
    ));
    assert_eq!(into(&baseline, &missing).unwrap(), 2);
    assert_eq!(into(&progressive, scratch.path()).unwrap(), 2);
    assert_eq!(
        std::fs::read_dir(scratch.path()).unwrap().count(),
        0,
        "the scan copy is removed with the picture"
    );
}
