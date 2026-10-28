//! The decoding half: every scan of the picture read an MCU row at a time, each from its own cursor (a
//! progressive JPEG's scans, or a sequential one's components in separate scans, each a stream over the same blocks),
//! the row's blocks then dequantised and turned back into samples by an IDCT of 8/s points a side from their lowest
//! coefficients (DCT scaling by 1/s).

use std::io::Read;

use super::{Head, Layout, Refused, UNZIGZAG, damaged};

use crate::num::ToF64;

/// The input, read through a buffer of its own; a read error or the end of the input is a damaged picture.
pub(super) struct Source<R> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    len: usize,
}

impl<R: Read> Source<R> {
    pub(super) fn new(inner: R) -> Self {
        Self::with_capacity(inner, 64 * 1024)
    }

    /// A source reading `capacity` bytes at a time: a scan's cursor takes a small one.
    pub(super) fn with_capacity(inner: R, capacity: usize) -> Self {
        Self {
            inner,
            buf: vec![0; capacity].into_boxed_slice(),
            pos: 0,
            len: 0,
        }
    }

    pub(super) fn byte(&mut self) -> Result<u8, Refused> {
        self.try_byte()?.ok_or_else(|| damaged("cut short"))
    }

    /// The next byte, `None` at the end of the input.
    pub(super) fn try_byte(&mut self) -> Result<Option<u8>, Refused> {
        if self.pos == self.len {
            loop {
                match self.inner.read(&mut self.buf) {
                    Ok(0) => return Ok(None),
                    Ok(n) => {
                        self.pos = 0;
                        self.len = n;
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(damaged(format!("unreadable: {e}"))),
                }
            }
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(Some(b))
    }

    pub(super) fn u16(&mut self) -> Result<u16, Refused> {
        Ok(u16::from_be_bytes([self.byte()?, self.byte()?]))
    }

    /// The next `n` bytes, in a vector of exactly `n` (a segment's payload: at most 65 533).
    pub(super) fn bytes(&mut self, n: usize) -> Result<Vec<u8>, Refused> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.byte()?);
        }
        Ok(out)
    }

    /// Reads past `n` bytes without keeping them.
    pub(super) fn skip(&mut self, n: usize) -> Result<(), Refused> {
        for _ in 0..n {
            self.byte()?;
        }
        Ok(())
    }

    /// The next `n` bytes, at most 16 of them, left unread: a segment's signature.
    pub(super) fn peek(&mut self, n: usize) -> Result<Vec<u8>, Refused> {
        debug_assert!(n <= 16);
        if self.len - self.pos < n {
            // Move what is left to the front and fill up behind it; a read of 0 is the end of the input.
            self.buf.copy_within(self.pos..self.len, 0);
            self.len -= self.pos;
            self.pos = 0;
            while self.len < n {
                match self.inner.read(&mut self.buf[self.len..]) {
                    Ok(0) => break,
                    Ok(read) => self.len += read,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(damaged(format!("unreadable: {e}"))),
                }
            }
        }
        Ok(self.buf[self.pos..self.len.min(self.pos + n)].to_vec())
    }
}

/// Codes up to this many bits are looked up in one step.
const LOOKUP: usize = 9;

