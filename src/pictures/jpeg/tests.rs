//! The downscale checked against independent codecs: `jpeg-encoder` makes the sources, `jpeg-decoder` reads both
//! the source and the output, and the output is compared with a box average of the source at full size.

use super::*;
use crate::num::{ToF64, saturating_u8};
use crate::pictures::Header;

/// A source picture's options.
#[derive(Clone)]
struct Source {
    width: u16,
    height: u16,
    gray: bool,
    sampling: jpeg_encoder::SamplingFactor,
    quality: u8,
    progressive: bool,
    restart: u16,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
    density: Option<u16>,
}

impl Source {
    fn rgb(width: u16, height: u16, sampling: jpeg_encoder::SamplingFactor) -> Self {
        Self {
            width,
            height,
            gray: false,
            sampling,
            quality: 90,
            progressive: false,
            restart: 0,
            icc: None,
            exif: None,
            density: None,
        }
    }

    fn gray(width: u16, height: u16) -> Self {
        Self {
            gray: true,
            ..Self::rgb(width, height, jpeg_encoder::SamplingFactor::F_1_1)
        }
    }

    /// A smooth gradient, so a box average and a DCT-scaled block agree closely.
    #[allow(
        clippy::many_single_char_names,
        reason = "the formula's own names (x, y, w, h; r, g, b; the DCT's indices)"
    )]
    fn pixels(&self) -> Vec<u8> {
        let (w, h) = (usize::from(self.width), usize::from(self.height));
        let mut out = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let r = u8::try_from(x * 255 / w.max(1)).unwrap();
                let g = u8::try_from(y * 255 / h.max(1)).unwrap();
                let b = u8::try_from((x + y) * 255 / (w + h)).unwrap();
                if self.gray {
                    out.push(u8::try_from((x + 2 * y) * 255 / (w + 2 * h)).unwrap());
                } else {
                    out.extend_from_slice(&[r, g, b]);
                }
            }
        }
        out
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, self.quality);
        encoder.set_sampling_factor(self.sampling);
        encoder.set_progressive(self.progressive);
        if self.restart > 0 {
            encoder.set_restart_interval(self.restart);
        }
        if let Some(icc) = &self.icc {
            encoder.add_icc_profile(icc).unwrap();
        }
        if let Some(exif) = &self.exif {
            encoder.add_exif_metadata(exif).unwrap();
        }
        if let Some(dpi) = self.density {
            encoder.set_density(jpeg_encoder::PixelDensity::dpi(dpi));
        }
        let colour = if self.gray {
            jpeg_encoder::ColorType::Luma
        } else {
            jpeg_encoder::ColorType::Rgb
        };
        encoder
            .encode(&self.pixels(), self.width, self.height, colour)
            .unwrap();
        out
    }
}

/// Width, height and pixels (gray or RGB) of `bytes`, by `jpeg-decoder`.
fn decode(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    let pixels = decoder.decode().unwrap();
    let info = decoder.info().unwrap();
    (usize::from(info.width), usize::from(info.height), pixels)
}

/// `pixels` averaged over `factor × factor` blocks, edge blocks over what they hold.
fn box_average(
    width: usize,
    height: usize,
    channels: usize,
    pixels: &[u8],
    factor: usize,
) -> Vec<u8> {
    let (ow, oh) = (width.div_ceil(factor), height.div_ceil(factor));
    let mut out = Vec::with_capacity(ow * oh * channels);
    for oy in 0..oh {
        for ox in 0..ow {
            for c in 0..channels {
                let (mut sum, mut n) = (0u64, 0u64);
                for y in oy * factor..((oy + 1) * factor).min(height) {
                    for x in ox * factor..((ox + 1) * factor).min(width) {
                        sum += u64::from(pixels[(y * width + x) * channels + c]);
                        n += 1;
                    }
                }
                out.push(u8::try_from((sum + n / 2) / n).unwrap());
            }
        }
    }
    out
}

