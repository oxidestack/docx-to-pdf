//! The first of two reads of a JPEG with several scans: its entropy-coded data copied to a scratch file
//! as it streams in, each scan's start in that file noted with its header, Huffman tables and restart interval, the
//! tables and intervals defined between scans followed. The second read runs one [`Spooled`] cursor a scan.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use super::decode::{ScanSpec, Source};
use super::{Head, Refused, damaged, read_huffman, read_quant, read_scan};

/// The most scans read. libjpeg's progressive scripts take 10, `jpeg-encoder`'s up to 64 a component (193 for
/// three); each costs a cursor of [`CURSOR_BUFFER`] bytes and its tables, and every scan is a pass over the picture.
pub const MAX_SCANS: usize = 256;

/// The read buffer of each scan's cursor.
pub(super) const CURSOR_BUFFER: usize = 8 * 1024;

/// Reads the rest of the JPEG `source`, whose first scan's header `first` has just been read, copying each scan's
/// entropy-coded data to a scratch file in `scratch`, each ended by the marker that ended it (`EOI` added after the last, so a
/// picture cut short reads as one whose last scan ends there). Returns the file and each scan with its offset in it.
///
/// `head` follows the tables defined between scans: a Huffman table or restart interval changed between scans holds
/// from the next one on; a quantisation table may not change once a component using it has been scanned (libjpeg
/// latches it then), so such a picture is refused rather than read with the wrong one. A sequential picture must
/// scan each component once; a progressive one must have a first DC scan for each.
pub(super) fn read<R: Read>(
    source: &mut Source<R>,
    head: &mut Head,
    first: ScanSpec,
    scratch: &Path,
) -> Result<(File, Vec<(u64, ScanSpec)>), Refused> {
    let progressive = head.sof == 0xC2;
    let mut spool = BufWriter::with_capacity(64 * 1024, tempfile::tempfile_in(scratch)?);
    let mut written = 0u64;
    let mut scanned = vec![0usize; head.components.len()];
    let mut dc_first = vec![false; head.components.len()];
    let mut scans: Vec<(u64, ScanSpec)> = Vec::new();
    let mut next = Some(first);
    'file: while let Some(scan) = next.take() {
        if scans.len() == MAX_SCANS {
            return Err(Refused::TooManyScans);
        }
        for &c in &scan.comps {
            scanned[c] += 1;
            if !progressive && scanned[c] > 1 {
                return Err(damaged("a component in two sequential scans"));
            }
            dc_first[c] |= scan.ss == 0 && scan.ah == 0;
        }
        scans.push((written, scan));
        // The scan's entropy-coded data, up to the marker that ends it.
        let mut marker = loop {
            let Some(b) = source.try_byte()? else {
                break 'file;
            };
            if b != 0xFF {
                spool.write_all(&[b])?;
                written += 1;
                continue;
            }
            let mut after = 0xFF;
            while after == 0xFF {
                match source.try_byte()? {
                    Some(b) => after = b,
                    None => break 'file,
                }
            }
            spool.write_all(&[0xFF, after])?;
            written += 2;
            if after != 0 && !(0xD0..=0xD7).contains(&after) {
                break after;
            }
        };
        // The segments up to the next scan or the end.
        loop {
            match marker {
                0xD9 => break 'file,
                0x01 | 0xD0..=0xD7 => {}
                0xD8 => return Err(damaged("a second SOI")),
                _ => {
                    let length = usize::from(source.u16()?);
                    if length < 2 {
                        return Err(damaged("a segment shorter than its length"));
                    }
                    if matches!(marker, 0xE0..=0xEF | 0xFE) {
                        // Past the first scan, nothing is kept: the encoder has the head's segments.
                        source.skip(length - 2)?;
                    } else {
                        let data = source.bytes(length - 2)?;
                        match marker {
                            0xDA => {
                                next = Some(read_scan(head, &data)?);
                                continue 'file;
                            }
                            0xDB => {
                                let before = head.quant;
                                read_quant(head, &data)?;
                                let changed =
                                    head.components.iter().enumerate().any(|(c, comp)| {
                                        scanned[c] > 0 && head.quant[comp.tq] != before[comp.tq]
                                    });
                                if changed {
                                    return Err(damaged(
                                        "a quantisation table changed after its component's scan",
                                    ));
                                }
                            }
                            0xC4 => read_huffman(head, &data)?,
                            0xDD => {
                                let at = data.get(..2).ok_or_else(|| damaged("a short DRI"))?;
                                head.restart = usize::from(u16::from_be_bytes([at[0], at[1]]));
                            }
                            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                                return Err(damaged("a second frame"));
                            }
                            _ => {} // DNL, DAC, DHP, EXP, JPGn: nothing this reads
                        }
                    }
                }
            }
            if source.byte()? != 0xFF {
                return Err(damaged("a marker expected"));
            }
            marker = source.byte()?;
            while marker == 0xFF {
                marker = source.byte()?;
            }
        }
    }
    spool.write_all(&[0xFF, 0xD9])?;
    if progressive && dc_first.contains(&false) {
        return Err(damaged("a component without its first DC scan"));
    }
    if !progressive && scanned.contains(&0) {
        return Err(damaged("a component in no scan"));
    }
    let file = spool.into_inner().map_err(io::IntoInnerError::into_error)?;
    Ok((file, scans))
}

/// The scratch file read from `pos` on; cursors share the file, each seeking to its own place before it reads.
pub(super) struct Spooled<'a> {
    file: &'a File,
    pos: u64,
}

impl<'a> Spooled<'a> {
    pub(super) fn new(file: &'a File, pos: u64) -> Self {
        Spooled { file, pos }
    }
}

impl Read for Spooled<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut file = self.file;
        file.seek(SeekFrom::Start(self.pos))?;
        let n = file.read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}
