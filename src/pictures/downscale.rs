//! A PNG past the bitmap threshold downscaled instead of replaced: by the smallest
//! power of two that brings LibreOffice's bitmap to at most the threshold, each output pixel the average of its
//! 2ⁿ × 2ⁿ block (alpha-weighted where there is alpha, so a transparent pixel lends no colour). The rest is kept as
//! it was: the colour type (a palette expanded to RGB, or RGBA with a `tRNS`; gray below 8 bits widened to 8), the
//! alpha, the `iCCP`, `sRGB`, `gAMA` and `cHRM` chunks; 16-bit samples are averaged at 16 bits and rounded to 8,
//! LibreOffice's depth. `pHYs` is divided by the factor, so the picture keeps its physical size.
//!
//! Memory is bounded by rows, never by the bitmap: the decoder hands one row at a time (it holds two raw rows to
//! unfilter and its inflate window), the accumulator holds one output row of sums, the encoder streams `IDAT`
//! chunks. An interlaced (Adam7) PNG cannot be read that way and is refused, as is a damaged one (a bad CRC or
//! Adler-32, a truncated stream); the caller replaces those with the placeholder.

use std::borrow::Cow;
use std::fmt;
use std::io::{self, BufRead, Read, Seek, Write};

use super::{Header, Kind};

/// The largest factor tried: a block of 2³⁰ pixels keeps the 16-bit, alpha-weighted sums within a `u64`.
pub const MAX_FACTOR: u32 = 1 << 15;

/// The smallest power of two (1, 2, 4, …) by which `header`'s picture, each side divided and rounded up, holds a
/// bitmap of at most `bound` bytes; `None` past [`MAX_FACTOR`].
pub fn factor(header: &Header, bound: u64) -> Option<u32> {
    let mut factor = 1u32;
    loop {
        let reduced = Header {
            width: header.width.div_ceil(factor),
            height: header.height.div_ceil(factor),
            ..*header
        };
        if reduced.bitmap() <= bound {
            return Some(factor);
        }
        if factor >= MAX_FACTOR {
            return None;
        }
        factor *= 2;
    }
}

/// What [`png()`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Downscaled {
    /// The power of two each side was divided by.
    pub factor: u32,
    /// The new width in pixels.
    pub width: u32,
    /// The new height in pixels.
    pub height: u32,
}

/// Why [`png()`] wrote nothing usable.
#[derive(Debug)]
pub enum Refused {
    /// Adam7: its rows arrive in seven passes, so it cannot be averaged a row at a time.
    Interlaced,
    /// No power of two up to [`MAX_FACTOR`] brings it under the bound.
    Bound,
    /// Not a PNG the decoder reads to its end: a bad CRC or Adler-32, a stream cut short, a malformed chunk.
    Damaged(png::DecodingError),
    /// The output could not be written.
    Output(io::Error),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Interlaced => f.write_str("interlaced (Adam7)"),
            Self::Bound => write!(f, "no factor up to {MAX_FACTOR} reaches the threshold"),
            Self::Damaged(error) => write!(f, "damaged: {error}"),
            Self::Output(error) => write!(f, "not written: {error}"),
        }
    }
}

impl From<png::DecodingError> for Refused {
    /// The decoder's errors are all the input's: a read error included, it is the entry that cannot be read.
    fn from(error: png::DecodingError) -> Self {
        Self::Damaged(error)
    }
}

impl From<png::EncodingError> for Refused {
    fn from(error: png::EncodingError) -> Self {
        match error {
            png::EncodingError::IoError(e) => Self::Output(e),
            other => Self::Output(io::Error::other(other.to_string())),
        }
    }
}

impl From<io::Error> for Refused {
    fn from(error: io::Error) -> Self {
        Self::Output(error)
    }
}