/// The luma of gray or RGB pixels (ITU-R BT.601, JFIF's).
fn luma(pixels: &[u8], channels: usize) -> Vec<u8> {
    pixels
        .chunks(channels)
        .map(|p| match p {
            [y] => *y,
            [r, g, b] => saturating_u8(
                (0.299 * f64::from(*r) + 0.587 * f64::from(*g) + 0.114 * f64::from(*b)).round(),
            ),
            _ => unreachable!(),
        })
        .collect()
}

/// The first component (Y) of `bytes` as `jpeg-decoder` decodes it, at full size or DCT-scaled to `size`, before any
/// colour conversion (`ColorTransform::RGB` passes the components through; `None` panics on three in 0.3.2).
fn first_component(bytes: &[u8], size: Option<(usize, usize)>) -> Vec<u8> {
    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    decoder.read_info().unwrap();
    if let Some((w, h)) = size {
        decoder
            .scale(u16::try_from(w).unwrap(), u16::try_from(h).unwrap())
            .unwrap();
    }
    if decoder.info().unwrap().pixel_format != jpeg_decoder::PixelFormat::L8 {
        decoder.set_color_transform(jpeg_decoder::ColorTransform::RGB);
    }
    let pixels = decoder.decode().unwrap();
    let info = decoder.info().unwrap();
    if let Some((w, h)) = size {
        assert_eq!((usize::from(info.width), usize::from(info.height)), (w, h));
    }
    pixels
        .chunks(info.pixel_format.pixel_bytes())
        .map(|p| p[0])
        .collect()
}

/// The segments of `bytes` up to its first `SOS`: marker and payload.
fn segments(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = 2;
    loop {
        let marker = bytes[at + 1];
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        out.push((marker, bytes[at + 4..at + 2 + length].to_vec()));
        if marker == 0xDA {
            return out;
        }
        at += 2 + length;
    }
}

fn of(segments: &[(u8, Vec<u8>)], marker: u8) -> Vec<Vec<u8>> {
    segments
        .iter()
        .filter(|(m, _)| *m == marker)
        .map(|(_, d)| d.clone())
        .collect()
}

/// The bitmap of `width × height` at `bits`: a bound reached exactly there.
fn bound(width: u32, height: u32, bits: u8) -> u64 {
    Header {
        kind: Kind::Jpeg,
        width,
        height,
        bits,
    }
    .bitmap()
}

fn shrink(source: &[u8], bound: u64) -> Result<(Downscaled, Vec<u8>), Refused> {
    let mut out = Vec::new();
    let done = downscale(source, &mut out, bound)?;
    Ok((done, out))
}

/// Mean and largest absolute difference.
fn difference(a: &[u8], b: &[u8]) -> (f64, u8) {
    assert_eq!(a.len(), b.len());
    let mut sum = 0u64;
    let mut max = 0u8;
    for (&x, &y) in a.iter().zip(b) {
        let d = x.abs_diff(y);
        sum += u64::from(d);
        max = max.max(d);
    }
    (sum.to_f64() / a.len().to_f64(), max)
}

use jpeg_encoder::SamplingFactor::{F_1_1, F_1_2, F_2_1, F_2_2};

