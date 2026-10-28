//! Test-only: a JPEG writer for any frame and scan script, sequential or progressive, so the decoder
//! can be checked on pictures no other encoder at hand makes: components in separate scans, successive
//! approximation on every band, restart intervals inside progressive scans. The progressive coding is libjpeg's
//! `jcphuff.c` (EOB runs, correction bits buffered behind them); `jpeg-decoder` checks it decodes to the same picture
//! as the same coefficients coded sequentially. Integration tests include this file by `#[path]`.
//!
//! The Huffman tables are fixed: ITU T.81 Annex K's luminance DC table, and an AC table giving every symbol 8 bits
//! (one 9), so any run, size and EOB run is codable without a counting pass.

#![allow(dead_code)]

/// A frame component: its id, sampling factors and quantisation table.
#[derive(Clone, Copy, Debug)]
pub struct Component {
    pub id: u8,
    pub h: usize,
    pub v: usize,
    pub tq: usize,
}

/// What `SOFn` says, the quantisation tables (zigzag order) and the restart interval.
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub components: Vec<Component>,
    pub quant: Vec<[u16; 64]>,
    pub progressive: bool,
    pub restart: usize,
}

/// One scan: the frame components it codes (indices), its spectral band and its successive approximation bits.
#[derive(Clone, Debug)]
pub struct Scan {
    pub comps: Vec<usize>,
    pub ss: usize,
    pub se: usize,
    pub ah: u32,
    pub al: u32,
}

fn scan(comps: &[usize], ss: usize, se: usize, ah: u32, al: u32) -> Scan {
    Scan {
        comps: comps.to_vec(),
        ss,
        se,
        ah,
        al,
    }
}

/// Every component in one sequential scan.
pub fn sequential(n: usize) -> Vec<Scan> {
    vec![scan(&(0..n).collect::<Vec<_>>(), 0, 63, 0, 0)]
}

/// Each component in a sequential scan of its own (non-interleaved), in frame order.
pub fn separate(n: usize) -> Vec<Scan> {
    (0..n).map(|c| scan(&[c], 0, 63, 0, 0)).collect()
}

/// libjpeg's `jpeg_simple_progression`: successive approximation on DC and AC.
pub fn simple_progression(n: usize) -> Vec<Scan> {
    let all: Vec<usize> = (0..n).collect();
    if n == 3 {
        return vec![
            scan(&all, 0, 0, 0, 1),
            scan(&[0], 1, 5, 0, 2),
            scan(&[2], 1, 63, 0, 1),
            scan(&[1], 1, 63, 0, 1),
            scan(&[0], 6, 63, 0, 2),
            scan(&[0], 1, 63, 2, 1),
            scan(&all, 0, 0, 1, 0),
            scan(&[2], 1, 63, 1, 0),
            scan(&[1], 1, 63, 1, 0),
            scan(&[0], 1, 63, 1, 0),
        ];
    }
    let mut out = vec![scan(&all, 0, 0, 0, 1)];
    for c in 0..n {
        out.push(scan(&[c], 1, 5, 0, 2));
        out.push(scan(&[c], 6, 63, 0, 2));
    }
    for c in 0..n {
        out.push(scan(&[c], 1, 63, 2, 1));
    }
    out.push(scan(&all, 0, 0, 1, 0));
    for c in 0..n {
        out.push(scan(&[c], 1, 63, 1, 0));
    }
    out
}

/// Spectral selection only (no successive approximation), DC of each component in its own scan.
pub fn spectral(n: usize) -> Vec<Scan> {
    let mut out: Vec<Scan> = (0..n).map(|c| scan(&[c], 0, 0, 0, 0)).collect();
    for c in 0..n {
        out.push(scan(&[c], 1, 2, 0, 0));
        out.push(scan(&[c], 3, 20, 0, 0));
        out.push(scan(&[c], 21, 63, 0, 0));
    }
    out
}

/// Successive approximation down three bits on every band, refinements interleaved with other components' scans.
pub fn deep_progression(n: usize) -> Vec<Scan> {
    let all: Vec<usize> = (0..n).collect();
    let mut out = vec![scan(&all, 0, 0, 0, 3)];
    for c in 0..n {
        out.push(scan(&[c], 1, 9, 0, 3));
        out.push(scan(&[c], 10, 63, 0, 3));
    }
    for (ah, al) in [(3, 2), (2, 1), (1, 0)] {
        out.push(scan(&all, 0, 0, ah, al));
        for c in (0..n).rev() {
            out.push(scan(&[c], 1, 63, ah, al));
        }
    }
    out
}