/// Reads the PNG `input` a row at a time and writes to `out` the same picture downscaled by [`factor`] for `bound`.
pub fn png<R: Read, W: Write>(input: R, out: W, bound: u64) -> Result<Downscaled, Refused> {
    let mut decoder = png::Decoder::new(Forward(io::BufReader::with_capacity(64 * 1024, input)));
    decoder.set_transformations(png::Transformations::EXPAND);
    decoder.ignore_checksums(false);
    let mut reader = decoder.read_info()?;
    let (colour, depth) = reader.output_color_type();
    let info = reader.info();
    if info.interlaced {
        return Err(Refused::Interlaced);
    }
    let alpha = matches!(
        colour,
        png::ColorType::GrayscaleAlpha | png::ColorType::Rgba
    );
    let bits = match colour {
        _ if alpha => 32,
        png::ColorType::Grayscale => 8,
        _ => 24,
    };
    let (width, height) = (info.width, info.height);
    let header = Header {
        kind: Kind::Png,
        width,
        height,
        bits,
    };
    let factor = factor(&header, bound).ok_or(Refused::Bound)?;
    let done = Downscaled {
        factor,
        width: width.div_ceil(factor),
        height: height.div_ceil(factor),
    };

    let mut kept = png::Info::with_size(done.width, done.height);
    kept.color_type = colour;
    kept.bit_depth = png::BitDepth::Eight;
    kept.srgb = info.srgb;
    kept.source_gamma = info.gama_chunk;
    kept.source_chromaticities = info.chrm_chunk;
    kept.icc_profile = info
        .icc_profile
        .as_ref()
        .map(|p| Cow::Owned(p.clone().into_owned()));
    kept.pixel_dims = info.pixel_dims.map(|d| png::PixelDimensions {
        xppu: (d.xppu / factor).max(1),
        yppu: (d.yppu / factor).max(1),
        unit: d.unit,
    });
    let mut encoder = png::Encoder::with_info(out, kept)?;
    encoder.set_compression(png::Compression::Fast);
    let mut header_written = encoder.write_header()?;
    let mut writer = header_written.stream_writer_with_size(64 * 1024)?;

    let mut row = Row::new(
        width,
        factor,
        colour.samples(),
        alpha,
        depth == png::BitDepth::Sixteen,
    );
    let mut read = 0u32;
    while let Some(input) = reader.next_row()? {
        row.add(input.data());
        read += 1;
        if read.is_multiple_of(factor) || read == height {
            writer.write_all(row.take())?;
        }
    }
    reader.finish()?;
    writer.finish()?;
    header_written.finish()?;
    Ok(done)
}

/// One output row being summed: per output pixel and sample, the sum of its block's samples (for colour under
/// alpha, also the alpha-weighted sum), and how many input rows it holds so far.
pub(super) struct Row {
    width: u32,
    factor: u32,
    channels: usize,
    alpha: bool,
    wide: bool,
    sums: Vec<u64>,
    weighted: Vec<u64>,
    rows: u32,
    out: Vec<u8>,
}

impl Row {
    pub(super) fn new(width: u32, factor: u32, channels: usize, alpha: bool, wide: bool) -> Self {
        let samples = width.div_ceil(factor) as usize * channels;
        Self {
            width,
            factor,
            channels,
            alpha,
            wide,
            sums: vec![0; samples],
            weighted: if alpha { vec![0; samples] } else { Vec::new() },
            rows: 0,
            out: vec![0; samples],
        }
    }

    /// Adds one decoded input row: `width × channels` samples of one byte, or two big-endian.
    ///
    /// `while` loops over slices, no iterator or `Vec` index per sample: an unoptimised test build streams a
    /// gigapixel through here, and those calls are not inlined there.
    pub(super) fn add(&mut self, data: &[u8]) {
        let (channels, factor, alpha, wide) =
            (self.channels, self.factor as usize, self.alpha, self.wide);
        self.rows += 1;
        let colours = if alpha { channels - 1 } else { channels };
        let size = if wide { 2 } else { 1 };
        let sample = |i: usize| -> u64 {
            if wide {
                u64::from(data[i]) << 8 | u64::from(data[i + 1])
            } else {
                u64::from(data[i])
            }
        };
        let sums: &mut [u64] = &mut self.sums;
        let weighted: &mut [u64] = &mut self.weighted;
        let width = self.width as usize;
        let mut x = 0;
        while x < width {
            let at = (x / factor) * channels;
            let first = x * channels * size;
            if !alpha && !wide {
                let mut c = 0;
                while c < channels {
                    sums[at + c] += u64::from(data[first + c]);
                    c += 1;
                }
            } else {
                let a = if alpha {
                    let a = sample(first + colours * size);
                    sums[at + colours] += a;
                    a
                } else {
                    0
                };
                let mut c = 0;
                while c < colours {
                    let v = sample(first + c * size);
                    sums[at + c] += v;
                    if alpha {
                        weighted[at + c] += v * a;
                    }
                    c += 1;
                }
            }
            x += 1;
        }
    }

