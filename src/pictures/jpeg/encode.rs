//! The encoding half: the downscaled components, a row at a time, back into a sequential JPEG with the source's
//! component ids, sampling factors and quantisation tables, an MCU row at a time.

use std::collections::VecDeque;
use std::io::{self, Write};

use super::{Head, Layout, Refused, UNZIGZAG};

use crate::num::ToF64;

/// ITU T.81 Annex K.3: the typical Huffman tables, luminance then chrominance, DC then AC.
const DC_LUMA: ([u8; 16], &[u8]) = (
    [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
);
const DC_CHROMA: ([u8; 16], &[u8]) = (
    [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
);
const AC_LUMA: ([u8; 16], &[u8]) = (
    [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D],
    &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52,
        0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
        0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3,
        0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8,
        0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ],
);
const AC_CHROMA: ([u8; 16], &[u8]) = (
    [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33,
        0x52, 0xF0, 0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18,
        0x19, 0x1A, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4,
        0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA,
        0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
        0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ],
);

/// Code and length of every symbol of a table.
struct Codes([(u16, u8); 256]);

impl Codes {
    fn new((counts, values): &([u8; 16], &[u8])) -> Self {
        let mut codes = [(0, 0); 256];
        let (mut code, mut k) = (0u16, 0);
        for (length, &count) in (1u8..).zip(counts.iter()) {
            for _ in 0..count {
                codes[usize::from(values[k])] = (code, length);
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        Self(codes)
    }
}

/// Entropy-coded bits, `0xFF` stuffed, buffered before the writer.
struct BitWriter<W> {
    out: W,
    buf: Vec<u8>,
    acc: u64,
    n: u32,
}

impl<W: Write> BitWriter<W> {
    fn put(&mut self, bits: u32, count: u32) -> io::Result<()> {
        self.acc = self.acc << count | u64::from(bits & ((1 << count) - 1));
        self.n += count;
        while self.n >= 8 {
            self.n -= 8;
            let byte = (self.acc >> self.n).to_le_bytes()[0];
            self.buf.push(byte);
            if byte == 0xFF {
                self.buf.push(0);
            }
        }
        if self.buf.len() >= 64 * 1024 {
            self.out.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }

    /// Pads the last byte with ones and writes everything out.
    fn flush(&mut self) -> io::Result<()> {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1 << pad) - 1, pad)?;
        }
        self.out.write_all(&self.buf)?;
        self.buf.clear();
        Ok(())
    }
}

/// One output component: its rows waiting to be coded, padded to its MCU row width, and its coding state.
struct Plane {
    /// Rows to keep and rows still to come: the output's size of the component.
    width: usize,
    height: usize,
    padded_width: usize,
    /// Rows one MCU row takes.
    strip: usize,
    rows: VecDeque<Vec<u8>>,
    /// The last row taken, which pads the bottom.
    last: Vec<u8>,
    received: usize,
    /// Quantisers in natural order.
    quant: [f32; 64],
    table: usize,
    predictor: i32,
}

/// The `8 × 8` forward DCT, `A[u][x] = C(u)/2 · cos((2x+1)uπ/16)`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the matrix is f32 by design; its values lie in [-1, 1]"
)]
fn fdct_matrix() -> [f32; 64] {
    let mut a = [0f32; 64];
    for u in 0..8 {
        for x in 0..8 {
            let c = if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            a[u * 8 + x] = (c / 2.0
                * ((2 * x + 1).to_f64() * u.to_f64() * std::f64::consts::PI / 16.0).cos())
                as f32;
        }
    }
    a
}

/// Writes a downscaled JPEG: [`Encoder::start`] writes everything up to the scan, [`Encoder::push_row`] takes the
/// components' rows and codes each MCU row once every component has its rows, [`Encoder::finish`] ends it.
pub(super) struct Encoder<W> {
    bits: BitWriter<W>,
    layout: Layout,
    planes: Vec<Plane>,
    coded_rows: usize,
    fdct: [f32; 64],
    dc: [Codes; 2],
    ac: [Codes; 2],
}