/// The blocks of each component, quantised, zigzag order, laid out over the frame's MCUs (padding blocks included).
#[derive(Clone, Debug)]
pub struct Blocks {
    /// Per component: blocks a row and the blocks.
    pub planes: Vec<(usize, Vec<[i16; 64]>)>,
}

impl Blocks {
    pub fn get(&self, c: usize, bx: usize, by: usize) -> [i16; 64] {
        let (row, blocks) = &self.planes[c];
        blocks[by * row + bx]
    }
}

/// `(h_max, v_max, mcus_x, mcus_y)` of a frame.
pub type Mcus = (usize, usize, usize, usize);

/// The geometry: the frame's [`Mcus`] and per component `(width, height)` in samples.
pub fn geometry(frame: &Frame) -> (Mcus, Vec<(usize, usize)>) {
    let h_max = frame.components.iter().map(|c| c.h).max().unwrap();
    let v_max = frame.components.iter().map(|c| c.v).max().unwrap();
    let sizes = frame
        .components
        .iter()
        .map(|c| {
            (
                (frame.width * c.h).div_ceil(h_max),
                (frame.height * c.v).div_ceil(v_max),
            )
        })
        .collect();
    (
        (
            h_max,
            v_max,
            frame.width.div_ceil(8 * h_max),
            frame.height.div_ceil(8 * v_max),
        ),
        sizes,
    )
}

/// The `n`-th of 64 positions in zigzag order, as row-major.
const UNZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Annex K's tables scaled to `quality` as libjpeg does: luminance for table 0, chrominance for 1.
pub fn quant(quality: u32) -> Vec<[u16; 64]> {
    const LUMA: [u16; 64] = [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
        56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81,
        104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ];
    const CHROMA: [u16; 64] = [
        17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99,
        99, 47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
        99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    ];
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - 2 * quality
    };
    [LUMA, CHROMA]
        .iter()
        .map(|natural| {
            let mut zigzag = [0u16; 64];
            for k in 0..64 {
                let v = (u32::from(natural[UNZIGZAG[k]]) * scale + 50) / 100;
                zigzag[k] = v.clamp(1, 255) as u16;
            }
            zigzag
        })
        .collect()
}

/// The blocks of `frame` from `sample(component, x, y)` at the component's resolution (edges replicated into the
/// padding), by a float FDCT quantised with the frame's tables.
#[allow(
    clippy::many_single_char_names,
    reason = "the formula's own names (x, y, w, h; r, g, b; the DCT's indices)"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a quantised coefficient of 8-bit samples is within ±2^11; the float-to-int `as` saturates"
)]
pub fn blocks(frame: &Frame, sample: impl Fn(usize, usize, usize) -> u8) -> Blocks {
    let ((_, _, mcus_x, mcus_y), sizes) = geometry(frame);
    let mut cos = [[0f64; 8]; 8];
    for (x, row) in cos.iter_mut().enumerate() {
        for (u, value) in row.iter_mut().enumerate() {
            let c = if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            let (x, u) = (u32::try_from(x).unwrap(), u32::try_from(u).unwrap());
            *value =
                c / 2.0 * (f64::from(2 * x + 1) * f64::from(u) * std::f64::consts::PI / 16.0).cos();
        }
    }
    let planes = frame
        .components
        .iter()
        .enumerate()
        .map(|(ci, c)| {
            let (w, h) = sizes[ci];
            let (bw, bh) = if frame.components.len() == 1 {
                (w.div_ceil(8), h.div_ceil(8))
            } else {
                (mcus_x * c.h, mcus_y * c.v)
            };
            let q = &frame.quant[c.tq];
            let mut out = Vec::with_capacity(bw * bh);
            for by in 0..bh {
                for bx in 0..bw {
                    let mut s = [0f64; 64];
                    for y in 0..8 {
                        for x in 0..8 {
                            let (sx, sy) = ((bx * 8 + x).min(w - 1), (by * 8 + y).min(h - 1));
                            s[y * 8 + x] = f64::from(sample(ci, sx, sy)) - 128.0;
                        }
                    }
                    let mut block = [0i16; 64];
                    for (k, value) in block.iter_mut().enumerate() {
                        let (v, u) = (UNZIGZAG[k] / 8, UNZIGZAG[k] % 8);
                        let mut sum = 0.0;
                        for y in 0..8 {
                            for x in 0..8 {
                                sum += cos[x][u] * cos[y][v] * s[y * 8 + x];
                            }
                        }
                        *value = (sum / f64::from(q[k])).round() as i16;
                    }
                    out.push(block);
                }
            }
            (bw, out)
        })
        .collect();
    Blocks { planes }
}