    /// The finished output row at 8 bits, its sums cleared for the next.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "`to_eight` gets an average of samples, at most 65535 when wide (scaled to at most 255) and at \
                  most 255 otherwise"
    )]
    pub(super) fn take(&mut self) -> &[u8] {
        let (channels, factor, alpha) = (self.channels, self.factor as usize, self.alpha);
        let colours = if alpha { channels - 1 } else { channels };
        let to_eight = |v: u64| if self.wide { (v * 255 + 32767) / 65535 } else { v } as u8;
        let rows = self.rows as usize;
        let (sums, weighted, out): (&mut [u64], &mut [u64], &mut [u8]) =
            (&mut self.sums, &mut self.weighted, &mut self.out);
        let width = self.width as usize;
        let mut ox = 0;
        while ox * channels < out.len() {
            let n = ((width - ox * factor).min(factor) * rows) as u64;
            let at = ox * channels;
            let alpha_sum = if alpha { sums[at + colours] } else { 0 };
            let mut c = 0;
            while c < channels {
                let v = if c < colours && alpha_sum > 0 {
                    (weighted[at + c] + alpha_sum / 2) / alpha_sum
                } else {
                    (sums[at + c] + n / 2) / n
                };
                out[at + c] = to_eight(v);
                sums[at + c] = 0;
                if alpha {
                    weighted[at + c] = 0;
                }
                c += 1;
            }
            ox += 1;
        }
        self.rows = 0;
        &self.out
    }
}

/// A reader the decoder can take: it asks for `Seek` but reads straight through, so seeking is refused.
struct Forward<R>(R);

impl<R: Read> Read for Forward<io::BufReader<R>> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl<R: Read> BufRead for Forward<io::BufReader<R>> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.0.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.0.consume(amount);
    }
}

impl<R> Seek for Forward<R> {
    fn seek(&mut self, _: io::SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a picture is read straight through",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1024 * 1024;

    fn rgb(width: u32, height: u32) -> Header {
        Header {
            kind: Kind::Png,
            width,
            height,
            bits: 24,
        }
    }

    #[test]
    fn the_factor_is_the_smallest_power_of_two_under_the_bound() {
        // The corpus's px-256mpx: RGB 16 000², 768 000 000 bytes; 8 000² is 192 000 000, under 256 MiB.
        assert_eq!(factor(&rgb(16000, 16000), 256 * MIB), Some(2));
        // RGB 9 459² is the first past 256 MiB; halved it is far under.
        assert_eq!(factor(&rgb(9459, 9459), 256 * MIB), Some(2));
        assert_eq!(
            factor(&rgb(9458, 9458), 256 * MIB),
            Some(1),
            "already under"
        );
        // 1 gigapixel gray, 32 768²: 16 384² is exactly 256 MiB, which is at most the bound.
        let gray = Header {
            bits: 8,
            ..rgb(32768, 32768)
        };
        assert_eq!(factor(&gray, 256 * MIB), Some(2));
        assert_eq!(factor(&gray, 256 * MIB - 1), Some(4));
        // RGB 40 000² (4.8 GB) needs a quarter of each side: 10 000² is 300 000 000, past; 5 000² is not.
        assert_eq!(factor(&rgb(40000, 40000), 256 * MIB), Some(8));
        assert_eq!(
            factor(&rgb(1 << 20, 1 << 20), 3),
            None,
            "even 32² is past 3 bytes"
        );
    }

    #[test]
    fn the_factor_rounds_each_side_up_so_the_aspect_holds_within_a_pixel() {
        // 5 × 3 at 24 bits: 2 × 1 after a quarter, 3 × 2 after a half (scanline 9 → 12, 24 bytes).
        assert_eq!(factor(&rgb(5, 3), 24), Some(2));
        assert_eq!(factor(&rgb(5, 3), 23), Some(4));
    }

    /// A PNG encoded by the `png` crate from `pixels` (rows of samples, big-endian at 16 bits).
    struct Picture<'a> {
        width: u32,
        height: u32,
        colour: png::ColorType,
        depth: png::BitDepth,
        pixels: Vec<u8>,
        palette: Option<Vec<u8>>,
        trns: Option<Vec<u8>>,
        icc: Option<&'a [u8]>,
        srgb: bool,
        gamma: Option<png::ScaledFloat>,
        chromaticities: Option<png::SourceChromaticities>,
        dims: Option<png::PixelDimensions>,
    }