#[test]
fn every_sampling_and_factor_downscales_close_to_a_box_average() {
    let cases = [
        Source::gray(256, 192),
        Source::rgb(256, 192, F_1_1),
        Source::rgb(256, 192, F_2_2),
        Source::rgb(256, 192, F_2_1),
        Source::rgb(256, 192, F_1_2),
        Source::gray(203, 117),
        Source::rgb(203, 117, F_2_2),
    ];
    for source in cases {
        let bytes = source.encode();
        let (w, h, full) = decode(&bytes);
        let channels = if source.gray { 1 } else { 3 };
        let bits = if source.gray { 8 } else { 24 };
        for factor in [2u32, 4, 8, 16, 32] {
            let (ow, oh) = (
                u32::try_from(w).unwrap().div_ceil(factor),
                u32::try_from(h).unwrap().div_ceil(factor),
            );
            let what = format!(
                "{}x{} gray {} sampling {:?} factor {factor}",
                w, h, source.gray, source.sampling as u8
            );
            let (done, out) =
                shrink(&bytes, bound(ow, oh, bits)).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(
                done,
                Downscaled {
                    factor,
                    width: ow,
                    height: oh
                },
                "{what}"
            );
            let (dw, dh, pixels) = decode(&out);
            assert_eq!(
                (u32::try_from(dw).unwrap(), u32::try_from(dh).unwrap()),
                (ow, oh),
                "{what}"
            );
            let want = box_average(w, h, channels, &full, factor as usize);
            // Luma is never subsampled: it matches at every size. Below 16 pixels a side the decoder's chroma
            // upsampling (from as few as 4 × 2 samples), not the downscale, dominates the colour's difference.
            let (mean, max) = difference(&luma(&pixels, channels), &luma(&want, channels));
            assert!(
                mean < 2.5,
                "{what}: luma mean difference {mean}, largest {max}"
            );
            let (mean, max) = difference(&pixels, &want);
            let limit = if ow < 16 { 8.0 } else { 2.5 };
            assert!(
                mean < limit,
                "{what}: mean difference {mean}, largest {max}"
            );
            assert!(max < 40, "{what}: mean difference {mean}, largest {max}");
        }
    }
}

#[test]
fn the_smallest_factor_is_taken_and_the_aspect_holds_within_a_pixel() {
    let bytes = Source::rgb(203, 117, F_2_2).encode();
    // Halved, 102 × 59; one byte less than that and a quarter is needed: 51 × 30.
    let (done, _) = shrink(&bytes, bound(102, 59, 24)).unwrap();
    assert_eq!((done.factor, done.width, done.height), (2, 102, 59));
    let (done, _) = shrink(&bytes, bound(102, 59, 24) - 1).unwrap();
    assert_eq!((done.factor, done.width, done.height), (4, 51, 30));
    // Each side is the source's divided by the factor, rounded up: within a pixel of the exact aspect.
    assert!((51.0f64 - 203.0 / 4.0).abs() < 1.0 && (30.0f64 - 117.0 / 4.0).abs() < 1.0);
}

#[test]
fn the_quantisation_tables_and_the_sampling_are_the_sources() {
    for (source, quality) in [
        (Source::rgb(128, 96, F_2_2), 75),
        (Source::rgb(128, 96, F_2_1), 95),
        (Source::rgb(128, 96, F_1_1), 50),
        (Source::gray(128, 96), 85),
    ] {
        let source = Source { quality, ..source };
        let bytes = source.encode();
        let (_, out) = shrink(&bytes, bound(64, 48, if source.gray { 8 } else { 24 })).unwrap();
        let (before, after) = (segments(&bytes), segments(&out));
        // Each table by its id, as DQT carries it (a source may define one it never uses; that one is not copied).
        let tables = |s: &[(u8, Vec<u8>)]| -> Vec<Vec<u8>> {
            let mut by_id = vec![Vec::new(); 4];
            for data in of(s, 0xDB) {
                for table in data.chunks(65) {
                    by_id[usize::from(table[0] & 15)] = table.to_vec();
                }
            }
            by_id
        };
        let used: Vec<usize> = of(&after, 0xC0)[0][6..]
            .chunks(3)
            .map(|c| usize::from(c[2]))
            .collect();
        for id in used {
            assert_eq!(
                tables(&after)[id],
                tables(&before)[id],
                "quality {quality}: DQT {id} copied"
            );
        }
        let specs = |s: &[(u8, Vec<u8>)]| -> Vec<u8> { of(s, 0xC0)[0][5..].to_vec() };
        assert_eq!(
            specs(&after),
            specs(&before),
            "quality {quality}: component ids, sampling, tables"
        );
        // And the quality estimated back from the luminance table (libjpeg's scaling of Annex K's) is the source's.
        assert_eq!(estimate_quality(&of(&after, 0xDB)[0][1..65]), quality);
    }
}