/// A Huffman table for decoding: a lookup for short codes, the canonical bounds for long ones.
#[derive(Clone)]
pub(super) struct Huffman {
    /// `(length << 8) | value` for every `LOOKUP`-bit prefix that starts a short code; 0 where none does.
    lookup: Box<[u16; 1 << LOOKUP]>,
    /// The largest code of each length, -1 when there is none.
    max_code: [i32; 17],
    /// What to add to a code of each length to index `values`.
    offset: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    /// The table `DHT` defines by its counts per length and its values.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "code stays at most 2^16 (checked against `1 << length` below) and k at most 16 × 255 (the counts are \
                  u8), so both fit an i32; a lookup entry is a length of at most LOOKUP (9) over a u8 value, < 2^16"
    )]
    pub(super) fn new(counts: &[u8; 16], values: &[u8]) -> Result<Self, String> {
        let mut table = Self {
            lookup: Box::new([0; 1 << LOOKUP]),
            max_code: [-1; 17],
            offset: [0; 17],
            values: values.to_vec(),
        };
        let (mut code, mut k) = (0usize, 0usize);
        for length in 1..=16 {
            let n = usize::from(counts[length - 1]);
            table.offset[length] = k as i32 - code as i32;
            if code + n > 1 << length {
                return Err("a Huffman table with too many codes".into());
            }
            for _ in 0..n {
                if length <= LOOKUP {
                    let first = code << (LOOKUP - length);
                    let entry = (length << 8 | usize::from(values[k])) as u16;
                    table.lookup[first..first + (1 << (LOOKUP - length))].fill(entry);
                }
                code += 1;
                k += 1;
            }
            if n > 0 {
                table.max_code[length] = code as i32 - 1;
            }
            code <<= 1;
        }
        Ok(table)
    }
}

/// The entropy-coded bits: `0xFF 0x00` unstuffed; a marker ends them, and past it the bits read as zeros, as libjpeg
/// pads a scan cut short. The zeros are bounded by the bytes read (`read`, `padded`, see [`Bits::restart`]): every
/// block of a sequential scan or a first DC scan takes at least a bit, so the work a scan costs stays proportional
/// to its input however large its frame claims to be. (The other progressive scans can skip blocks in EOB runs, but
/// a picture must have its first DC scans, so its blocks are paid for there.)
struct Bits<S> {
    source: S,
    acc: u64,
    n: usize,
    marker: Option<u8>,
    read: usize,
    padded: usize,
}

/// Zero bytes a scan may be padded with past what it read: a whole scan's look-ahead, with room.
const PADDING: usize = 64;

/// Zero bytes each restart interval adds to [`PADDING`]: the look-ahead behind its marker is at most 8. Each costs the
/// marker's two bytes, so a scan of empty intervals still decodes at most about 30 blocks a byte of input.
const RESTART_PADDING: usize = 16;