    impl Picture<'_> {
        fn new(width: u32, height: u32, colour: png::ColorType, depth: png::BitDepth) -> Self {
            Picture {
                width,
                height,
                colour,
                depth,
                pixels: Vec::new(),
                palette: None,
                trns: None,
                icc: None,
                srgb: false,
                gamma: None,
                chromaticities: None,
                dims: None,
            }
        }

        fn encode(&self) -> Vec<u8> {
            let mut info = png::Info::with_size(self.width, self.height);
            info.color_type = self.colour;
            info.bit_depth = self.depth;
            info.palette = self.palette.clone().map(Cow::Owned);
            info.trns = self.trns.clone().map(Cow::Owned);
            info.icc_profile = self.icc.map(|p| Cow::Owned(p.to_vec()));
            info.srgb = self.srgb.then_some(png::SrgbRenderingIntent::Perceptual);
            info.source_gamma = self.gamma;
            info.source_chromaticities = self.chromaticities;
            info.pixel_dims = self.dims;
            let mut out = Vec::new();
            let mut writer = png::Encoder::with_info(&mut out, info)
                .unwrap()
                .write_header()
                .unwrap();
            writer.write_image_data(&self.pixels).unwrap();
            writer.finish().unwrap();
            out
        }
    }

    /// What a test reads back from the output: the decoder's info, owned, and the pixels.
    struct Decoded {
        width: u32,
        height: u32,
        color_type: png::ColorType,
        bit_depth: png::BitDepth,
        icc_profile: Option<Vec<u8>>,
        gama_chunk: Option<png::ScaledFloat>,
        chrm_chunk: Option<png::SourceChromaticities>,
        srgb: Option<png::SrgbRenderingIntent>,
        pixel_dims: Option<png::PixelDimensions>,
    }

    fn decode(bytes: &[u8]) -> (Decoded, Vec<u8>) {
        let mut reader = png::Decoder::new(io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut pixels).unwrap();
        let info = reader.info();
        let decoded = Decoded {
            width: info.width,
            height: info.height,
            color_type: info.color_type,
            bit_depth: info.bit_depth,
            icc_profile: info.icc_profile.as_ref().map(|p| p.to_vec()),
            gama_chunk: info.gama_chunk,
            chrm_chunk: info.chrm_chunk,
            srgb: info.srgb,
            pixel_dims: info.pixel_dims,
        };
        (decoded, pixels)
    }

    /// Downscales `picture` under `bound` and decodes the result.
    fn shrink(picture: &[u8], bound: u64) -> (Downscaled, Decoded, Vec<u8>) {
        let mut out = Vec::new();
        let done = png(picture, &mut out, bound).unwrap();
        let (info, pixels) = decode(&out);
        (done, info, pixels)
    }

    /// The bitmap of `width × height` at `bits` a pixel: a bound that is reached exactly there.
    fn bound(width: u32, height: u32, bits: u8) -> u64 {
        Header {
            kind: Kind::Png,
            width,
            height,
            bits,
        }
        .bitmap()
    }

    use png::BitDepth::{Eight, Four, One, Sixteen, Two};
    use png::ColorType::{Grayscale, GrayscaleAlpha, Indexed, Rgb, Rgba};

    #[test]
    fn a_gray_block_is_averaged_and_rounded() {
        let mut p = Picture::new(2, 2, Grayscale, Eight);
        p.pixels = vec![0, 255, 255, 255]; // 765 / 4 = 191.25
        let (done, info, pixels) = shrink(&p.encode(), bound(1, 1, 8));
        assert_eq!(
            done,
            Downscaled {
                factor: 2,
                width: 1,
                height: 1
            }
        );
        assert_eq!((info.color_type, info.bit_depth), (Grayscale, Eight));
        assert_eq!(pixels, [191]);
    }

    #[test]
    fn odd_sides_are_rounded_up_and_their_edge_blocks_averaged_over_what_they_hold() {
        // 5 × 3 → 3 × 2: the last column's blocks are 1 pixel wide, the last row's 1 pixel high.
        let mut p = Picture::new(5, 3, Grayscale, Eight);
        #[rustfmt::skip]
        let rows = [
            [10, 20, 30, 40, 50],
            [30, 40, 50, 60, 70],
            [100, 101, 200, 201, 7],
        ];
        p.pixels = rows.concat();
        let (done, info, pixels) = shrink(&p.encode(), bound(3, 2, 8));
        assert_eq!(
            (done.width, done.height, info.width, info.height),
            (3, 2, 3, 2)
        );
        // (10+20+30+40)/4 = 25, (30+40+50+60)/4 = 45, (50+70)/2 = 60; then 100.5 → 101, 200.5 → 201, 7.
        assert_eq!(pixels, [25, 45, 60, 101, 201, 7]);
    }

    #[test]
    fn sixteen_bit_samples_are_averaged_at_sixteen_and_rounded_to_eight() {
        let mut p = Picture::new(2, 1, Rgb, Sixteen);
        // R: 0x0000 and 0x0101 → 0x0080.5 → 0x0081 at 16 → 0.50 at 8 → 1; G: 0xFFFF twice → 255; B: 0x8000, 0x8001
        // → 0x8001 → 128.
        p.pixels = vec![0, 0, 0xFF, 0xFF, 0x80, 0, 1, 1, 0xFF, 0xFF, 0x80, 1];
        let (_, info, pixels) = shrink(&p.encode(), bound(1, 1, 24));
        assert_eq!((info.color_type, info.bit_depth), (Rgb, Eight));
        assert_eq!(pixels, [1, 255, 128]);
    }

    #[test]
    fn alpha_weighs_the_colour_and_is_averaged_itself() {
        let mut p = Picture::new(2, 1, Rgba, Eight);
        // Opaque red beside fully transparent blue: the colour is red's, the alpha half.
        p.pixels = vec![255, 0, 0, 255, 0, 0, 255, 0];
        let (_, info, pixels) = shrink(&p.encode(), bound(1, 1, 32));
        assert_eq!((info.color_type, info.bit_depth), (Rgba, Eight));
        assert_eq!(pixels, [255, 0, 0, 128]);

        let mut p = Picture::new(2, 1, GrayscaleAlpha, Sixteen);
        p.pixels = vec![0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]; // transparent black, opaque white
        let (_, info, pixels) = shrink(&p.encode(), bound(1, 1, 32));
        assert_eq!((info.color_type, info.bit_depth), (GrayscaleAlpha, Eight));
        assert_eq!(pixels, [255, 128]);
    }

    /// A picture whose every 2 × 2 block is one colour, so the downscale must give back that colour exactly.
    fn blocky(
        colour: png::ColorType,
        depth: png::BitDepth,
        sample: impl Fn(u32, u32, usize) -> u16,
    ) -> Picture<'static> {
        let (width, height) = (8u32, 6u32);
        let channels = colour.samples();
        let mut p = Picture::new(width, height, colour, depth);
        let bits = depth as usize;
        for y in 0..height {
            let mut row = Vec::new();
            let mut acc = 0u16;
            let mut filled = 0;
            for x in 0..width {
                for c in 0..channels {
                    let v = sample(x / 2, y / 2, c);
                    if bits == 16 {
                        row.extend_from_slice(&v.to_be_bytes());
                    } else if bits == 8 {
                        row.push(u8::try_from(v).unwrap());
                    } else {
                        acc = (acc << bits) | v;
                        filled += bits;
                        if filled == 8 {
                            row.push(u8::try_from(acc).unwrap());
                            acc = 0;
                            filled = 0;
                        }
                    }
                }
            }
            if filled > 0 {
                row.push(u8::try_from(acc << (8 - filled)).unwrap());
            }
            p.pixels.extend_from_slice(&row);
        }
        p
    }

    /// Source colour type and depth, its `PLTE` and `tRNS`, the colour type expected out and LibreOffice's bits.
    type Case = (
        png::ColorType,
        png::BitDepth,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        png::ColorType,
        u8,
    );

    #[test]
    fn every_colour_type_and_depth_keeps_its_kind_at_eight_bits() {
        let cases: Vec<Case> = vec![
            (Grayscale, One, None, None, Grayscale, 8),
            (Grayscale, Two, None, None, Grayscale, 8),
            (Grayscale, Four, None, None, Grayscale, 8),
            (Grayscale, Eight, None, None, Grayscale, 8),
            (Grayscale, Sixteen, None, None, Grayscale, 8),
            (Grayscale, Eight, None, Some(vec![0, 3]), GrayscaleAlpha, 32), // gray 3 transparent
            (Rgb, Eight, None, None, Rgb, 24),
            (Rgb, Sixteen, None, None, Rgb, 24),
            (
                Indexed,
                Two,
                Some(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]),
                None,
                Rgb,
                24,
            ),
            (
                Indexed,
                Eight,
                Some(vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]),
                Some(vec![255, 0]),
                Rgba,
                32,
            ),
            (GrayscaleAlpha, Eight, None, None, GrayscaleAlpha, 32),
            (GrayscaleAlpha, Sixteen, None, None, GrayscaleAlpha, 32),
            (Rgba, Eight, None, None, Rgba, 32),
            (Rgba, Sixteen, None, None, Rgba, 32),
        ];
        for (colour, depth, palette, trns, expected, bits) in cases {
            let max = (1u32 << (depth as u32)) - 1;
            let alpha_channel =
                matches!(colour, GrayscaleAlpha | Rgba).then(|| colour.samples() - 1);
            let sample = |bx: u32, by: u32, c: usize| -> u16 {
                if Some(c) == alpha_channel {
                    return u16::try_from(max).unwrap(); // opaque, so the colour is a plain average
                }
                match colour {
                    Indexed => ((bx + by) % 4) as u16,
                    _ => {
                        u16::try_from((bx * 7 + by * 3 + u32::try_from(c).unwrap() * 5) % (max + 1))
                            .unwrap()
                    }
                }
            };
            let mut p = blocky(colour, depth, sample);
            p.palette = palette.clone();
            p.trns = trns.clone();
            let (done, info, pixels) = shrink(&p.encode(), bound(4, 3, bits));
            let what = format!("{colour:?} {depth:?}");
            assert_eq!(done.factor, 2, "{what}");
            assert_eq!((info.width, info.height), (4, 3), "{what}");
            assert_eq!(
                (info.color_type, info.bit_depth),
                (expected, Eight),
                "{what}"
            );
            // The original decoded with the same expansion, sampled at each block's corner, is the output.
            let mut reader = png::Decoder::new(io::Cursor::new(p.encode()));
            reader.set_transformations(png::Transformations::EXPAND);
            let mut reader = reader.read_info().unwrap();
            let mut full = vec![0; reader.output_buffer_size().unwrap()];
            reader.next_frame(&mut full).unwrap();
            let channels = expected.samples();
            let wide = depth == Sixteen;
            let step = if wide { 2 } else { 1 };
            let to8 = |v: u32| if wide { (v * 255 + 32767) / 65535 } else { v };
            let mut want = Vec::new();
            for oy in 0..3usize {
                for ox in 0..4usize {
                    for c in 0..channels {
                        let at = ((oy * 2 * 8 + ox * 2) * channels + c) * step;
                        let v = if wide {
                            u32::from(u16::from_be_bytes([full[at], full[at + 1]]))
                        } else {
                            u32::from(full[at])
                        };
                        want.push(u8::try_from(to8(v)).unwrap());
                    }
                }
            }
            assert_eq!(pixels, want, "{what}");
        }
    }

    #[test]
    fn the_colour_chunks_are_kept_and_the_density_follows_the_factor() {
        let icc = b"a stand-in ICC profile, kept byte for byte".repeat(4);
        let mut p = Picture::new(4, 4, Rgb, Eight);
        p.pixels = vec![9; 48];
        p.icc = Some(&icc);
        p.gamma = Some(png::ScaledFloat::from_scaled(45455));
        p.chromaticities = Some(png::SourceChromaticities::new(
            (0.3127, 0.329),
            (0.64, 0.33),
            (0.3, 0.6),
            (0.15, 0.06),
        ));
        p.dims = Some(png::PixelDimensions {
            xppu: 11811,
            yppu: 5905,
            unit: png::Unit::Meter,
        });
        let (_, info, _) = shrink(&p.encode(), bound(2, 2, 24));
        assert_eq!(info.icc_profile.as_deref(), Some(icc.as_slice()));
        assert_eq!(info.gama_chunk, p.gamma);
        assert_eq!(info.chrm_chunk, p.chromaticities);
        let dims = info.pixel_dims.expect("pHYs kept");
        assert_eq!(
            (dims.xppu, dims.yppu, dims.unit),
            (5905, 2952, png::Unit::Meter)
        );

        let mut p = Picture::new(4, 4, Rgb, Eight);
        p.pixels = vec![9; 48];
        p.srgb = true;
        let (_, info, _) = shrink(&p.encode(), bound(2, 2, 24));
        assert_eq!(info.srgb, Some(png::SrgbRenderingIntent::Perceptual));
    }

    fn plain(width: u32, height: u32) -> Vec<u8> {
        let mut p = Picture::new(width, height, Rgb, Eight);
        p.pixels = (0..width * height * 3)
            .map(|i| i.to_le_bytes()[0])
            .collect();
        p.encode()
    }

    #[test]
    fn an_interlaced_png_is_refused() {
        let mut bytes = plain(8, 8);
        bytes[8 + 8 + 12] = 1; // IHDR's interlace method: Adam7
        let crc = crc32fast::hash(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&crc.to_be_bytes());
        let refused = png(bytes.as_slice(), Vec::new(), bound(4, 4, 24)).unwrap_err();
        assert!(matches!(refused, Refused::Interlaced), "{refused}");
    }

    #[test]
    fn a_damaged_png_is_refused() {
        let good = plain(64, 64);
        let cut = &good[..good.len() / 2];
        let refused = png(cut, Vec::new(), bound(32, 32, 24)).unwrap_err();
        assert!(matches!(refused, Refused::Damaged(_)), "cut: {refused}");

        let mut flipped = good.clone();
        let middle = flipped.len() / 2;
        flipped[middle] ^= 0x55; // inside IDAT: its CRC no longer matches
        let refused = png(flipped.as_slice(), Vec::new(), bound(32, 32, 24)).unwrap_err();
        assert!(matches!(refused, Refused::Damaged(_)), "flipped: {refused}");

        let refused = png(&b"not a png"[..], Vec::new(), 1).unwrap_err();
        assert!(matches!(refused, Refused::Damaged(_)), "{refused}");
    }

    #[test]
    fn no_factor_past_the_largest_is_refused() {
        let refused = png(plain(8, 8).as_slice(), Vec::new(), 0).unwrap_err();
        assert!(matches!(refused, Refused::Bound), "{refused}");
    }
}