/// libjpeg's quality from a luminance table: the scale that maps Annex K.1's table onto it.
fn estimate_quality(zigzag: &[u8]) -> u8 {
    const STANDARD: [u8; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
        56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81,
        104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    (1..=100u8)
        .min_by_key(|&q| {
            let scale = if q < 50 {
                5000 / u32::from(q)
            } else {
                200 - 2 * u32::from(q)
            };
            (0..64)
                .map(|k| {
                    let want =
                        ((u32::from(STANDARD[UNZIGZAG[k]]) * scale + 50) / 100).clamp(1, 255);
                    want.abs_diff(u32::from(zigzag[k]))
                })
                .sum::<u32>()
        })
        .unwrap()
}

#[test]
fn the_icc_profile_and_exif_are_kept_and_the_density_follows_the_factor() {
    let icc: Vec<u8> = (0..70_000u32).map(|i| (i * 7).to_le_bytes()[0]).collect(); // spans two APP2 segments
    // A little-endian TIFF header with one IFD entry: Orientation (0x0112) = 6, rotated 90°.
    let exif = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];
    let source = Source {
        icc: Some(icc.clone()),
        exif: Some(exif.clone()),
        density: Some(300),
        ..Source::rgb(128, 96, F_2_2)
    };
    let bytes = source.encode();
    let (_, out) = shrink(&bytes, bound(32, 24, 24)).unwrap();
    let mut decoder = jpeg_decoder::Decoder::new(out.as_slice());
    decoder.decode().unwrap();
    assert_eq!(decoder.icc_profile(), Some(icc));
    assert_eq!(decoder.exif_data(), Some(exif.as_slice()));
    let jfif = &of(&segments(&out), 0xE0)[0];
    assert_eq!(&jfif[..5], b"JFIF\0");
    assert_eq!(
        (
            jfif[7],
            u16::from_be_bytes([jfif[8], jfif[9]]),
            u16::from_be_bytes([jfif[10], jfif[11]])
        ),
        (1, 75, 75),
        "300 dpi at a quarter of the pixels is 75 dpi: the same physical size"
    );
}

#[test]
fn restart_intervals_are_read() {
    for source in [
        Source {
            restart: 3,
            ..Source::rgb(203, 117, F_2_2)
        },
        Source {
            restart: 1,
            ..Source::gray(100, 60)
        },
    ] {
        let bytes = source.encode();
        assert!(!of(&segments(&bytes), 0xDD).is_empty(), "a DRI written");
        let (w, h, full) = decode(&bytes);
        let channels = if source.gray { 1 } else { 3 };
        let bits = if source.gray { 8 } else { 24 };
        let (done, out) = shrink(
            &bytes,
            bound(
                u32::try_from(w.div_ceil(2)).unwrap(),
                u32::try_from(h.div_ceil(2)).unwrap(),
                bits,
            ),
        )
        .unwrap();
        assert_eq!(done.factor, 2);
        let (_, _, pixels) = decode(&out);
        let (mean, _) = difference(&pixels, &box_average(w, h, channels, &full, 2));
        assert!(mean < 2.5, "{mean}");
    }
}

#[test]
fn a_progressive_jpeg_by_jpeg_encoder_downscales_close_to_a_box_average() {
    for source in [
        Source::rgb(203, 117, F_2_2),
        Source::rgb(96, 64, F_2_1),
        Source::gray(77, 51),
    ] {
        let source = Source {
            progressive: true,
            ..source
        };
        let bytes = source.encode();
        let (w, h, full) = decode(&bytes);
        let channels = if source.gray { 1 } else { 3 };
        let bits = if source.gray { 8 } else { 24 };
        for factor in [2usize, 8] {
            let (ow, oh) = (w.div_ceil(factor), h.div_ceil(factor));
            let (done, out) = shrink(
                &bytes,
                bound(u32::try_from(ow).unwrap(), u32::try_from(oh).unwrap(), bits),
            )
            .unwrap();
            assert_eq!(done.factor as usize, factor);
            let (_, _, pixels) = decode(&out);
            let want = box_average(w, h, channels, &full, factor);
            let (mean, max) = difference(&luma(&pixels, channels), &luma(&want, channels));
            assert!(
                mean < 2.5,
                "{w}x{h} factor {factor}: luma mean {mean}, largest {max}"
            );
        }
    }
}