impl<R: Read, S: std::ops::DerefMut<Target = Source<R>>> Bits<S> {
    fn fill(&mut self) -> Result<(), Refused> {
        while self.n <= 56 {
            let byte = if self.marker.is_some() {
                self.padded += 1;
                if self.padded > self.read + PADDING {
                    return Err(damaged("the scan ends before its last MCU"));
                }
                0
            } else {
                self.read += 1;
                let b = self.source.byte()?;
                if b == 0xFF {
                    let mut next = self.source.byte()?;
                    while next == 0xFF {
                        next = self.source.byte()?;
                    }
                    if next != 0 {
                        self.marker = Some(next);
                        0
                    } else {
                        0xFF
                    }
                } else {
                    b
                }
            };
            self.acc = self.acc << 8 | u64::from(byte);
            self.n += 8;
        }
        Ok(())
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the value is masked to `count` bits, at most 16 (a Huffman size or run is a 4-bit field)"
    )]
    fn bits(&mut self, count: usize) -> Result<u32, Refused> {
        if count == 0 {
            return Ok(0);
        }
        if self.n < count {
            self.fill()?;
        }
        self.n -= count;
        Ok(((self.acc >> self.n) & ((1 << count) - 1)) as u32)
    }

    /// `count` bits as a signed value (T.81 F.2.2.1 `EXTEND`).
    fn value(&mut self, count: usize) -> Result<i32, Refused> {
        let v = self.bits(count)?.cast_signed();
        Ok(if count > 0 && v < 1 << (count - 1) {
            v - (1 << count) + 1
        } else {
            v
        })
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a lookup entry's low byte is its value; a code is masked to `length` bits, at most 16"
    )]
    fn decode(&mut self, table: &Huffman) -> Result<u8, Refused> {
        if self.n < 16 {
            self.fill()?;
        }
        let entry = table.lookup[((self.acc >> (self.n - LOOKUP)) & ((1 << LOOKUP) - 1)) as usize];
        if entry != 0 {
            self.n -= usize::from(entry >> 8);
            return Ok(entry as u8);
        }
        for length in LOOKUP + 1..=16 {
            let code = ((self.acc >> (self.n - length)) & ((1 << length) - 1)) as i32;
            if code <= table.max_code[length] {
                self.n -= length;
                return usize::try_from(code + table.offset[length])
                    .ok()
                    .and_then(|at| table.values.get(at))
                    .copied()
                    .ok_or_else(|| damaged("a bad Huffman code"));
            }
        }
        Err(damaged("a bad Huffman code"))
    }

    /// One correction bit for a nonzero coefficient: when set and its bit not already there, the coefficient
    /// grows away from zero by it.
    fn correct(&mut self, coefficient: &mut i16, p1: i16, m1: i16) -> Result<(), Refused> {
        if self.bits(1)? == 1 && *coefficient & p1 == 0 {
            *coefficient = coefficient.wrapping_add(if *coefficient >= 0 { p1 } else { m1 });
        }
        Ok(())
    }

    /// Drops the bits left before a restart marker and reads it: it must be `RSTn` for `expected`.
    ///
    /// Each interval adds [`RESTART_PADDING`] to the zeros the scan may be padded with: a progressive scan's interval
    /// may hold a byte or two (an EOB run), less than the look-ahead pads behind its marker.
    fn restart(&mut self, expected: u8) -> Result<(), Refused> {
        self.acc = 0;
        self.n = 0;
        self.read += RESTART_PADDING;
        let marker = match self.marker.take() {
            Some(marker) => marker,
            None => loop {
                if self.source.byte()? != 0xFF {
                    continue;
                }
                let mut next = self.source.byte()?;
                while next == 0xFF {
                    next = self.source.byte()?;
                }
                if next != 0 {
                    break next;
                }
            },
        };
        if marker != 0xD0 + (expected & 7) {
            return Err(damaged(format!(
                "RST{expected} expected, {marker:#04X} found"
            )));
        }
        Ok(())
    }
}

/// The `n`-point IDCT of the lowest `n` coefficients of an 8-point DCT, row-major `[x][u]`: `C(u)/2 · cos((2x+1)uπ/2n)`.
/// The 8-point normalisation is kept, so the DC term gives the block's mean, as at full size.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the kernel is f32 by design; its values lie in [-1, 1]"
)]
pub(super) fn kernel(n: usize) -> Vec<f32> {
    let mut m = vec![0f32; n * n];
    for x in 0..n {
        for u in 0..n {
            let c = if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            m[x * n + u] = (c / 2.0
                * ((2 * x + 1).to_f64() * u.to_f64() * std::f64::consts::PI / (2 * n).to_f64())
                    .cos()) as f32;
        }
    }
    m
}

/// The samples of a block from its dequantised coefficients (row-major, only the top-left `n × n` read), written
/// `n × n` at `out` with rows `stride` apart.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "`level` clamps to 0..=255 before its cast to u8"
)]
pub(super) fn idct(
    coefficients: &[f32; 64],
    ac: bool,
    n: usize,
    m: &[f32],
    out: &mut [u8],
    stride: usize,
) {
    let level = |v: f32| (v + 128.5).clamp(0.0, 255.0) as u8;
    if !ac {
        let dc = level(coefficients[0] / 8.0);
        for y in 0..n {
            out[y * stride..y * stride + n].fill(dc);
        }
        return;
    }
    let mut rows = [0f32; 64];
    for v in 0..n {
        for x in 0..n {
            let mut s = 0.0;
            for u in 0..n {
                s += m[x * n + u] * coefficients[v * 8 + u];
            }
            rows[v * n + x] = s;
        }
    }
    for y in 0..n {
        for x in 0..n {
            let mut s = 0.0;
            for v in 0..n {
                s += m[y * n + v] * rows[v * n + x];
            }
            out[y * stride + x] = level(s);
        }
    }
}

