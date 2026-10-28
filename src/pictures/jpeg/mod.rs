//! A JPEG past the bitmap threshold downscaled instead of replaced: by the smallest
//! power of two that brings LibreOffice's bitmap to at most the threshold. Up to 8 the reduction is DCT scaling while
//! decoding: each 8 × 8 block's lowest 4 × 4, 2 × 2 or 1 × 1 coefficients give the block at a half, a quarter or an
//! eighth of its side, the way libjpeg's `scale_denom` does. Past 8 the eighth is averaged over 2ⁿ × 2ⁿ blocks.
//!
//! No visible loss (the user, 2026-10-24: «quality такой же должен остаться»): the picture never leaves its own
//! components (no colour conversion, no chroma upsampling), keeps their ids and sampling factors, so its chroma
//! subsampling, and is re-encoded with the source's own quantisation tables, copied, so its quality is the source's
//! by construction rather than estimated. The `APPn` segments that change how it looks are copied whole: the ICC
//! profile (`APP2`, up to its 255 chunks), Exif with its orientation (`APP1`), Adobe's colour transform (`APP14`);
//! JFIF's density (`APP0`) is divided by the factor, so the picture keeps its physical size. The other `APPn` and `COM`
//! segments (XMP, Photoshop's, comments) are copied up to [`KEPT_OTHER_MAX`] bytes and dropped past it,
//! so the segments held cost about 17 MiB at the most. Huffman tables are the standard ones (ITU T.81 Annex K).
//!
//! Memory is bounded by MCU rows, never by the bitmap: the decoder ([`decode`]) holds one MCU row of coefficients and
//! scaled samples, the encoder ([`encode`]) one MCU row of output rows per component. A sequential JPEG whose
//! components share one scan is decoded as it streams in. One with several scans (progressive, or a sequential one with
//! its components in separate scans) is read twice: first through ([`index`]), its entropy-coded data
//! copied to a scratch file and each scan's start, band and tables noted; then from that file by one cursor a scan,
//! all of them run over each MCU row in file order before the row is turned into samples. Every scan revisits every
//! block, but no scan needs more than the block it is at, so the coefficients held are one MCU row's, as for a single
//! scan; the disk holds the entropy-coded data once.
//!
//! The others are refused and replaced by the placeholder: arithmetic, lossless or hierarchical coding, 12-bit samples,
//! CMYK or YCCK, more than [`index::MAX_SCANS`] scans, an ICC profile in more than 255 chunks, and a damaged one.

mod decode;
mod encode;
mod index;

use std::fmt;
use std::io::{self, Read, Write};

use super::downscale::{Downscaled, Row, factor};
use super::{Header, Kind};

/// Why [`downscale`] wrote nothing usable; the caller keeps the placeholder.
#[derive(Debug)]
pub enum Refused {
    /// Arithmetic, lossless or hierarchical coding: this `SOFn`.
    Coding(u8),
    /// Samples of this many bits; LibreOffice and this codec read 8.
    Precision(u8),
    /// Four components: CMYK or YCCK (replaced, not downscaled).
    Cmyk,
    /// Neither gray nor three components.
    Components(u8),
    /// More scans than [`index::MAX_SCANS`].
    TooManyScans,
    /// No power of two up to the largest factor reaches the threshold.
    Bound,
    /// Its `APPn` segments could not be kept whole: why.
    Metadata(String),
    /// Not a JPEG this decoder reads: a bad marker, table or Huffman code, a stream cut short.
    Damaged(String),
    /// The output could not be written.
    Output(io::Error),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coding(sof) => write!(
                f,
                "arithmetic, lossless or hierarchical coding (SOF{})",
                sof - 0xC0
            ),
            Self::Precision(bits) => write!(f, "{bits}-bit samples"),
            Self::Cmyk => f.write_str("CMYK or YCCK (four components)"),
            Self::Components(n) => write!(f, "{n} components"),
            Self::TooManyScans => write!(f, "more than {} scans", index::MAX_SCANS),
            Self::Bound => write!(
                f,
                "no factor up to {} reaches the threshold",
                super::downscale::MAX_FACTOR
            ),
            Self::Metadata(why) => write!(f, "metadata not kept whole: {why}"),
            Self::Damaged(why) => write!(f, "damaged: {why}"),
            Self::Output(error) => write!(f, "not written: {error}"),
        }
    }
}