#[test]
fn cmyk_and_ycck_are_refused() {
    for colour in [
        jpeg_encoder::ColorType::Cmyk,
        jpeg_encoder::ColorType::CmykAsYcck,
    ] {
        let mut bytes = Vec::new();
        jpeg_encoder::Encoder::new(&mut bytes, 90)
            .encode(&[128; 64 * 64 * 4], 64, 64, colour)
            .unwrap();
        let refused = shrink(&bytes, bound(32, 32, 24)).unwrap_err();
        assert!(matches!(refused, Refused::Cmyk), "{refused}");
    }
}

#[test]
fn twelve_bit_samples_are_refused() {
    let mut bytes = Source::gray(64, 64).encode();
    let at = bytes.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    bytes[at + 4] = 12;
    let refused = shrink(&bytes, bound(32, 32, 8)).unwrap_err();
    assert!(matches!(refused, Refused::Precision(12)), "{refused}");
}

#[test]
fn a_damaged_jpeg_is_refused() {
    let good = Source::rgb(128, 128, F_2_2).encode();
    let cut = &good[..good.len() / 2];
    let refused = shrink(cut, bound(64, 64, 24)).unwrap_err();
    assert!(matches!(refused, Refused::Damaged(_)), "cut: {refused}");

    let refused = shrink(b"not a jpeg", 1).unwrap_err();
    assert!(matches!(refused, Refused::Damaged(_)), "{refused}");

    // A restart marker out of sequence.
    let bytes = Source {
        restart: 1,
        ..Source::gray(64, 64)
    }
    .encode();
    let mut wrong = bytes;
    let at = wrong.windows(2).position(|w| w == [0xFF, 0xD1]).unwrap();
    wrong[at + 1] = 0xD5;
    let refused = shrink(&wrong, bound(32, 32, 8)).unwrap_err();
    assert!(matches!(refused, Refused::Damaged(_)), "RST: {refused}");
}

#[test]
fn no_factor_past_the_largest_is_refused() {
    let refused = shrink(&Source::gray(64, 64).encode(), 0).unwrap_err();
    assert!(matches!(refused, Refused::Bound), "{refused}");
}

#[test]
fn the_scaled_idct_of_a_dc_only_block_is_its_mean_and_rounds() {
    let m = decode::kernel(4);
    let mut c = [0f32; 64];
    c[0] = 8.0 * 50.0; // a block of 178
    let mut out = [0u8; 16];
    decode::idct(&c, true, 4, &m, &mut out, 4);
    assert!(out.iter().all(|&v| v == 178), "{out:?}");
}