/// Bits MSB first, `0xFF` stuffed, padded with ones.
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

    fn pad(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1 << pad) - 1, pad);
        }
    }
}

const DC_COUNTS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const AC_COUNTS: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 255, 1, 0, 0, 0, 0, 0, 0, 0];

/// `(code, length)` of each symbol of the canonical table with `counts`, for symbols `0..` in order.
fn codes(counts: &[u8; 16]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut code = 0u32;
    for (length, &count) in (1u32..).zip(counts.iter()) {
        for _ in 0..count {
            out.push((code, length));
            code += 1;
        }
        code <<= 1;
    }
    out
}

/// The size category of `v` and its bits (T.81 F.1.2.1).
fn category(v: i32) -> (u32, u32) {
    let size = 32 - v.unsigned_abs().leading_zeros();
    let bits = if v < 0 {
        (v - 1).cast_unsigned()
    } else {
        v.cast_unsigned()
    };
    (size, bits & ((1u32 << size) - 1))
}

struct Writer {
    bits: Bits,
    dc: Vec<(u32, u32)>,
    ac: Vec<(u32, u32)>,
    eobrun: u32,
    /// Correction bits owed behind the pending EOB run (libjpeg's `BE`).
    owed: Vec<u8>,
}

impl Writer {
    fn dc(&mut self, symbol: u32) {
        let (code, length) = self.dc[symbol as usize];
        self.bits.put(code, length);
    }

    fn ac(&mut self, symbol: u32) {
        let (code, length) = self.ac[symbol as usize];
        self.bits.put(code, length);
    }

    fn eobrun(&mut self) {
        if self.eobrun == 0 {
            return;
        }
        let nbits = self.eobrun.ilog2();
        self.ac(nbits << 4);
        self.bits.put(self.eobrun & ((1 << nbits) - 1), nbits);
        self.eobrun = 0;
        for bit in std::mem::take(&mut self.owed) {
            self.bits.put(u32::from(bit), 1);
        }
    }

    fn sequential(&mut self, block: &[i16; 64], last: &mut i32) {
        let dc = i32::from(block[0]);
        let (size, bits) = category(dc - *last);
        *last = dc;
        self.dc(size);
        self.bits.put(bits, size);
        let mut run = 0;
        for &value in &block[1..] {
            if value == 0 {
                run += 1;
                continue;
            }
            while run > 15 {
                self.ac(0xF0);
                run -= 16;
            }
            let (size, bits) = category(i32::from(value));
            self.ac(run << 4 | size);
            self.bits.put(bits, size);
            run = 0;
        }
        if run > 0 {
            self.ac(0x00);
        }
    }

    fn dc_first(&mut self, block: &[i16; 64], al: u32, last: &mut i32) {
        let dc = i32::from(block[0]) >> al;
        let (size, bits) = category(dc - *last);
        *last = dc;
        self.dc(size);
        self.bits.put(bits, size);
    }

    fn dc_refine(&mut self, block: &[i16; 64], al: u32) {
        self.bits
            .put((i32::from(block[0]) >> al).cast_unsigned() & 1, 1);
    }

    fn ac_first(&mut self, block: &[i16; 64], ss: usize, se: usize, al: u32) {
        let mut run = 0;
        for &value in &block[ss..=se] {
            // Magnitude shifted, then its bits as T.81 codes them (one's complement when negative).
            let t = i32::from(value).abs() >> al;
            let t2 = if value < 0 { !t } else { t };
            if t == 0 {
                run += 1;
                continue;
            }
            self.eobrun();
            while run > 15 {
                self.ac(0xF0);
                run -= 16;
            }
            let size = 32 - t.leading_zeros();
            self.ac(run << 4 | size);
            self.bits.put(t2.cast_unsigned() & ((1 << size) - 1), size);
            run = 0;
        }
        if run > 0 {
            self.eobrun += 1;
            if self.eobrun == 0x7FFF {
                self.eobrun();
            }
        }
    }