impl From<io::Error> for Refused {
    fn from(error: io::Error) -> Self {
        Self::Output(error)
    }
}

fn damaged(why: impl Into<String>) -> Refused {
    Refused::Damaged(why.into())
}

/// The natural (row-major) index of each zigzag position.
pub(crate) const UNZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// A frame component: its id, sampling factors and quantisation table.
#[derive(Debug, Clone, Copy)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
}

/// A quantisation table as `DQT` carries it: its precision (0 for 8 bits, 1 for 16) and values in zigzag order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Quant {
    precision: u8,
    zigzag: [u16; 64],
}

/// Everything before the scan's entropy-coded data.
struct Head {
    /// `SOF0`, `SOF1` or `SOF2`.
    sof: u8,
    width: usize,
    height: usize,
    components: Vec<Component>,
    quant: [Option<Quant>; 4],
    dc: [Option<decode::Huffman>; 4],
    ac: [Option<decode::Huffman>; 4],
    restart: usize,
    /// `APPn` and `COM` segments: marker and payload.
    kept: Vec<(u8, Vec<u8>)>,
}

/// How a frame of `width × height` with `components` lays out in blocks and MCUs.
#[derive(Debug, Clone)]
struct Layout {
    mcus_x: usize,
    mcus_y: usize,
    /// Per component: blocks a side in one MCU (1 × 1 when it is alone in its scan), its width and height in
    /// samples, and the width of an MCU row in blocks.
    parts: Vec<Part>,
}

#[derive(Debug, Clone, Copy)]
struct Part {
    h: usize,
    v: usize,
    width: usize,
    height: usize,
    blocks_x: usize,
}

impl Layout {
    fn new(width: usize, height: usize, components: &[Component]) -> Self {
        let h_max = components.iter().map(|c| c.h).max().unwrap_or(1);
        let v_max = components.iter().map(|c| c.v).max().unwrap_or(1);
        let size = |c: &Component| {
            (
                (width * c.h).div_ceil(h_max),
                (height * c.v).div_ceil(v_max),
            )
        };
        if components.len() == 1 {
            // A single component is coded non-interleaved: one block an MCU, whatever its sampling factors say.
            let (w, h) = size(&components[0]);
            let blocks_x = w.div_ceil(8);
            return Self {
                mcus_x: blocks_x,
                mcus_y: h.div_ceil(8),
                parts: vec![Part {
                    h: 1,
                    v: 1,
                    width: w,
                    height: h,
                    blocks_x,
                }],
            };
        }
        let mcus_x = width.div_ceil(8 * h_max);
        let mcus_y = height.div_ceil(8 * v_max);
        let parts = components
            .iter()
            .map(|c| {
                let (w, h) = size(c);
                Part {
                    h: c.h,
                    v: c.v,
                    width: w,
                    height: h,
                    blocks_x: mcus_x * c.h,
                }
            })
            .collect();
        Self {
            mcus_x,
            mcus_y,
            parts,
        }
    }
}

/// Reads the JPEG `input` an MCU row at a time and writes to `out` the same picture downscaled by [`factor`] for
/// `bound`; a picture of several scans copies them to a scratch file in TMPDIR ([`downscale_in`] says where).
pub fn downscale<R: Read, W: Write>(input: R, out: W, bound: u64) -> Result<Downscaled, Refused> {
    downscale_in(input, out, bound, &std::env::temp_dir())
}