#[test]
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the names are this repository's own and lowercase"
)]
fn the_corpus_jpegs_are_scaled_as_libjpeg_scales_them() {
    // Real encoders' output (libjpeg-turbo, through Pillow): smartart-pictures' 4:2:0 pictures of odd sizes (61 × 71,
    // 195 × 129, 129 × 195, 97 × 61) at qualities 75 and 80, case-study-pictures' 4:4:4 ones at 94 and 97.
    // `jpeg-decoder`'s own DCT scaling (`Decoder::scale`, libjpeg's reduced IDCT) is the reference for the decode:
    // the luma ours decodes at 1/2, 1/4 and 1/8 is the same. The re-encoded output then differs from it by the
    // requantisation at the source's own tables, as any re-encoding at the source's quality does: measured, a mean
    // of 0.57 to 1.20 levels of luma for the quality 94 and 97 pictures, 1.6 to 4.4 for the quality 75 and 80 ones.
    use std::io::Read as _;
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/documents/features");
    let mut seen = 0;
    for document in ["smartart-pictures.docx", "case-study-pictures.docx"] {
        let mut archive =
            ::zip::ZipArchive::new(std::fs::File::open(root.join(document)).unwrap()).unwrap();
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            if !file.name().unwrap().ends_with(".jpeg") {
                continue;
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            let (w, h, _) = decode(&bytes);
            for factor in [2usize, 4, 8] {
                let what = format!("{document} {} factor {factor}", file.name().unwrap());
                let (ow, oh) = (w.div_ceil(factor), h.div_ceil(factor));
                let scaled = first_component(&bytes, Some((ow, oh)));

                let (_, y) = decode_component(&bytes, factor, 0).unwrap();
                let (mean, max) = difference(&y, &scaled);
                // Within a level: at 1/8 jpeg-decoder truncates `DC / 8` where libjpeg (and this) rounds.
                assert!(
                    mean < 0.6 && max <= 1,
                    "{what}: decoded luma mean difference {mean}, largest {max}"
                );

                let (done, out) = shrink(
                    &bytes,
                    bound(u32::try_from(ow).unwrap(), u32::try_from(oh).unwrap(), 24),
                )
                .unwrap_or_else(|e| panic!("{what}: {e}"));
                assert_eq!(done.factor as usize, factor, "{what}");
                let (mean, max) = difference(&first_component(&out, None), &scaled);
                assert!(
                    mean < 6.0,
                    "{what}: re-encoded luma mean difference {mean}, largest {max}"
                );
            }
            seen += 1;
        }
    }
    assert_eq!(seen, 6);
}

#[test]
fn at_full_scale_the_decoder_gives_the_first_component_jpeg_decoder_does() {
    use std::io::Read as _;
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/documents/features");
    let mut archive =
        ::zip::ZipArchive::new(std::fs::File::open(root.join("smartart-pictures.docx")).unwrap())
            .unwrap();
    let mut sources = vec![
        Source::rgb(203, 117, F_2_2).encode(),
        Source::gray(61, 71).encode(),
    ];
    for name in [
        "word/media/image1.jpeg",
        "word/media/image3.jpeg",
        "word/media/image4.jpeg",
    ] {
        let mut bytes = Vec::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        sources.push(bytes);
    }
    for bytes in sources {
        let full = first_component(&bytes, None);
        let (head, ours) = decode_component(&bytes, 1, 0).unwrap();
        let (mean, max) = difference(&ours, &full);
        assert!(
            max <= 1 && mean < 0.05,
            "{}x{}: mean {mean}, largest {max}",
            head.width,
            head.height
        );
    }
}

// Damaged and hostile inputs end in a refusal, never a panic, in time bounded by the input.

/// A hand-made gray baseline JPEG of `width × height`: quantisers of 1, a DC table of `dc` (counts, values), an AC
/// table holding only EOB as `0`, then the entropy-coded `scan` as it is and `EOI`.
fn handmade(width: u16, height: u16, dc: (&[u8; 16], &[u8]), scan: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    let mut segment = |marker: u8, data: &[u8]| {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&(u16::try_from(data.len()).unwrap() + 2).to_be_bytes());
        out.extend_from_slice(data);
    };
    segment(0xDB, &[[0u8].as_slice(), &[1; 64]].concat());
    let mut frame = vec![8];
    frame.extend_from_slice(&height.to_be_bytes());
    frame.extend_from_slice(&width.to_be_bytes());
    frame.extend_from_slice(&[1, 1, 0x11, 0]);
    segment(0xC0, &frame);
    segment(0xC4, &[[0x00].as_slice(), dc.0, dc.1].concat());
    let mut ac = [0u8; 16];
    ac[0] = 1;
    segment(0xC4, &[[0x10].as_slice(), &ac, &[0x00]].concat());
    segment(0xDA, &[1, 1, 0x00, 0, 63, 0]);
    out.extend_from_slice(scan);
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// A DC table whose only code, `0`, is `size`.
fn one_dc(size: u8) -> ([u8; 16], [u8; 1]) {
    let mut counts = [0u8; 16];
    counts[0] = 1;
    (counts, [size])
}