/// One scan as its `SOS` says, with the Huffman tables and restart interval in force at it.
#[derive(Clone)]
pub(super) struct ScanSpec {
    /// The frame components it codes, by index, in frame order.
    pub(super) comps: Vec<usize>,
    pub(super) ss: usize,
    pub(super) se: usize,
    pub(super) ah: u8,
    pub(super) al: u8,
    /// Per scan component: its DC table, when the scan reads one.
    pub(super) dc: Vec<Option<Huffman>>,
    /// Per scan component: its AC table, when the scan reads one.
    pub(super) ac: Vec<Option<Huffman>>,
    pub(super) restart: usize,
}

/// What a block of a scan holds: a sequential scan the whole block, a progressive one a DC or AC band, first or
/// refined (T.81 G.1.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Sequential,
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

/// A scan being decoded: its bits, DC predictors, EOB run and restart count, kept from one MCU row to the next.
pub(super) struct Cursor<S> {
    spec: ScanSpec,
    kind: Kind,
    bits: Bits<S>,
    predictors: Vec<i32>,
    eobrun: u32,
    done: usize,
    next_restart: u8,
}

impl<R: Read, S: std::ops::DerefMut<Target = Source<R>>> Cursor<S> {
    /// A cursor at the start of `spec`'s entropy-coded data in `source`, in a progressive frame or not.
    pub(super) fn new(spec: ScanSpec, progressive: bool, source: S) -> Self {
        let kind = match (progressive, spec.ss, spec.ah) {
            (false, _, _) => Kind::Sequential,
            (true, 0, 0) => Kind::DcFirst,
            (true, 0, _) => Kind::DcRefine,
            (true, _, 0) => Kind::AcFirst,
            (true, _, _) => Kind::AcRefine,
        };
        let predictors = vec![0; spec.comps.len()];
        Self {
            spec,
            kind,
            bits: Bits {
                source,
                acc: 0,
                n: 0,
                marker: None,
                read: 0,
                padded: 0,
            },
            predictors,
            eobrun: 0,
            done: 0,
            next_restart: 0,
        }
    }

    /// Before each MCU (or block, alone in its scan): the restart marker when one is due.
    fn unit(&mut self) -> Result<(), Refused> {
        let restart = self.spec.restart;
        if restart > 0 && self.done > 0 && self.done.is_multiple_of(restart) {
            self.bits.restart(self.next_restart)?;
            self.next_restart = self.next_restart.wrapping_add(1);
            self.predictors.fill(0);
            self.eobrun = 0;
        }
        self.done += 1;
        Ok(())
    }