    #[allow(
        clippy::iter_with_drain,
        reason = "`corrections` is refilled after each drain"
    )]
    fn ac_refine(&mut self, block: &[i16; 64], ss: usize, se: usize, al: u32) {
        let mut abs = [0i32; 64];
        let mut eob = 0;
        for k in ss..=se {
            abs[k] = i32::from(block[k]).abs() >> al;
            if abs[k] == 1 {
                eob = k;
            }
        }
        let mut run = 0;
        let mut corrections: Vec<u8> = Vec::new();
        for k in ss..=se {
            let t = abs[k];
            if t == 0 {
                run += 1;
                continue;
            }
            while run > 15 && k <= eob {
                self.eobrun();
                self.ac(0xF0);
                run -= 16;
                for bit in corrections.drain(..) {
                    self.bits.put(u32::from(bit), 1);
                }
            }
            if t > 1 {
                corrections.push(u8::from(t & 1 != 0));
                continue;
            }
            self.eobrun();
            self.ac(run << 4 | 1);
            self.bits.put(u32::from(block[k] >= 0), 1);
            for bit in corrections.drain(..) {
                self.bits.put(u32::from(bit), 1);
            }
            run = 0;
        }
        if run > 0 || !corrections.is_empty() {
            self.eobrun += 1;
            self.owed.extend(corrections);
            if self.eobrun == 0x7FFF || self.owed.len() > 1000 - 64 + 1 {
                self.eobrun();
            }
        }
    }
}

fn segment(out: &mut Vec<u8>, marker: u8, data: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&(u16::try_from(data.len()).unwrap() + 2).to_be_bytes());
    out.extend_from_slice(data);
}