/// [`downscale`] with a picture of several scans copying them to a scratch file in `scratch`, the `/tmp` the sizing
/// reserves rather than `TMPDIR`, removed when it returns.
pub fn downscale_in<R: Read, W: Write>(
    input: R,
    out: W,
    bound: u64,
    scratch: &std::path::Path,
) -> Result<Downscaled, Refused> {
    let mut source = decode::Source::new(input);
    let (mut head, first) = read_head(&mut source)?;
    let header = Header {
        kind: Kind::Jpeg,
        width: u32::try_from(head.width).expect("a JPEG dimension is a u16"),
        height: u32::try_from(head.height).expect("a JPEG dimension is a u16"),
        bits: if head.components.len() == 1 { 8 } else { 24 },
    };
    let factor = factor(&header, bound).ok_or(Refused::Bound)?;
    let done = Downscaled {
        factor,
        width: header.width.div_ceil(factor),
        height: header.height.div_ceil(factor),
    };
    let progressive = head.sof == 0xC2;
    if !progressive && first.comps.len() == head.components.len() {
        // One scan: decoded as it streams in.
        let mut cursors = [decode::Cursor::new(first, false, &mut source)];
        transcode(&head, out, &done, &mut cursors)?;
        return Ok(done);
    }
    let (spool, scans) = index::read(&mut source, &mut head, first, scratch)?;
    let mut cursors: Vec<_> = scans
        .into_iter()
        .map(|(offset, spec)| {
            let at = index::Spooled::new(&spool, offset);
            let source = Box::new(decode::Source::with_capacity(at, index::CURSOR_BUFFER));
            decode::Cursor::new(spec, progressive, source)
        })
        .collect();
    transcode(&head, out, &done, &mut cursors)?;
    Ok(done)
}

/// Test-only: the samples of `component` of the JPEG `bytes` decoded at `1/scale`, row after row, and its head.
#[cfg(test)]
fn decode_component(
    bytes: &[u8],
    scale: usize,
    component: usize,
) -> Result<(Head, Vec<u8>), Refused> {
    let mut source = decode::Source::new(bytes);
    let (mut head, first) = read_head(&mut source)?;
    let layout = Layout::new(head.width, head.height, &head.components);
    let mut out = Vec::new();
    let mut keep = |c: usize, row: &[u8]| {
        if c == component {
            out.extend_from_slice(row);
        }
        Ok(())
    };
    let progressive = head.sof == 0xC2;
    if !progressive && first.comps.len() == head.components.len() {
        let mut cursors = [decode::Cursor::new(first, false, &mut source)];
        decode::rows(&head, &layout, scale, &mut cursors, &mut keep)?;
    } else {
        let (spool, scans) = index::read(&mut source, &mut head, first, &std::env::temp_dir())?;
        let mut cursors: Vec<_> = scans
            .into_iter()
            .map(|(offset, spec)| {
                let at = index::Spooled::new(&spool, offset);
                let source = Box::new(decode::Source::with_capacity(at, index::CURSOR_BUFFER));
                decode::Cursor::new(spec, progressive, source)
            })
            .collect();
        decode::rows(&head, &layout, scale, &mut cursors, &mut keep)?;
    }
    Ok((head, out))
}