    /// Decodes this scan's part of MCU row `my` into `rows`: per frame component, its blocks of the row (`v` block
    /// rows of `blocks_x`, natural order coefficients).
    fn row(
        &mut self,
        my: usize,
        layout: &Layout,
        rows: &mut [Vec<[i16; 64]>],
    ) -> Result<(), Refused> {
        if self.spec.comps.len() == 1 {
            // Alone in its scan: the component's own blocks, row by row, no MCU padding.
            let c = self.spec.comps[0];
            let part = layout.parts[c];
            let (wide, high) = (part.width.div_ceil(8), part.height.div_ceil(8));
            for by in 0..part.v {
                if my * part.v + by >= high {
                    break;
                }
                for bx in 0..wide {
                    self.unit()?;
                    let block = &mut rows[c][by * part.blocks_x + bx];
                    self.block(0, block)?;
                }
            }
            return Ok(());
        }
        for mx in 0..layout.mcus_x {
            self.unit()?;
            for i in 0..self.spec.comps.len() {
                let part = layout.parts[self.spec.comps[i]];
                for by in 0..part.v {
                    for bx in 0..part.h {
                        let at = by * part.blocks_x + mx * part.h + bx;
                        let block = &mut rows[self.spec.comps[i]][at];
                        self.block(i, block)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Decodes one block of scan component `i` into `block`.
    fn block(&mut self, i: usize, block: &mut [i16; 64]) -> Result<(), Refused> {
        match self.kind {
            Kind::Sequential => {
                self.dc_first(i, block)?;
                self.ac_first(i, block)
            }
            Kind::DcFirst => self.dc_first(i, block),
            Kind::DcRefine => {
                if self.bits.bits(1)? == 1 {
                    block[0] |= 1 << self.spec.al;
                }
                Ok(())
            }
            Kind::AcFirst => self.ac_first(i, block),
            Kind::AcRefine => self.ac_refine(block),
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the cut to i16 is libjpeg's JCOEF, as the comment in the body says"
    )]
    fn dc_first(&mut self, i: usize, block: &mut [i16; 64]) -> Result<(), Refused> {
        let table = self.spec.dc[i].as_ref().expect("checked by read_scan");
        let size = usize::from(self.bits.decode(table)?);
        if size > 11 {
            return Err(damaged("a DC difference too large"));
        }
        // Wrapping as libjpeg's `int` does, then cut to its 16-bit `JCOEF`: only a hostile scan gets there.
        self.predictors[i] = self.predictors[i].wrapping_add(self.bits.value(size)?);
        block[0] = self.predictors[i].wrapping_shl(u32::from(self.spec.al)) as i16;
        Ok(())
    }

    /// The AC coefficients of a sequential block (band 1..63, no shift), or of a first progressive AC scan, whose
    /// end-of-band symbols may start an EOB run over the next blocks.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the cut to i16 is libjpeg's JCOEF, as in `dc_first`"
    )]
    fn ac_first(&mut self, i: usize, block: &mut [i16; 64]) -> Result<(), Refused> {
        let (ss, se) = match self.kind {
            Kind::Sequential => (1, 63),
            _ => (self.spec.ss, self.spec.se),
        };
        if self.eobrun > 0 {
            self.eobrun -= 1;
            return Ok(());
        }
        let table = self.spec.ac[i].as_ref().expect("checked by read_scan");
        let al = u32::from(self.spec.al);
        let mut k = ss;
        while k <= se {
            let rs = self.bits.decode(table)?;
            let (run, size) = (usize::from(rs >> 4), usize::from(rs & 15));
            if size == 0 {
                if run == 15 {
                    k += 16;
                    continue;
                }
                if self.kind != Kind::Sequential {
                    let run = u32::from(rs >> 4);
                    self.eobrun = (1 << run) + self.bits.bits(run as usize)? - 1;
                }
                break;
            }
            k += run;
            if k > se {
                return Err(damaged("coefficients past the band"));
            }
            let value = self.bits.value(size)?;
            block[UNZIGZAG[k]] = value.wrapping_shl(al) as i16;
            k += 1;
        }
        Ok(())
    }

    /// A refinement of an AC band (T.81 G.1.2.3, libjpeg's `decode_mcu_AC_refine`): a correction bit for every
    /// coefficient already nonzero, new coefficients of ±1 placed by runs counting only the zero ones, and the same
    /// corrections through every block of an EOB run. It needs every coefficient of the band, not only the ones
    /// the scale keeps: whether one is nonzero decides how the bits are read.
    fn ac_refine(&mut self, block: &mut [i16; 64]) -> Result<(), Refused> {
        let (ss, se, al) = (self.spec.ss, self.spec.se, self.spec.al);
        let p1: i16 = 1 << al;
        let m1: i16 = -1 << al;
        let mut k = ss;
        if self.eobrun == 0 {
            let table = self.spec.ac[0].as_ref().expect("checked by read_scan");
            while k <= se {
                let rs = self.bits.decode(table)?;
                let (mut run, size) = (i32::from(rs >> 4), rs & 15);
                let mut value = 0;
                if size != 0 {
                    if size != 1 {
                        return Err(damaged("a refinement coefficient larger than one bit"));
                    }
                    value = if self.bits.bits(1)? == 1 { p1 } else { m1 };
                } else if run != 15 {
                    let run = u32::from(rs >> 4);
                    self.eobrun = (1 << run) + self.bits.bits(run as usize)?;
                    break;
                }
                // Past `run` zero coefficients (correcting the nonzero ones on the way) to the new one's place.
                let mut placed = false;
                while k <= se {
                    let z = UNZIGZAG[k];
                    if block[z] != 0 {
                        self.bits.correct(&mut block[z], p1, m1)?;
                    } else {
                        if run == 0 {
                            placed = true;
                            break;
                        }
                        run -= 1;
                    }
                    k += 1;
                }
                if value != 0 {
                    if !placed {
                        return Err(damaged("a refinement coefficient past the band"));
                    }
                    block[UNZIGZAG[k]] = value;
                }
                k += 1;
            }
        }
        if self.eobrun > 0 {
            // In an EOB run: only corrections to the coefficients already nonzero.
            while k <= se {
                let z = UNZIGZAG[k];
                if block[z] != 0 {
                    self.bits.correct(&mut block[z], p1, m1)?;
                }
                k += 1;
            }
            self.eobrun -= 1;
        }
        Ok(())
    }
}

/// Decodes the picture at `1/scale` of each side (`scale` 1, 2, 4 or 8) from `cursors`, one per scan in file order,
/// handing `row` each component's sample rows in order, cropped to the component's scaled size: per MCU row, all of
/// the first component's rows, then the next's. Only one MCU row of coefficients is held: every scan is run over it
/// before it is turned into samples.
pub(super) fn rows<R: Read, S: std::ops::DerefMut<Target = Source<R>>>(
    head: &Head,
    layout: &Layout,
    scale: usize,
    cursors: &mut [Cursor<S>],
    mut row: impl FnMut(usize, &[u8]) -> Result<(), Refused>,
) -> Result<(), Refused> {
    let n = 8 / scale;
    let m = kernel(n);
    // Per component, its table in natural order.
    let quant: Vec<[f32; 64]> = head
        .components
        .iter()
        .map(|c| {
            let table = head.quant[c.tq].expect("checked by read_scan");
            let mut q = [0f32; 64];
            for (k, &value) in table.zigzag.iter().enumerate() {
                q[UNZIGZAG[k]] = f32::from(value);
            }
            q
        })
        .collect();
    let mut coefficients: Vec<Vec<[i16; 64]>> = layout
        .parts
        .iter()
        .map(|p| vec![[0; 64]; p.blocks_x * p.v])
        .collect();
    let mut strips: Vec<Vec<u8>> = layout
        .parts
        .iter()
        .map(|p| vec![0; p.blocks_x * n * p.v * n])
        .collect();
    let mut emitted = vec![0usize; layout.parts.len()];
    let mut block = [0f32; 64];
    for my in 0..layout.mcus_y {
        for c in &mut coefficients {
            c.fill([0; 64]);
        }
        for cursor in cursors.iter_mut() {
            cursor.row(my, layout, &mut coefficients)?;
        }
        for (c, part) in layout.parts.iter().enumerate() {
            let stride = part.blocks_x * n;
            for (i, coefficient) in coefficients[c].iter().enumerate() {
                let (by, bx) = (i / part.blocks_x, i % part.blocks_x);
                let mut ac = false;
                for v in 0..n {
                    for u in 0..n {
                        let z = v * 8 + u;
                        block[z] = f32::from(coefficient[z]) * quant[c][z];
                        ac |= z > 0 && coefficient[z] != 0;
                    }
                }
                let at = by * n * stride + bx * n;
                idct(&block, ac, n, &m, &mut strips[c][at..], stride);
            }
            let (width, height) = (part.width.div_ceil(scale), part.height.div_ceil(scale));
            for y in 0..part.v * n {
                if emitted[c] == height {
                    break;
                }
                row(c, &strips[c][y * stride..y * stride + width])?;
                emitted[c] += 1;
            }
        }
    }
    Ok(())
}