/// The JPEG of `frame` coded by `script`, the blocks from `block(component, bx, by)` (quantised, zigzag order;
/// called again for every scan, so a large picture needs no store). `extra` segments go right after `SOI`.
pub fn write(
    frame: &Frame,
    script: &[Scan],
    extra: &[(u8, Vec<u8>)],
    block: &dyn Fn(usize, usize, usize) -> [i16; 64],
) -> Vec<u8> {
    let ((_, _, mcus_x, mcus_y), sizes) = geometry(frame);
    let mut out = vec![0xFF, 0xD8];
    for (marker, data) in extra {
        segment(&mut out, *marker, data);
    }
    let wide = frame.quant.iter().any(|q| q.iter().any(|&v| v > 255));
    for (id, q) in frame.quant.iter().enumerate() {
        let mut data = vec![u8::from(wide) << 4 | u8::try_from(id).unwrap()];
        for &v in q {
            if wide {
                data.extend_from_slice(&v.to_be_bytes());
            } else {
                data.push(u8::try_from(v).unwrap());
            }
        }
        segment(&mut out, 0xDB, &data);
    }
    let mut sof = vec![8];
    sof.extend_from_slice(&u16::try_from(frame.height).unwrap().to_be_bytes());
    sof.extend_from_slice(&u16::try_from(frame.width).unwrap().to_be_bytes());
    sof.push(u8::try_from(frame.components.len()).unwrap());
    for c in &frame.components {
        sof.extend_from_slice(&[
            c.id,
            u8::try_from(c.h << 4 | c.v).unwrap(),
            u8::try_from(c.tq).unwrap(),
        ]);
    }
    let marker = match (frame.progressive, wide) {
        (true, _) => 0xC2,
        (false, false) => 0xC0,
        (false, true) => 0xC1,
    };
    segment(&mut out, marker, &sof);
    if frame.restart > 0 {
        segment(
            &mut out,
            0xDD,
            &u16::try_from(frame.restart).unwrap().to_be_bytes(),
        );
    }
    let mut dht = vec![0x00];
    dht.extend_from_slice(&DC_COUNTS);
    dht.extend(0..12u8);
    dht.push(0x10);
    dht.extend_from_slice(&AC_COUNTS);
    dht.extend(0..=255u8);
    segment(&mut out, 0xC4, &dht);

    let mut writer = Writer {
        bits: Bits { out, acc: 0, n: 0 },
        dc: codes(&DC_COUNTS),
        ac: codes(&AC_COUNTS),
        eobrun: 0,
        owed: Vec::new(),
    };
    for s in script {
        let mut sos = vec![u8::try_from(s.comps.len()).unwrap()];
        for &c in &s.comps {
            sos.extend_from_slice(&[frame.components[c].id, 0x00]);
        }
        sos.extend_from_slice(&[
            u8::try_from(s.ss).unwrap(),
            u8::try_from(s.se).unwrap(),
            u8::try_from(s.ah << 4 | s.al).unwrap(),
        ]);
        segment(&mut writer.bits.out, 0xDA, &sos);

        // The units of the scan in order: each a list of (component, bx, by).
        let mut units: Vec<Vec<(usize, usize, usize)>> = Vec::new();
        if s.comps.len() == 1 {
            let c = s.comps[0];
            let (w, h) = sizes[c];
            for by in 0..h.div_ceil(8) {
                for bx in 0..w.div_ceil(8) {
                    units.push(vec![(c, bx, by)]);
                }
            }
        } else {
            for my in 0..mcus_y {
                for mx in 0..mcus_x {
                    let mut unit = Vec::new();
                    for &c in &s.comps {
                        let comp = frame.components[c];
                        for v in 0..comp.v {
                            for h in 0..comp.h {
                                unit.push((c, mx * comp.h + h, my * comp.v + v));
                            }
                        }
                    }
                    units.push(unit);
                }
            }
        }
        let mut last = vec![0i32; frame.components.len()];
        let mut rst = 0u8;
        for (i, unit) in units.iter().enumerate() {
            if frame.restart > 0 && i > 0 && i % frame.restart == 0 {
                writer.eobrun();
                writer.bits.pad();
                writer.bits.out.extend_from_slice(&[0xFF, 0xD0 + rst]);
                rst = (rst + 1) & 7;
                last.fill(0);
            }
            for &(c, bx, by) in unit {
                if s.ss > s.se || s.se > 63 {
                    break; // a band no decoder takes: its header alone, for refusals
                }
                let b = block(c, bx, by);
                match (frame.progressive, s.ss, s.ah) {
                    (false, _, _) => writer.sequential(&b, &mut last[c]),
                    (true, 0, 0) => writer.dc_first(&b, s.al, &mut last[c]),
                    (true, 0, _) => writer.dc_refine(&b, s.al),
                    (true, _, 0) => writer.ac_first(&b, s.ss, s.se, s.al),
                    (true, _, _) => writer.ac_refine(&b, s.ss, s.se, s.al),
                }
            }
        }
        writer.eobrun();
        writer.bits.pad();
    }
    let mut out = writer.bits.out;
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// A frame of `width × height`: gray, or three components sampled `(h, v)` for luma and 1 × 1 for chroma.
pub fn frame(width: usize, height: usize, luma: Option<(usize, usize)>, quality: u32) -> Frame {
    let components = match luma {
        None => vec![Component {
            id: 1,
            h: 1,
            v: 1,
            tq: 0,
        }],
        Some((h, v)) => vec![
            Component { id: 1, h, v, tq: 0 },
            Component {
                id: 2,
                h: 1,
                v: 1,
                tq: 1,
            },
            Component {
                id: 3,
                h: 1,
                v: 1,
                tq: 1,
            },
        ],
    };
    Frame {
        width,
        height,
        components,
        quant: quant(quality),
        progressive: false,
        restart: 0,
    }
}

/// A textured picture: gradients, stripes and some hash noise, so many AC coefficients are nonzero and several
/// above 1 (successive approximation has bits to refine).
pub fn texture(c: usize, x: usize, y: usize) -> u8 {
    let hash = (x.wrapping_mul(0x9E37) ^ y.wrapping_mul(0x85EB) ^ c.wrapping_mul(0xC2B2))
        .wrapping_mul(0x27D4_EB2F)
        >> 13;
    let stripes = if (x / 3 + y / 5 + c) % 4 < 2 { 40 } else { 0 };
    u8::try_from((x * 2 + y + c * 50) % 160 + stripes + hash % 24).expect("at most 159 + 40 + 23")
}