/// Decodes the picture from `cursors` (its scans in order) at the scale `done` says and writes it re-encoded to `out`.
fn transcode<W: Write, R: Read, S: std::ops::DerefMut<Target = decode::Source<R>>>(
    head: &Head,
    out: W,
    done: &Downscaled,
    cursors: &mut [decode::Cursor<S>],
) -> Result<(), Refused> {
    let factor = done.factor;
    let scale = factor.min(8) as usize;
    let group = factor / factor.min(8);
    let source_layout = Layout::new(head.width, head.height, &head.components);
    let mut encoder =
        encode::Encoder::start(out, head, done.width as usize, done.height as usize, factor)?;
    let mut averages: Vec<(Row, u32, u32)> = source_layout
        .parts
        .iter()
        .map(|p| {
            let width =
                u32::try_from(p.width.div_ceil(scale)).expect("at most a JPEG dimension, a u16");
            let height =
                u32::try_from(p.height.div_ceil(scale)).expect("at most a JPEG dimension, a u16");
            (Row::new(width, group, 1, false, false), 0, height)
        })
        .collect();
    decode::rows(head, &source_layout, scale, cursors, |component, row| {
        if group == 1 {
            return encoder.push_row(component, row);
        }
        let (average, seen, height) = &mut averages[component];
        average.add(row);
        *seen += 1;
        if seen.is_multiple_of(group) || *seen == *height {
            encoder.push_row(component, average.take())?;
        }
        Ok(())
    })?;
    encoder.finish()?;
    Ok(())
}

/// Reads the markers up to the first scan's header: the tables, the frame, the segments to keep; and that header.
fn read_head<R: Read>(source: &mut decode::Source<R>) -> Result<(Head, decode::ScanSpec), Refused> {
    if source.byte()? != 0xFF || source.byte()? != 0xD8 {
        return Err(damaged("no SOI"));
    }
    let mut head = Head {
        sof: 0,
        width: 0,
        height: 0,
        components: Vec::new(),
        quant: [None; 4],
        dc: Default::default(),
        ac: Default::default(),
        restart: 0,
        kept: Vec::new(),
    };
    let mut keep = Keep::default();
    loop {
        if source.byte()? != 0xFF {
            return Err(damaged("a marker expected"));
        }
        let mut marker = source.byte()?;
        while marker == 0xFF {
            marker = source.byte()?;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            continue;
        }
        if marker == 0xD8 || marker == 0xD9 {
            return Err(damaged("no scan"));
        }
        let length = usize::from(source.u16()?);
        if length < 2 {
            return Err(damaged("a segment shorter than its length"));
        }
        if matches!(marker, 0xE0..=0xEF | 0xFE) {
            keep.segment(source, &mut head, marker, length - 2)?;
            continue;
        }
        let data = source.bytes(length - 2)?;
        match marker {
            0xDB => read_quant(&mut head, &data)?,
            0xC4 => read_huffman(&mut head, &data)?,
            0xDD => {
                let at = data.get(..2).ok_or_else(|| damaged("a short DRI"))?;
                head.restart = usize::from(u16::from_be_bytes([at[0], at[1]]));
            }
            0xC0..=0xC2 => read_frame(&mut head, marker, &data)?,
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return Err(Refused::Coding(marker)),
            0xDA => {
                let scan = read_scan(&head, &data)?;
                return Ok((head, scan));
            }
            _ => {} // DNL, DAC, DHP, EXP, JPGn: nothing this reads
        }
    }
}

/// The most `APPn` and `COM` bytes kept that no renderer reads (XMP, Photoshop's resources, comments, a second
/// Exif): past it, such segments are skipped unread.
pub const KEPT_OTHER_MAX: usize = 1024 * 1024;

/// The most `APP2` chunks an ICC profile spans: its chunk counter is one byte.
pub const ICC_CHUNKS_MAX: usize = 255;

/// What [`read_head`] has kept so far: which rendering segments it has seen, and the other bytes it holds.
#[derive(Default)]
struct Keep {
    jfif: bool,
    exif: bool,
    adobe: bool,
    icc_chunks: usize,
    other: usize,
}