impl<W: Write> Encoder<W> {
    /// Writes `SOI`, the source's kept segments (JFIF's density divided by `factor`), its quantisation tables, the
    /// frame of `width × height` and the scan header.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "every value comes from the parsed head: dimensions are u16 there and only shrink, at most 4 \
                  components and tables, h and v are nibbles, an 8-bit table's values are bytes, and a kept segment's \
                  length was a u16 in the source"
    )]
    pub(super) fn start(
        mut out: W,
        head: &Head,
        width: usize,
        height: usize,
        factor: u32,
    ) -> Result<Self, Refused> {
        let segment = |out: &mut W, marker: u8, data: &[u8]| -> io::Result<()> {
            out.write_all(&[0xFF, marker])?;
            out.write_all(&(data.len() as u16 + 2).to_be_bytes())?;
            out.write_all(data)
        };
        out.write_all(&[0xFF, 0xD8])?;
        for (marker, data) in &head.kept {
            if *marker == 0xE0 && data.len() >= 12 && data.starts_with(b"JFIF\0") && data[7] != 0 {
                let mut jfif = data.clone();
                for at in [8, 10] {
                    let density = u16::from_be_bytes([jfif[at], jfif[at + 1]]);
                    let reduced = (u32::from(density) / factor).max(1) as u16;
                    jfif[at..at + 2].copy_from_slice(&reduced.to_be_bytes());
                }
                segment(&mut out, *marker, &jfif)?;
            } else {
                segment(&mut out, *marker, data)?;
            }
        }
        let mut used = [false; 4];
        for c in &head.components {
            used[c.tq] = true;
        }
        for (id, quant) in head.quant.iter().enumerate() {
            let Some(quant) = quant.filter(|_| used[id]) else {
                continue;
            };
            let mut data = vec![quant.precision << 4 | id as u8];
            for value in quant.zigzag {
                if quant.precision == 0 {
                    data.push(value as u8);
                } else {
                    data.extend_from_slice(&value.to_be_bytes());
                }
            }
            segment(&mut out, 0xDB, &data)?;
        }
        let mut frame = vec![8];
        frame.extend_from_slice(&(height as u16).to_be_bytes());
        frame.extend_from_slice(&(width as u16).to_be_bytes());
        frame.push(head.components.len() as u8);
        for c in &head.components {
            frame.extend_from_slice(&[c.id, (c.h << 4 | c.v) as u8, c.tq as u8]);
        }
        // Sequential whatever the source was: a progressive source is written as baseline, or as extended
        // sequential when a table it uses has 16-bit values (baseline allows only 8).
        let wide = head
            .components
            .iter()
            .any(|c| head.quant[c.tq].is_some_and(|q| q.precision == 1));
        let sof = if head.sof == 0xC2 {
            if wide { 0xC1 } else { 0xC0 }
        } else {
            head.sof
        };
        segment(&mut out, sof, &frame)?;
        let chroma = head.components.len() > 1;
        for (class_id, (counts, values)) in [
            (0x00, DC_LUMA),
            (0x10, AC_LUMA),
            (0x01, DC_CHROMA),
            (0x11, AC_CHROMA),
        ]
        .into_iter()
        .take(if chroma { 4 } else { 2 })
        {
            let mut data = vec![class_id];
            data.extend_from_slice(&counts);
            data.extend_from_slice(values);
            segment(&mut out, 0xC4, &data)?;
        }
        let mut scan = vec![head.components.len() as u8];
        for (i, c) in head.components.iter().enumerate() {
            let table = u8::from(i > 0);
            scan.extend_from_slice(&[c.id, table << 4 | table]);
        }
        scan.extend_from_slice(&[0, 63, 0]);
        segment(&mut out, 0xDA, &scan)?;

        let layout = Layout::new(width, height, &head.components);
        let planes = layout
            .parts
            .iter()
            .zip(&head.components)
            .enumerate()
            .map(|(i, (part, c))| {
                let table = head.quant[c.tq].expect("checked by read_scan");
                let mut quant = [0f32; 64];
                for (k, &value) in table.zigzag.iter().enumerate() {
                    quant[UNZIGZAG[k]] = f32::from(value);
                }
                Plane {
                    width: part.width,
                    height: part.height,
                    padded_width: part.blocks_x * 8,
                    strip: part.v * 8,
                    rows: VecDeque::new(),
                    last: vec![128; part.blocks_x * 8],
                    received: 0,
                    quant,
                    table: usize::from(i > 0),
                    predictor: 0,
                }
            })
            .collect();
        Ok(Self {
            bits: BitWriter {
                out,
                buf: Vec::with_capacity(64 * 1024 + 16),
                acc: 0,
                n: 0,
            },
            layout,
            planes,
            coded_rows: 0,
            fdct: fdct_matrix(),
            dc: [Codes::new(&DC_LUMA), Codes::new(&DC_CHROMA)],
            ac: [Codes::new(&AC_LUMA), Codes::new(&AC_CHROMA)],
        })
    }

    /// Takes the next row of `component`; a row past its height is dropped, a short one widened by its last sample.
    pub(super) fn push_row(&mut self, component: usize, row: &[u8]) -> Result<(), Refused> {
        let plane = &mut self.planes[component];
        if plane.received == plane.height || row.is_empty() {
            return Ok(());
        }
        let mut padded = vec![0u8; plane.padded_width];
        let kept = row.len().min(plane.width);
        padded[..kept].copy_from_slice(&row[..kept]);
        let last = padded[kept - 1];
        padded[kept..].fill(last);
        plane.last.copy_from_slice(&padded);
        plane.rows.push_back(padded);
        plane.received += 1;
        if plane.received == plane.height {
            Self::pad_rows(plane, self.layout.mcus_y);
        }
        self.code_ready()
    }

    /// Repeats a finished component's last row down to the bottom of its last MCU row.
    fn pad_rows(plane: &mut Plane, mcus_y: usize) {
        let total = mcus_y * plane.strip;
        while plane.received < total {
            plane.rows.push_back(plane.last.clone());
            plane.received += 1;
        }
        plane.height = plane.received;
    }

    /// Codes every MCU row whose rows every component has.
    fn code_ready(&mut self) -> Result<(), Refused> {
        while self.coded_rows < self.layout.mcus_y
            && self.planes.iter().all(|p| p.rows.len() >= p.strip)
        {
            self.code_mcu_row()?;
            for plane in &mut self.planes {
                plane.rows.drain(..plane.strip);
            }
            self.coded_rows += 1;
        }
        Ok(())
    }

    fn code_mcu_row(&mut self) -> Result<(), Refused> {
        let mut block = [0f32; 64];
        for mx in 0..self.layout.mcus_x {
            for c in 0..self.planes.len() {
                let part = self.layout.parts[c];
                for by in 0..part.v {
                    for bx in 0..part.h {
                        let x0 = (mx * part.h + bx) * 8;
                        let plane = &self.planes[c];
                        for y in 0..8 {
                            let row = &plane.rows[by * 8 + y][x0..x0 + 8];
                            for x in 0..8 {
                                block[y * 8 + x] = f32::from(row[x]) - 128.0;
                            }
                        }
                        self.code_block(c, &block)?;
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(
        clippy::float_cmp,
        reason = "the values are exact: whole counts and durations set by the test"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a coefficient is a sample sum over a quantiser of at least 1, far inside i32; the float-to-int `as` \
                  saturates"
    )]
    fn code_block(&mut self, c: usize, block: &[f32; 64]) -> Result<(), Refused> {
        let mut coefficients = [0i32; 64];
        let plane = &self.planes[c];
        let flat = block.iter().all(|&s| s == block[0]);
        if flat {
            // Only the DC term: 8 × the mean.
            coefficients[0] = (block[0] * 8.0 / plane.quant[0]).round() as i32;
        } else {
            let a = &self.fdct;
            let mut rows = [0f32; 64]; // rows[y][u]
            for y in 0..8 {
                for u in 0..8 {
                    let mut s = 0.0;
                    for x in 0..8 {
                        s += a[u * 8 + x] * block[y * 8 + x];
                    }
                    rows[y * 8 + u] = s;
                }
            }
            for v in 0..8 {
                for u in 0..8 {
                    let mut s = 0.0;
                    for y in 0..8 {
                        s += a[v * 8 + y] * rows[y * 8 + u];
                    }
                    coefficients[v * 8 + u] = (s / plane.quant[v * 8 + u]).round() as i32;
                }
            }
        }
        let table = plane.table;
        let dc = coefficients[0].clamp(-1024, 1023);
        let diff = dc - plane.predictor;
        self.planes[c].predictor = dc;
        let size = magnitude(diff);
        let (code, length) = self.dc[table].0[size as usize];
        self.bits.put(u32::from(code), u32::from(length))?;
        self.bits.put(signed_bits(diff), size)?;
        let mut run = 0;
        for &z in &UNZIGZAG[1..] {
            let value = coefficients[z].clamp(-1023, 1023);
            if value == 0 {
                run += 1;
                continue;
            }
            while run >= 16 {
                let (code, length) = self.ac[table].0[0xF0];
                self.bits.put(u32::from(code), u32::from(length))?;
                run -= 16;
            }
            let size = magnitude(value);
            let (code, length) = self.ac[table].0[(run << 4 | size) as usize];
            self.bits.put(u32::from(code), u32::from(length))?;
            self.bits.put(signed_bits(value), size)?;
            run = 0;
        }
        if run > 0 {
            let (code, length) = self.ac[table].0[0x00];
            self.bits.put(u32::from(code), u32::from(length))?;
        }
        Ok(())
    }

    /// Pads a component the decoder gave fewer rows than its size, codes what is left, writes `EOI`.
    pub(super) fn finish(mut self) -> Result<W, Refused> {
        let mcus_y = self.layout.mcus_y;
        for plane in &mut self.planes {
            if plane.received < plane.height {
                Self::pad_rows(plane, mcus_y);
            }
        }
        self.code_ready()?;
        if self.coded_rows != mcus_y {
            return Err(Refused::Output(io::Error::other("rows missing at the end")));
        }
        self.bits.flush()?;
        self.bits.out.write_all(&[0xFF, 0xD9])?;
        self.bits.out.flush()?;
        Ok(self.bits.out)
    }
}

/// The bits `value` takes (T.81 F.1.2.1: its size category).
fn magnitude(value: i32) -> u32 {
    32 - value.unsigned_abs().leading_zeros()
}

/// `value`'s low bits as coded: itself when positive, `value - 1` when negative.
fn signed_bits(value: i32) -> u32 {
    if value < 0 {
        (value - 1).cast_unsigned()
    } else {
        value.cast_unsigned()
    }
}