#[test]
fn a_huffman_table_with_more_codes_than_its_lengths_allow_is_damaged() {
    // Three codes of one bit: the third would start past the 9-bit lookup.
    let mut counts = [0u8; 16];
    counts[0] = 3;
    let bytes = handmade(16, 16, (&counts, &[0, 1, 2]), &[0; 8]);
    assert!(matches!(shrink(&bytes, 1), Err(Refused::Damaged(_))));
}

#[test]
fn a_scan_that_ends_long_before_its_last_mcu_is_damaged_not_padded_forever() {
    let (counts, values) = one_dc(0);
    // 4096²: 262 144 blocks of two bits each (DC `0`, EOB `0`) need 64 KiB; two bytes are there.
    let bytes = handmade(4096, 4096, (&counts, &values), &[0, 0]);
    assert!(
        matches!(shrink(&bytes, 1 << 20), Err(Refused::Damaged(ref why)) if why.contains("ends before")),
        "{:?}",
        shrink(&bytes, 1 << 20).map(|(d, _)| d)
    );
    // The largest frame there is, from 300 bytes, is refused at once rather than decoded from zeros.
    let started = std::time::Instant::now();
    let bytes = handmade(65535, 65535, (&counts, &values), &[0; 64]);
    assert!(matches!(shrink(&bytes, 1 << 20), Err(Refused::Damaged(_))));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn a_scan_cut_by_a_few_bytes_is_still_downscaled() {
    let (counts, values) = one_dc(0);
    // 64 × 64: 64 blocks of two bits need 16 bytes; 12 are there, the rest is read as zeros.
    let bytes = handmade(64, 64, (&counts, &values), &[0; 12]);
    let (done, _) = shrink(&bytes, 1024).unwrap();
    assert_eq!(done.factor, 2);
}

#[test]
fn a_dc_predictor_past_i32_does_not_panic() {
    // Every block adds 2047 to the predictor (DC `0` then eleven ones, EOB `0`: thirteen bits); 1 049 600 blocks
    // take it past i32::MAX.
    let (counts, values) = one_dc(11);
    let (width, height) = (8192u16, 8200u16);
    let blocks = usize::from(width / 8) * usize::from(height / 8);
    let mut scan = Vec::with_capacity(blocks * 13 / 8 + blocks / 4);
    let (mut acc, mut n) = (0u32, 0u32);
    for _ in 0..blocks {
        acc = acc << 13 | 0b0_1111_1111_1110; // `0`, eleven ones, `0`
        n += 13;
        while n >= 8 {
            n -= 8;
            let byte = (acc >> n).to_le_bytes()[0];
            scan.push(byte);
            if byte == 0xFF {
                scan.push(0);
            }
        }
        acc &= (1 << n) - 1;
    }
    if n > 0 {
        scan.push((acc << (8 - n)).to_le_bytes()[0] | u8::try_from((1 << (8 - n)) - 1).unwrap());
    }
    let bytes = handmade(width, height, (&counts, &values), &scan);
    let (done, _) = shrink(&bytes, 2_000_000).unwrap();
    assert_eq!(done.factor, 8);
}

// The APPn and COM segments kept for the output are bounded, so a picture cannot make the step hold them without end.

/// `jpeg` with `extra` segments (marker, payload) after its own `APPn` segments, before its tables.
fn with_segments(jpeg: &[u8], extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut at = 2;
    while (0xE0..=0xEF).contains(&jpeg[at + 1]) {
        at += 2 + usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
    }
    let mut out = jpeg[..at].to_vec();
    for (marker, data) in extra {
        out.extend_from_slice(&[0xFF, *marker]);
        out.extend_from_slice(&(u16::try_from(data.len()).unwrap() + 2).to_be_bytes());
        out.extend_from_slice(data);
    }
    out.extend_from_slice(&jpeg[at..]);
    out
}

/// A segment payload of `len` bytes starting with `signature`.
fn payload(signature: &[u8], len: usize) -> Vec<u8> {
    let mut data = signature.to_vec();
    data.resize(len, 0x5A);
    data
}

#[test]
fn segments_no_renderer_reads_are_dropped_past_their_budget_and_the_rendering_ones_kept() {
    let icc: Vec<u8> = (0..70_000u32).map(|i| (i * 7).to_le_bytes()[0]).collect();
    let exif = vec![
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
    ];
    let source = Source {
        icc: Some(icc.clone()),
        exif: Some(exif.clone()),
        density: Some(300),
        ..Source::rgb(128, 96, F_2_2)
    };
    // 40 comments of 65 533 bytes (2.5 MiB), a second Exif, an XMP packet, then Adobe's segment last of all: the
    // budget is spent by then, and Adobe's colour transform is kept anyway.
    let mut extra: Vec<(u8, Vec<u8>)> = (0..40u8).map(|i| (0xFE, vec![i; 65_533])).collect();
    extra.push((0xE1, payload(b"Exif\0\0MM", 60_000)));
    extra.push((0xE1, payload(b"http://ns.adobe.com/xap/1.0/\0", 60_000)));
    extra.push((0xEE, b"Adobe\0\x64\0\0\0\0\x01".to_vec()));
    let bytes = with_segments(&source.encode(), &extra);
    assert!(bytes.len() > 2 * KEPT_OTHER_MAX);

    let (_, out) = shrink(&bytes, bound(32, 24, 24)).unwrap();
    let mut decoder = jpeg_decoder::Decoder::new(out.as_slice());
    decoder.decode().unwrap();
    assert_eq!(decoder.icc_profile(), Some(icc), "the ICC profile whole");
    assert_eq!(decoder.exif_data(), Some(exif.as_slice()), "the first Exif");
    let kept = segments(&out);
    assert_eq!(&of(&kept, 0xE0)[0][..5], b"JFIF\0");
    assert_eq!(of(&kept, 0xEE), vec![b"Adobe\0\x64\0\0\0\0\x01".to_vec()]);
    let comments = of(&kept, 0xFE);
    assert_eq!(
        comments,
        (0..16u8).map(|i| vec![i; 65_533]).collect::<Vec<_>>(),
        "the comments that fit in the budget, in order"
    );
    let other: usize = comments.len() * 65_533
        + of(&kept, 0xE1)
            .iter()
            .filter(|d| d.as_slice() != [b"Exif\0\0".as_slice(), &exif].concat())
            .map(Vec::len)
            .sum::<usize>();
    assert!(
        other <= KEPT_OTHER_MAX,
        "{other} bytes of other segments kept"
    );
}

#[test]
fn an_icc_profile_of_more_than_255_chunks_keeps_the_placeholder() {
    let base = Source::rgb(64, 64, F_2_2).encode();
    let chunks = |n: u8| -> Vec<(u8, Vec<u8>)> {
        (1..=n)
            .map(|i| {
                (
                    0xE2,
                    [b"ICC_PROFILE\0".as_slice(), &[i, n], &[7; 100]].concat(),
                )
            })
            .collect()
    };
    assert!(shrink(&with_segments(&base, &chunks(255)), bound(32, 32, 24)).is_ok());
    let mut too_many = chunks(255);
    too_many.push((
        0xE2,
        [b"ICC_PROFILE\0".as_slice(), &[0, 255], &[7; 100]].concat(),
    ));
    let refused = shrink(&with_segments(&base, &too_many), bound(32, 32, 24)).unwrap_err();
    assert!(matches!(refused, Refused::Metadata(_)), "{refused}");
}