impl Keep {
    /// Keeps, skips or refuses the `APPn` or `COM` segment `marker` whose `len` payload bytes `source` is at.
    ///
    /// Kept whole, whatever their size, are the segments that change how the picture looks: every ICC profile chunk
    /// (colour), the first JFIF (its density, so its physical size), the first Exif (its orientation) and the first
    /// Adobe segment (its colour transform). Each is at most 65 533 bytes and a profile at most 255 chunks, so they hold
    /// about 16 MiB at the most; a profile in more chunks is malformed and refused rather than cut, which would change
    /// its colour. The others are kept while they fit in [`KEPT_OTHER_MAX`] and skipped past it: no renderer draws them.
    fn segment<R: Read>(
        &mut self,
        source: &mut decode::Source<R>,
        head: &mut Head,
        marker: u8,
        len: usize,
    ) -> Result<(), Refused> {
        let signature = source.peek(len.min(12))?;
        let first = |seen: &mut bool| !std::mem::replace(seen, true);
        let rendering = match marker {
            0xE0 if signature.starts_with(b"JFIF\0") => first(&mut self.jfif),
            0xE1 if signature.starts_with(b"Exif\0") => first(&mut self.exif),
            0xEE if signature.starts_with(b"Adobe") => first(&mut self.adobe),
            0xE2 if signature.starts_with(b"ICC_PROFILE\0") => {
                self.icc_chunks += 1;
                if self.icc_chunks > ICC_CHUNKS_MAX {
                    return Err(Refused::Metadata(format!(
                        "an ICC profile in more than {ICC_CHUNKS_MAX} chunks"
                    )));
                }
                true
            }
            _ => false,
        };
        if !rendering {
            if self.other + len > KEPT_OTHER_MAX {
                return source.skip(len);
            }
            self.other += len;
        }
        head.kept.push((marker, source.bytes(len)?));
        Ok(())
    }
}

fn read_quant(head: &mut Head, mut data: &[u8]) -> Result<(), Refused> {
    while let Some((&pq_tq, rest)) = data.split_first() {
        let (precision, id) = (pq_tq >> 4, usize::from(pq_tq & 15));
        let size = if precision == 0 { 64 } else { 128 };
        if precision > 1 || id > 3 || rest.len() < size {
            return Err(damaged("a bad DQT"));
        }
        let mut zigzag = [0u16; 64];
        for (k, value) in zigzag.iter_mut().enumerate() {
            *value = if precision == 0 {
                u16::from(rest[k])
            } else {
                u16::from_be_bytes([rest[2 * k], rest[2 * k + 1]])
            };
        }
        if zigzag.contains(&0) {
            return Err(damaged("a zero quantiser"));
        }
        head.quant[id] = Some(Quant { precision, zigzag });
        data = &rest[size..];
    }
    Ok(())
}

fn read_huffman(head: &mut Head, mut data: &[u8]) -> Result<(), Refused> {
    while data.len() >= 17 {
        let (class, id) = (data[0] >> 4, usize::from(data[0] & 15));
        let counts: [u8; 16] = data[1..17].try_into().expect("16");
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        let values = data
            .get(17..17 + total)
            .ok_or_else(|| damaged("a short DHT"))?;
        if class > 1 || id > 3 {
            return Err(damaged("a bad DHT"));
        }
        let table = decode::Huffman::new(&counts, values).map_err(damaged)?;
        if class == 0 {
            head.dc[id] = Some(table);
        } else {
            head.ac[id] = Some(table);
        }
        data = &data[17 + total..];
    }
    if data.is_empty() {
        Ok(())
    } else {
        Err(damaged("a short DHT"))
    }
}

fn read_frame(head: &mut Head, marker: u8, data: &[u8]) -> Result<(), Refused> {
    if head.sof != 0 {
        return Err(damaged("a second frame"));
    }
    let short = || damaged("a short SOF");
    let fixed = data.get(..6).ok_or_else(short)?;
    if fixed[0] != 8 {
        return Err(Refused::Precision(fixed[0]));
    }
    let height = usize::from(u16::from_be_bytes([fixed[1], fixed[2]]));
    let width = usize::from(u16::from_be_bytes([fixed[3], fixed[4]]));
    let count = fixed[5];
    match count {
        1 | 3 => {}
        4 => return Err(Refused::Cmyk),
        n => return Err(Refused::Components(n)),
    }
    if width == 0 || height == 0 {
        return Err(damaged("no size in the frame (DNL)"));
    }
    let specs = data.get(6..6 + 3 * usize::from(count)).ok_or_else(short)?;
    for spec in specs.chunks(3) {
        let (h, v, tq) = (
            usize::from(spec[1] >> 4),
            usize::from(spec[1] & 15),
            usize::from(spec[2]),
        );
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 {
            return Err(damaged("a bad component"));
        }
        head.components.push(Component {
            id: spec[0],
            h,
            v,
            tq,
        });
    }
    if count > 1 && head.components.iter().map(|c| c.h * c.v).sum::<usize>() > 10 {
        return Err(damaged("more than 10 blocks an MCU"));
    }
    head.sof = marker;
    head.width = width;
    head.height = height;
    Ok(())
}

/// A scan's header (T.81 B.2.3): its components, in frame order, with the tables they read, its band and its
/// successive approximation. A progressive scan is checked as libjpeg checks it (G.1.1.1.1); a sequential one is
/// read as the whole band whatever it says, as libjpeg reads it.
#[allow(
    clippy::similar_names,
    reason = "the JPEG standard's names for the two quantities"
)]
fn read_scan(head: &Head, data: &[u8]) -> Result<decode::ScanSpec, Refused> {
    if head.sof == 0 {
        return Err(damaged("a scan before the frame"));
    }
    let short = || damaged("a short SOS");
    let count = usize::from(*data.first().ok_or_else(short)?);
    if count == 0 || count > 4 {
        return Err(damaged("a scan of no or more than four components"));
    }
    let specs = data.get(1..1 + 2 * count).ok_or_else(short)?;
    let band = data.get(1 + 2 * count..4 + 2 * count).ok_or_else(short)?;
    let progressive = head.sof == 0xC2;
    let (mut ss, mut se) = (usize::from(band[0]), usize::from(band[1]));
    let (mut ah, mut al) = (band[2] >> 4, band[2] & 15);
    if progressive {
        let bad = (ss == 0 && se != 0)
            || (ss > 0 && (count != 1 || se < ss || se > 63))
            || (ah != 0 && al + 1 != ah)
            || al > 13;
        if bad {
            return Err(damaged("a bad progressive scan"));
        }
    } else {
        (ss, se, ah, al) = (0, 63, 0, 0);
    }
    let (needs_dc, needs_ac) = (!progressive || (ss == 0 && ah == 0), !progressive || ss > 0);
    let missing = || damaged("a table the scan names is missing");
    let mut scan = decode::ScanSpec {
        comps: Vec::with_capacity(count),
        ss,
        se,
        ah,
        al,
        dc: Vec::with_capacity(count),
        ac: Vec::with_capacity(count),
        restart: head.restart,
    };
    for spec in specs.chunks(2) {
        let c = head
            .components
            .iter()
            .position(|c| c.id == spec[0])
            .ok_or_else(|| damaged("a scan component not in the frame"))?;
        if scan.comps.last().is_some_and(|&last| c <= last) {
            return Err(damaged("scan components out of the frame's order"));
        }
        let (td, ta) = (usize::from(spec[1] >> 4), usize::from(spec[1] & 15));
        if td > 3 || ta > 3 || head.quant[head.components[c].tq].is_none() {
            return Err(missing());
        }
        let table = |tables: &[Option<decode::Huffman>; 4], id: usize, needed: bool| {
            if needed {
                tables[id].clone().map(Some).ok_or_else(missing)
            } else {
                Ok(None)
            }
        };
        scan.dc.push(table(&head.dc, td, needs_dc)?);
        scan.ac.push(table(&head.ac, ta, needs_ac)?);
        scan.comps.push(c);
    }
    Ok(scan)
}

#[cfg(test)]
mod testenc;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_progressive;
