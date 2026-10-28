//! The allowlist: a document's format decided from its bytes alone, so only formats chosen on purpose reach
//! LibreOffice, and a new one joins by a decision and a test, not by accident. Four formats pass — DOCX, DOC, ODT and
//! RTF — with the Word-family variants moalhaddar/docx-to-pdf really converts, a real rendering and not just a 200
//! (if the original accepted and processed them, so does this service): DOCM, DOTX, OTT and flat ODT. Everything else
//! is refused with 415 before a pool instance is touched — an RTF behind a UTF-8 byte-order mark among them, since
//! moalhaddar/docx-to-pdf typesets its source as plain text. The part's file name and `Content-Type` play no part.
//!
//! What identifies each format, and so where "damaged" stops and "not this format" starts:
//!
//! - **RTF**: the bytes start with `{\rtf`. Nothing after the signature is read.
//! - **DOCX**: a ZIP whose `[Content_Types].xml` entry (part names compare ASCII-case-insensitively, as OPC
//!   says) declares the WordprocessingML main document type, in an `Override` or a `Default`. **DOCM**
//!   and **DOTX** likewise, by the macro-enabled document's and the template's main part types.
//! - **ODT**: a ZIP whose `mimetype` entry is `application/vnd.oasis.opendocument.text`; **OTT** the same
//!   with `…text-template`.
//! - **Flat ODT**: what LibreOffice's flat XML detection, and so moalhaddar/docx-to-pdf, takes for one — an XML
//!   declaration first (after a UTF-8 byte-order mark at most), then, past comments, processing
//!   instructions and a doctype, an `office:document` root whose `office:mimetype` is
//!   `"application/vnd.oasis.opendocument.text"`, double-quoted and ending within the first 4000 bytes.
//! - **DOC**: an OLE compound file whose root storage holds a stream named `WordDocument` (names compare
//!   case-insensitively, as the compound file format says). A `WordDocument` inside a sub-storage — a Word
//!   object embedded in a spreadsheet — does not count.
//!
//! Only the identifying structure has to be intact. A ZIP is read through its central directory and, when
//! that is missing or unreadable (a truncated download), by walking its local headers from the start, so a
//! truncated DOCX or ODT whose identifying entry survived still passes; a compound file needs its header,
//! the FAT sectors of its directory chain and the directory itself. Damage anywhere else — a corrupt
//! `word/document.xml`, a garbage `WordDocument` stream, an RTF cut short — passes the allowlist and is
//! LibreOffice's to judge (422).
//!
//! **Inflation**: what a DOCX's or ODT's parts inflate to is bounded apart from the document's
//! size, since LibreOffice writes an inflated part to `/tmp` while it loads it (a 7.4 MiB DOCX
//! wrote 1.5 GiB). [`inflation`] counts it for the ZIP formats on the allowlist: the uncompressed sizes the
//! directory declares first, which refuse a plain bomb without inflating a byte; then, since those sizes are
//! the sender's word, every entry is inflated for real and its output counted, up to the limit and no further.
//! A directory that understates its sizes (or an entry the directory lists twice) thus gains nothing, and the
//! work is bounded by the limit, not by what the bomb would inflate to.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use miniz_oxide::inflate::stream::{InflateState, inflate};
use miniz_oxide::{DataFormat, MZError, MZFlush, MZStatus};

use crate::num::usize_from_u64;

/// A format on the allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    /// A Word document (Office Open XML).
    Docx,
    /// A Word 97-2003 document (OLE compound file).
    Doc,
    /// An OpenDocument text.
    Odt,
    /// Rich Text Format.
    Rtf,
    /// A macro-enabled Word document: DOCX's container with a different main part type.
    Docm,
    /// A Word template.
    Dotx,
    /// An ODF text template.
    Ott,
    /// A flat ODT: the ODF text document as one XML file.
    Fodt,
}

impl Format {
    /// The format's name in telemetry: the `format` attribute.
    pub fn name(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Doc => "doc",
            Self::Odt => "odt",
            Self::Rtf => "rtf",
            Self::Docm => "docm",
            Self::Dotx => "dotx",
            Self::Ott => "ott",
            Self::Fodt => "fodt",
        }
    }

    /// Whether the format is a ZIP container: DOCX, DOCM, DOTX, ODT or OTT.
    pub fn is_zip(self) -> bool {
        matches!(
            self,
            Self::Docx | Self::Docm | Self::Dotx | Self::Odt | Self::Ott
        )
    }

    /// The file-name extension the document is given when it is handed to LibreOffice, named by its sniffed format
    /// rather than the client's file name.
    pub fn extension(self) -> &'static str {
        self.name()
    }
}

/// The detail of the 415 answer (`docs/guide/api.md`, "The formats accepted").
pub const REFUSED: &str = "the document is not DOCX, DOC, ODT or RTF";

/// The `format` attribute of a refused document.
pub const UNSUPPORTED: &str = "unsupported";

/// The format of the file at `path`, or `None` when it is not on the allowlist. An error is an I/O
/// failure reading the file, never a verdict on its content.
pub async fn sniff_file(path: &Path) -> io::Result<Option<Format>> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || sniff(&mut std::fs::File::open(path)?))
        .await
        .map_err(io::Error::other)?
}

/// What a document's parts inflate to, as far as [`inflation`] counted them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inflation {
    /// Not a ZIP format on the allowlist (DOC, RTF, flat ODT, or refused): nothing inflates, nothing counted.
    NotCounted,
    /// Every entry inflated, to this many bytes in all, within the limit.
    Within(u64),
    /// The directory declares this many uncompressed bytes in all, past the limit; nothing was inflated.
    DeclaredPast(u64),
    /// The entries inflated past the limit: this many bytes when counting stopped, although the directory
    /// declared less.
    InflatedPast(u64),
}

impl Inflation {
    /// Whether the document is past the limit.
    pub fn is_past(self) -> bool {
        matches!(self, Self::DeclaredPast(_) | Self::InflatedPast(_))
    }
}

/// [`inflation`] of the file at `path`. An error is an I/O failure reading the file.
pub async fn inflation_file(path: &Path, limit: u64) -> io::Result<Inflation> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || inflation(&mut std::fs::File::open(path)?, limit))
        .await
        .map_err(io::Error::other)?
}

/// What the parts of the DOCX, DOCM, DOTX, ODT or OTT `source` holds inflate to, counted up to `limit` bytes
/// (the module's documentation says how). Any other source is [`Inflation::NotCounted`]: the allowlist
/// judges it. Damage is not a verdict here: an entry that cannot be found or inflated counts what came out of
/// it before the damage, and LibreOffice judges the rest (422).
pub fn inflation<R: Read + Seek>(source: &mut R, limit: u64) -> io::Result<Inflation> {
    match sniff(source)? {
        Some(Format::Docx | Format::Docm | Format::Dotx | Format::Odt | Format::Ott) => {
            zip::inflation(&mut Source::new(source)?, limit)
        }
        _ => Ok(Inflation::NotCounted),
    }
}

/// The entries of the ZIP `source`, as [`zip::listing`] says, for the pictures step to read and rewrite.
pub(crate) fn zip_listing<R: Read + Seek>(source: &mut R) -> io::Result<Option<Vec<zip::Listed>>> {
    zip::listing(&mut Source::new(source)?)
}

/// The first `n` bytes of a listed entry's content, as [`zip::head`] says.
pub(crate) fn zip_head<R: Read + Seek>(
    source: &mut R,
    entry: &zip::Listed,
    n: usize,
) -> io::Result<Option<Vec<u8>>> {
    zip::head(&mut Source::new(source)?, entry, n)
}

/// The format of the bytes `source` holds, or `None` when they are not on the allowlist. Reads are
/// bounded: only the structures named in the module's documentation are read, never the whole source.
pub fn sniff<R: Read + Seek>(source: &mut R) -> io::Result<Option<Format>> {
    let mut source = Source::new(source)?;
    let Some(head) = source.read_at(0, 8)? else {
        // Shorter than any signature but RTF's.
        let head = source
            .read_at(0, usize_from_u64(source.len))?
            .unwrap_or_default();
        return Ok(head.starts_with(RTF_SIGNATURE).then_some(Format::Rtf));
    };
    if head.starts_with(RTF_SIGNATURE) {
        Ok(Some(Format::Rtf))
    } else if head == OLE_SIGNATURE {
        ole::sniff(&mut source)
    } else if head.starts_with(ZIP_LOCAL_HEADER) {
        zip::sniff(&mut source)
    } else {
        let window = source.read_up_to(0, FLAT_MIMETYPE_WINDOW)?;
        Ok(flat::is_flat_odt(&window).then_some(Format::Fodt))
    }
}

const RTF_SIGNATURE: &[u8] = b"{\\rtf";

/// How far into a flat ODT its root's `office:mimetype` may end, closing quote included: LibreOffice's
/// flat XML detection reads no further (moalhaddar/docx-to-pdf renders a flat ODT whose closing quote is
/// byte 3999 as a document, and typesets one whose quote is byte 4000 as plain text).
const FLAT_MIMETYPE_WINDOW: usize = 4000;
const OLE_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const ZIP_LOCAL_HEADER: &[u8] = b"PK\x03\x04";

/// A seekable source of known length, read in bounded pieces. A read past its end is `None`, not an
/// error: a structure that points outside the file is a malformed file, not an I/O failure.
struct Source<'a, R> {
    inner: &'a mut R,
    len: u64,
}

impl<'a, R: Read + Seek> Source<'a, R> {
    fn new(inner: &'a mut R) -> io::Result<Self> {
        let len = inner.seek(SeekFrom::End(0))?;
        Ok(Self { inner, len })
    }

    /// `n` bytes at `offset`, or `None` when they are not all inside the source.
    fn read_at(&mut self, offset: u64, n: usize) -> io::Result<Option<Vec<u8>>> {
        match offset.checked_add(n as u64) {
            Some(end) if end <= self.len => {}
            _ => return Ok(None),
        }
        self.inner.seek(SeekFrom::Start(offset))?;
        let mut buffer = vec![0; n];
        self.inner.read_exact(&mut buffer)?;
        Ok(Some(buffer))
    }

    /// Up to `n` bytes at `offset`, fewer at the end of the source.
    fn read_up_to(&mut self, offset: u64, n: usize) -> io::Result<Vec<u8>> {
        let available = usize_from_u64(self.len.saturating_sub(offset).min(n as u64));
        Ok(self.read_at(offset, available)?.unwrap_or_default())
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Flat ODT: one XML file.
mod flat {
    const BOM: &[u8] = b"\xEF\xBB\xBF";
    const ROOT: &[u8] = b"office:document";
    const MIMETYPE: &[u8] = b"office:mimetype";
    const ODT_MIMETYPE: &[u8] = b"application/vnd.oasis.opendocument.text";

    /// Whether `window`, the start of a file, is a flat ODT's: a UTF-8 byte-order mark at most, an XML
    /// declaration, comments, processing instructions or a doctype, then an `office:document` start tag
    /// whose `office:mimetype` is the ODF text type in double quotes, all inside `window`.
    pub(super) fn is_flat_odt(window: &[u8]) -> bool {
        let rest = window.strip_prefix(BOM).unwrap_or(window);
        if !rest.starts_with(b"<?xml") || !rest.get(5).is_some_and(u8::is_ascii_whitespace) {
            return false;
        }
        let mut rest = rest;
        loop {
            rest = rest.trim_ascii_start();
            let skip = if rest.starts_with(b"<!--") {
                after(rest, b"-->")
            } else if rest.starts_with(b"<?") {
                after(rest, b"?>")
            } else if rest.starts_with(b"<!") {
                after(rest, b">")
            } else if let Some(tag) = rest.strip_prefix(b"<").and_then(|r| r.strip_prefix(ROOT)) {
                return tag.first().is_some_and(u8::is_ascii_whitespace) && root_is_text(tag);
            } else {
                return false;
            };
            let Some(next) = skip else { return false };
            rest = next;
        }
    }

    /// What follows the first `marker` in `bytes`.
    fn after<'a>(bytes: &'a [u8], marker: &[u8]) -> Option<&'a [u8]> {
        let at = bytes.windows(marker.len()).position(|w| w == marker)?;
        Some(&bytes[at + marker.len()..])
    }

    /// Whether the attributes opening `tag` (a start tag past its name) hold `office:mimetype` equal to the
    /// ODF text type, double-quoted as LibreOffice's detection wants it.
    fn root_is_text(mut tag: &[u8]) -> bool {
        loop {
            tag = tag.trim_ascii_start();
            let name_len = tag
                .iter()
                .position(|&b| b == b'=' || b == b'>' || b == b'/' || b.is_ascii_whitespace())
                .unwrap_or(tag.len());
            if name_len == 0 {
                return false; // the tag ends, or is cut off, without the attribute
            }
            let (name, rest) = tag.split_at(name_len);
            let Some(rest) = rest.trim_ascii_start().strip_prefix(b"=") else {
                return false;
            };
            let rest = rest.trim_ascii_start();
            let Some(&quote) = rest.first().filter(|&&q| q == b'"' || q == b'\'') else {
                return false;
            };
            let Some(end) = rest[1..].iter().position(|&b| b == quote) else {
                return false;
            };
            let value = &rest[1..=end];
            if name == MIMETYPE {
                return quote == b'"' && value == ODT_MIMETYPE;
            }
            tag = &rest[end + 2..];
        }
    }
}

/// DOCX and ODT: the ZIP container.
pub(crate) mod zip {
    use super::{
        DataFormat, Format, InflateState, Inflation, MZError, MZFlush, MZStatus, Read, Seek,
        Source, ZIP_LOCAL_HEADER, inflate, io, u16_at, u32_at, usize_from_u64,
    };

    /// The WordprocessingML main document part's content type (ECMA-376 Part 1 §11.3.10).
    const DOCX_MAIN: &[u8] =
        b"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml";
    /// The ODF text document's media type (ODF 1.3 Part 2 §3.3).
    const ODT_MIMETYPE: &[u8] = b"application/vnd.oasis.opendocument.text";
    /// The macro-enabled Word document's main part type ([MS-OFFMACRO2] §2.2.1.4).
    const DOCM_MAIN: &[u8] = b"application/vnd.ms-word.document.macroEnabled.main+xml";
    /// The Word template's main part type (ECMA-376 Part 1 §11.3.10).
    const DOTX_MAIN: &[u8] =
        b"application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml";
    /// The ODF text template's media type (ODF 1.3 Part 2 §3.3, Part 1 Appendix C).
    const OTT_MIMETYPE: &[u8] = b"application/vnd.oasis.opendocument.text-template";

    const CONTENT_TYPES: &[u8] = b"[Content_Types].xml";
    const MIMETYPE: &[u8] = b"mimetype";

    /// The largest identifying entry read, compressed or not: a real `[Content_Types].xml` is a few KiB.
    const ENTRY_LIMIT: usize = 1 << 20;

    const STORED: u16 = 0;
    const DEFLATED: u16 = 8;
    /// General-purpose flag bit 3: sizes and CRC follow the data in a data descriptor.
    const DATA_DESCRIPTOR: u16 = 1 << 3;

    /// An entry as a directory or a local header describes it.
    struct Entry {
        name: Vec<u8>,
        method: u16,
        /// The compressed size, when known.
        compressed: Option<u64>,
        /// The uncompressed size the entry declares, when it declares one (not in a data descriptor, not
        /// ZIP64's `0xFFFFFFFF`).
        uncompressed: Option<u64>,
        /// Where the entry's local header starts.
        local_header: u64,
        /// The central directory's record of it, name, extra field and comment included; empty when the entry was
        /// found by walking local headers.
        record: Vec<u8>,
    }

    pub(super) fn sniff<R: Read + Seek>(source: &mut Source<'_, R>) -> io::Result<Option<Format>> {
        let entries = match central_directory(source)? {
            Some(entries) => entries,
            None => local_headers(source)?,
        };
        let find = |name: &[u8], case_insensitive: bool| {
            entries.iter().find(|e| {
                if case_insensitive {
                    e.name.eq_ignore_ascii_case(name)
                } else {
                    e.name == name
                }
            })
        };
        if let Some(entry) = find(CONTENT_TYPES, true)
            && let Some(content) = read_entry(source, entry)?
            && let Some(format) = [
                (DOCX_MAIN, Format::Docx),
                (DOCM_MAIN, Format::Docm),
                (DOTX_MAIN, Format::Dotx),
            ]
            .into_iter()
            .find_map(|(main, format)| declares(&content, main).then_some(format))
        {
            return Ok(Some(format));
        }
        if let Some(entry) = find(MIMETYPE, false)
            && let Some(content) = read_entry(source, entry)?
        {
            let mimetype = content.trim_ascii();
            if mimetype == ODT_MIMETYPE {
                return Ok(Some(Format::Odt));
            }
            if mimetype == OTT_MIMETYPE {
                return Ok(Some(Format::Ott));
            }
        }
        Ok(None)
    }

    /// Whether `xml` has an attribute whose value is `content_type`, quoted either way: a
    /// `ContentType="…"` of an `Override` or a `Default`. MIME types compare case-insensitively.
    fn declares(xml: &[u8], content_type: &[u8]) -> bool {
        let xml = xml.to_ascii_lowercase();
        let wanted = content_type.to_ascii_lowercase();
        (0..xml.len().saturating_sub(wanted.len() + 1)).any(|start| {
            let quote = xml[start];
            (quote == b'"' || quote == b'\'')
                && xml[start + 1..].starts_with(&wanted)
                && xml.get(start + 1 + wanted.len()) == Some(&quote)
        })
    }

    /// The entries the central directory lists, or `None` when there is no readable one.
    fn central_directory<R: Read + Seek>(
        source: &mut Source<'_, R>,
    ) -> io::Result<Option<Vec<Entry>>> {
        // The end-of-central-directory record: 22 bytes and a comment of up to 65535.
        const EOCD: &[u8] = b"PK\x05\x06";
        let window = 22 + 0xFFFF;
        let start = source.len.saturating_sub(window as u64);
        let tail = source.read_up_to(start, window)?;
        let Some(at) = (0..tail.len().saturating_sub(21))
            .rev()
            .find(|&i| tail[i..].starts_with(EOCD))
        else {
            return Ok(None);
        };
        let eocd = &tail[at..];
        let count = u16_at(eocd, 10) as usize;
        let size = u32_at(eocd, 12) as usize;
        let offset = u64::from(u32_at(eocd, 16));
        if size > ENTRY_LIMIT * 8 {
            return Ok(None);
        }
        let Some(directory) = source.read_at(offset, size)? else {
            return Ok(None);
        };
        let mut entries = Vec::with_capacity(count.min(4096));
        let mut at = 0;
        for _ in 0..count {
            let Some(header) = directory.get(at..at + 46) else {
                return Ok(None);
            };
            if !header.starts_with(b"PK\x01\x02") {
                return Ok(None);
            }
            let name_len = u16_at(header, 28) as usize;
            let extra_len = u16_at(header, 30) as usize;
            let comment_len = u16_at(header, 32) as usize;
            let Some(name) = directory.get(at + 46..at + 46 + name_len) else {
                return Ok(None);
            };
            if directory.len() < at + 46 + name_len + extra_len + comment_len {
                return Ok(None);
            }
            entries.push(Entry {
                name: name.to_vec(),
                method: u16_at(header, 10),
                compressed: Some(u64::from(u32_at(header, 20))),
                uncompressed: declared(u32_at(header, 24)),
                local_header: u64::from(u32_at(header, 42)),
                record: directory[at..at + 46 + name_len + extra_len + comment_len].to_vec(),
            });
            at += 46 + name_len + extra_len + comment_len;
        }
        Ok(Some(entries))
    }

    /// The entries found by walking local headers from the start, up to the first one that cannot be read
    /// or skipped: what a truncated archive still shows.
    fn local_headers<R: Read + Seek>(source: &mut Source<'_, R>) -> io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut at = 0u64;
        while let Some(header) = source.read_at(at, 30)? {
            if !header.starts_with(ZIP_LOCAL_HEADER) {
                break;
            }
            let flags = u16_at(&header, 6);
            let method = u16_at(&header, 8);
            let compressed = u64::from(u32_at(&header, 18));
            let uncompressed = u32_at(&header, 22);
            let name_len = u16_at(&header, 26) as usize;
            let extra_len = u64::from(u16_at(&header, 28));
            let Some(name) = source.read_at(at + 30, name_len)? else {
                break;
            };
            let data = at + 30 + name_len as u64 + extra_len;
            let deferred = flags & DATA_DESCRIPTOR != 0;
            entries.push(Entry {
                name,
                method,
                compressed: (!deferred).then_some(compressed),
                uncompressed: if deferred {
                    None
                } else {
                    declared(uncompressed)
                },
                local_header: at,
                record: Vec::new(),
            });
            // The next header: past the data, and past a data descriptor when there is one.
            let length = match (deferred, method) {
                (false, _) => compressed,
                (true, DEFLATED) => match inflate_into(source, data, None, |_| true)? {
                    Inflated::End { consumed } => consumed,
                    Inflated::Stopped | Inflated::Corrupt => break,
                },
                (true, _) => break,
            };
            at = data + length;
            if deferred {
                // An optional signature, then CRC-32 and the two sizes (ZIP64's longer form is not
                // expected in a document).
                let signed = source
                    .read_at(at, 4)?
                    .is_some_and(|s| s == b"PK\x07\x08".as_slice());
                at += if signed { 16 } else { 12 };
            }
        }
        Ok(entries)
    }

    /// The content of `entry`, or `None` when it cannot be read: outside the file, an unknown method,
    /// corrupt, or larger than [`ENTRY_LIMIT`].
    fn read_entry<R: Read + Seek>(
        source: &mut Source<'_, R>,
        entry: &Entry,
    ) -> io::Result<Option<Vec<u8>>> {
        let Some(data) = data_start(source, entry)? else {
            return Ok(None);
        };
        match entry.method {
            STORED => {
                let Some(size) = entry.compressed else {
                    return Ok(None);
                };
                if size > ENTRY_LIMIT as u64 {
                    return Ok(None);
                }
                source.read_at(data, usize_from_u64(size))
            }
            DEFLATED => Ok(inflate_from(source, data, entry.compressed, ENTRY_LIMIT)?
                .map(|(content, _)| content)),
            _ => Ok(None),
        }
    }

    /// The uncompressed size a 32-bit field declares: `None` for ZIP64's marker, whose real size is elsewhere.
    fn declared(size: u32) -> Option<u64> {
        (size != u32::MAX).then_some(u64::from(size))
    }

    /// Where `entry`'s data starts, past its local header, or `None` when the header is not there.
    fn data_start<R: Read + Seek>(
        source: &mut Source<'_, R>,
        entry: &Entry,
    ) -> io::Result<Option<u64>> {
        let Some(header) = source.read_at(entry.local_header, 30)? else {
            return Ok(None);
        };
        if !header.starts_with(ZIP_LOCAL_HEADER) {
            return Ok(None);
        }
        Ok(Some(
            entry.local_header
                + 30
                + u64::from(u16_at(&header, 26))
                + u64::from(u16_at(&header, 28)),
        ))
    }

    /// The entries' inflated total, up to `limit` (the module's documentation).
    pub(super) fn inflation<R: Read + Seek>(
        source: &mut Source<'_, R>,
        limit: u64,
    ) -> io::Result<Inflation> {
        let entries = match central_directory(source)? {
            Some(entries) => entries,
            None => local_headers(source)?,
        };
        let declared = entries
            .iter()
            .filter_map(|e| e.uncompressed)
            .fold(0u64, u64::saturating_add);
        if declared > limit {
            return Ok(Inflation::DeclaredPast(declared));
        }
        let mut total = 0u64;
        for entry in &entries {
            let remaining = limit - total;
            let counted = match entry.method {
                DEFLATED => {
                    let Some(data) = data_start(source, entry)? else {
                        continue;
                    };
                    let mut counted = 0u64;
                    inflate_into(source, data, entry.compressed, |chunk| {
                        counted += chunk.len() as u64;
                        counted <= remaining
                    })?;
                    counted
                }
                // Stored, or a method LibreOffice cannot inflate: its bytes as they are, never more than the file.
                _ => entry.compressed.unwrap_or(0).min(source.len),
            };
            total = total.saturating_add(counted);
            if total > limit {
                return Ok(Inflation::InflatedPast(total));
            }
        }
        Ok(Inflation::Within(total))
    }

    /// An entry of a ZIP whose central directory is readable and has no ZIP64 field, for a rewrite.
    pub struct Listed {
        pub(crate) name: Vec<u8>,
        /// The central directory's record, name, extra field and comment included.
        pub(crate) record: Vec<u8>,
        /// Where the entry's compressed data starts, and its length.
        pub(crate) data: u64,
        pub(crate) compressed: u64,
    }

    /// The entries in the central directory's order, or `None` when there is no readable directory, an entry's local
    /// header is not where it says, or a size is ZIP64's: a ZIP that is not rewritten.
    pub(super) fn listing<R: Read + Seek>(
        source: &mut Source<'_, R>,
    ) -> io::Result<Option<Vec<Listed>>> {
        let Some(entries) = central_directory(source)? else {
            return Ok(None);
        };
        let mut listed = Vec::with_capacity(entries.len());
        for entry in entries {
            let (Some(compressed), Some(_)) = (entry.compressed, entry.uncompressed) else {
                return Ok(None);
            };
            if compressed == u64::from(u32::MAX) || entry.local_header == u64::from(u32::MAX) {
                return Ok(None);
            }
            let Some(data) = data_start(source, &entry)? else {
                return Ok(None);
            };
            if data
                .checked_add(compressed)
                .is_none_or(|end| end > source.len)
            {
                return Ok(None);
            }
            listed.push(Listed {
                name: entry.name,
                record: entry.record,
                data,
                compressed,
            });
        }
        Ok(Some(listed))
    }

    /// The first `n` bytes of `entry`'s content (fewer when it is shorter), or `None` when it cannot be read.
    pub(super) fn head<R: Read + Seek>(
        source: &mut Source<'_, R>,
        entry: &Listed,
        n: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        match u16_at(&entry.record, 10) {
            STORED => Ok(Some(
                source.read_up_to(entry.data, n.min(usize_from_u64(entry.compressed)))?,
            )),
            DEFLATED => {
                let mut kept = Vec::new();
                let outcome = inflate_into(source, entry.data, Some(entry.compressed), |chunk| {
                    let take = chunk.len().min(n - kept.len());
                    kept.extend_from_slice(&chunk[..take]);
                    kept.len() < n
                })?;
                Ok(match outcome {
                    Inflated::Corrupt if kept.is_empty() => None,
                    _ => Some(kept),
                })
            }
            _ => Ok(None),
        }
    }

    /// How an [`inflate_into`] ended.
    enum Inflated {
        /// The stream ended, after taking this many input bytes.
        End { consumed: u64 },
        /// The sink asked to stop.
        Stopped,
        /// The stream is corrupt, or ends before its end.
        Corrupt,
    }

    /// Inflates the raw deflate stream at `start`, reading at most `available` input bytes (to the end of the
    /// source when unknown), handing each piece of output to `sink` until it returns `false`.
    fn inflate_into<R: Read + Seek>(
        source: &mut Source<'_, R>,
        start: u64,
        available: Option<u64>,
        mut sink: impl FnMut(&[u8]) -> bool,
    ) -> io::Result<Inflated> {
        const CHUNK: usize = 64 * 1024;
        let end = available
            .and_then(|a| start.checked_add(a))
            .unwrap_or(source.len)
            .min(source.len);
        let mut state = InflateState::new_boxed(DataFormat::Raw);
        let mut output = vec![0u8; CHUNK];
        let mut position = start;
        let mut input = Vec::new();
        let mut used = 0;
        loop {
            if used == input.len() {
                if position >= end {
                    return Ok(Inflated::Corrupt);
                }
                input = source.read_up_to(position, CHUNK.min(usize_from_u64(end - position)))?;
                position += input.len() as u64;
                used = 0;
            }
            let result = inflate(&mut state, &input[used..], &mut output, MZFlush::None);
            used += result.bytes_consumed;
            if !sink(&output[..result.bytes_written]) {
                return Ok(Inflated::Stopped);
            }
            match result.status {
                Ok(MZStatus::StreamEnd) => {
                    let consumed = position - (input.len() - used) as u64 - start;
                    return Ok(Inflated::End { consumed });
                }
                Ok(_) => {}
                // No progress without more input: the next turn reads it, or ends at `end`.
                Err(MZError::Buf) if used == input.len() => {}
                Err(_) => return Ok(Inflated::Corrupt),
            }
        }
    }

    /// Inflates the raw deflate stream at `start`, reading at most `available` input bytes, and returns its
    /// output and how many input bytes it took; `None` for a corrupt or unfinished stream, or one whose output
    /// would exceed `keep`.
    fn inflate_from<R: Read + Seek>(
        source: &mut Source<'_, R>,
        start: u64,
        available: Option<u64>,
        keep: usize,
    ) -> io::Result<Option<(Vec<u8>, u64)>> {
        let mut kept = Vec::new();
        let outcome = inflate_into(source, start, available, |chunk| {
            if kept.len() + chunk.len() > keep {
                return false;
            }
            kept.extend_from_slice(chunk);
            true
        })?;
        Ok(match outcome {
            Inflated::End { consumed } => Some((kept, consumed)),
            Inflated::Stopped | Inflated::Corrupt => None,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::declares;

        #[test]
        fn a_content_type_is_declared_only_as_a_whole_quoted_value() {
            let t = b"application/x-main+xml";
            assert!(declares(
                br#"<Override ContentType="application/x-main+xml"/>"#,
                t
            ));
            assert!(declares(
                b"<Default ContentType='APPLICATION/X-Main+XML'/>",
                t
            ));
            assert!(!declares(
                br#"<Override ContentType="application/x-main+xml2"/>"#,
                t
            ));
            assert!(!declares(
                br#"<Override ContentType="xapplication/x-main+xml"/>"#,
                t
            ));
            assert!(!declares(
                br#"<Override ContentType="application/x-main+xml'/>"#,
                t
            ));
            assert!(!declares(b"application/x-main+xml", t));
            assert!(!declares(b"", t));
        }
    }
}

/// DOC: the OLE compound file (MS-CFB).
mod ole {
    use super::{Format, Read, Seek, Source, io, u16_at, u32_at, usize_from_u64};

    const NOSTREAM: u32 = 0xFFFF_FFFF;
    /// Sector numbers at or above this are markers (end of chain, free, FAT, DIFAT), not sectors.
    const MAX_SECTOR: u32 = 0xFFFF_FFFA;
    const ENTRY_SIZE: usize = 128;
    const STREAM: u8 = 2;
    const ROOT: u8 = 5;
    const WORD_DOCUMENT: &str = "WordDocument";
    /// The longest directory chain followed: a real document's directory is a few sectors, and the bound
    /// keeps a hostile chain cheap.
    const DIRECTORY_LIMIT: usize = 1024;

    struct CompoundFile {
        sector_size: u64,
        /// The header's first 109 FAT sector numbers.
        header_difat: Vec<u32>,
        first_difat_sector: u32,
        /// How many sectors the file can hold: a bound on every chain.
        sectors: u64,
    }

    pub(super) fn sniff<R: Read + Seek>(source: &mut Source<'_, R>) -> io::Result<Option<Format>> {
        let Some(header) = source.read_at(0, 512)? else {
            return Ok(None);
        };
        let shift = u16_at(&header, 0x1E);
        if u16_at(&header, 0x1C) != 0xFFFE || !(shift == 9 || shift == 12) {
            return Ok(None);
        }
        let sector_size = 1u64 << shift;
        let file = CompoundFile {
            sector_size,
            header_difat: (0..109).map(|i| u32_at(&header, 0x4C + 4 * i)).collect(),
            first_difat_sector: u32_at(&header, 0x44),
            sectors: source.len / sector_size,
        };
        let Some(directory) = file.chain(source, u32_at(&header, 0x30))? else {
            return Ok(None);
        };
        let per_sector = usize_from_u64(sector_size) / ENTRY_SIZE;
        let mut entry = |id: u32| -> io::Result<Option<Vec<u8>>> {
            let Some(&sector) = directory.get(id as usize / per_sector) else {
                return Ok(None);
            };
            let offset = file.offset(sector) + ((id as usize % per_sector) * ENTRY_SIZE) as u64;
            source.read_at(offset, ENTRY_SIZE)
        };

        let Some(root) = entry(0)? else {
            return Ok(None);
        };
        if root[0x42] != ROOT {
            return Ok(None);
        }
        // The root's children are a tree of siblings; walk all of it, never into a child storage.
        let mut pending = vec![u32_at(&root, 0x4C)];
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = pending.pop() {
            if id == NOSTREAM || !seen.insert(id) || seen.len() > directory.len() * per_sector {
                continue;
            }
            let Some(node) = entry(id)? else {
                continue;
            };
            if node[0x42] == STREAM && name(&node).eq_ignore_ascii_case(WORD_DOCUMENT) {
                return Ok(Some(Format::Doc));
            }
            pending.push(u32_at(&node, 0x44));
            pending.push(u32_at(&node, 0x48));
        }
        Ok(None)
    }

    /// A directory entry's name: UTF-16LE, its length in bytes (the terminator included) at 0x40.
    fn name(entry: &[u8]) -> String {
        let length = (u16_at(entry, 0x40) as usize).clamp(2, 64) - 2;
        let units: Vec<u16> = entry[..length]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        String::from_utf16_lossy(&units)
    }

    impl CompoundFile {
        fn offset(&self, sector: u32) -> u64 {
            (u64::from(sector) + 1) * self.sector_size
        }

        /// The sectors of the directory chain starting at `first`, or `None` when it leaves the file,
        /// loops, runs past [`DIRECTORY_LIMIT`] or meets a FAT sector that cannot be read.
        fn chain<R: Read + Seek>(
            &self,
            source: &mut Source<'_, R>,
            first: u32,
        ) -> io::Result<Option<Vec<u32>>> {
            let mut sectors = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let mut sector = first;
            while sector < MAX_SECTOR {
                if sectors.len() >= DIRECTORY_LIMIT.min(usize_from_u64(self.sectors))
                    || !seen.insert(sector)
                {
                    return Ok(None);
                }
                sectors.push(sector);
                let Some(next) = self.next(source, sector)? else {
                    return Ok(None);
                };
                sector = next;
            }
            Ok((!sectors.is_empty()).then_some(sectors))
        }

        /// The FAT's entry for `sector`: the next sector of its chain.
        fn next<R: Read + Seek>(
            &self,
            source: &mut Source<'_, R>,
            sector: u32,
        ) -> io::Result<Option<u32>> {
            let per_sector = self.sector_size / 4;
            let Some(fat_sector) = self.fat_sector(source, u64::from(sector) / per_sector)? else {
                return Ok(None);
            };
            let offset = self.offset(fat_sector) + (u64::from(sector) % per_sector) * 4;
            Ok(source.read_at(offset, 4)?.map(|b| u32_at(&b, 0)))
        }

        /// The sector number of the FAT's `index`th sector: from the header's DIFAT, then the DIFAT chain.
        fn fat_sector<R: Read + Seek>(
            &self,
            source: &mut Source<'_, R>,
            index: u64,
        ) -> io::Result<Option<u32>> {
            if let Some(&sector) = self.header_difat.get(usize_from_u64(index)) {
                return Ok((sector < MAX_SECTOR).then_some(sector));
            }
            // Each DIFAT sector holds that many FAT sector numbers and, last, the next DIFAT sector.
            let per_sector = self.sector_size / 4 - 1;
            let mut remaining = index - self.header_difat.len() as u64;
            let mut difat = self.first_difat_sector;
            for _ in 0..self.sectors {
                if difat >= MAX_SECTOR {
                    return Ok(None);
                }
                if remaining < per_sector {
                    let offset = self.offset(difat) + remaining * 4;
                    return Ok(source
                        .read_at(offset, 4)?
                        .map(|b| u32_at(&b, 0))
                        .filter(|&s| s < MAX_SECTOR));
                }
                remaining -= per_sector;
                let Some(next) = source.read_at(self.offset(difat) + per_sector * 4, 4)? else {
                    return Ok(None);
                };
                difat = u32_at(&next, 0);
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sniffed(bytes: &[u8]) -> Option<Format> {
        sniff(&mut Cursor::new(bytes)).unwrap()
    }

    /// Deterministic noise: xorshift64.
    fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect()
    }

    /// Builders for test documents: just enough of each container to carry what the sniffer reads.
    mod build {
        /// A ZIP entry.
        pub struct Entry<'a> {
            pub name: &'a str,
            pub data: &'a [u8],
            pub deflate: bool,
            /// Sizes in a data descriptor after the data, zero in the local header.
            pub descriptor: bool,
        }

        pub fn stored<'a>(name: &'a str, data: &'a [u8]) -> Entry<'a> {
            Entry {
                name,
                data,
                deflate: false,
                descriptor: false,
            }
        }

        pub fn deflated<'a>(name: &'a str, data: &'a [u8]) -> Entry<'a> {
            Entry {
                name,
                data,
                deflate: true,
                descriptor: false,
            }
        }

        pub fn streamed<'a>(name: &'a str, data: &'a [u8]) -> Entry<'a> {
            Entry {
                name,
                data,
                deflate: true,
                descriptor: true,
            }
        }

        fn u16le(out: &mut Vec<u8>, v: u16) {
            out.extend_from_slice(&v.to_le_bytes());
        }

        fn u32le(out: &mut Vec<u8>, v: u32) {
            out.extend_from_slice(&v.to_le_bytes());
        }

        /// A ZIP archive of `entries`, central directory included. CRCs are zero: nothing reads them.
        pub fn zip(entries: &[Entry<'_>]) -> Vec<u8> {
            let mut out = Vec::new();
            let mut central = Vec::new();
            for entry in entries {
                let data = if entry.deflate {
                    miniz_oxide::deflate::compress_to_vec(entry.data, 6)
                } else {
                    entry.data.to_vec()
                };
                let flags = if entry.descriptor { 1 << 3 } else { 0 };
                let method = if entry.deflate { 8 } else { 0 };
                let offset = u32::try_from(out.len()).unwrap();
                let (compressed, size) = (
                    u32::try_from(data.len()).unwrap(),
                    u32::try_from(entry.data.len()).unwrap(),
                );

                out.extend_from_slice(b"PK\x03\x04");
                u16le(&mut out, 20);
                u16le(&mut out, flags);
                u16le(&mut out, method);
                u32le(&mut out, 0); // time, date
                u32le(&mut out, 0); // crc
                let (c, s) = if entry.descriptor {
                    (0, 0)
                } else {
                    (compressed, size)
                };
                u32le(&mut out, c);
                u32le(&mut out, s);
                u16le(&mut out, u16::try_from(entry.name.len()).unwrap());
                u16le(&mut out, 0);
                out.extend_from_slice(entry.name.as_bytes());
                out.extend_from_slice(&data);
                if entry.descriptor {
                    out.extend_from_slice(b"PK\x07\x08");
                    u32le(&mut out, 0);
                    u32le(&mut out, compressed);
                    u32le(&mut out, size);
                }

                central.extend_from_slice(b"PK\x01\x02");
                u16le(&mut central, 20);
                u16le(&mut central, 20);
                u16le(&mut central, flags);
                u16le(&mut central, method);
                u32le(&mut central, 0);
                u32le(&mut central, 0);
                u32le(&mut central, compressed);
                u32le(&mut central, size);
                u16le(&mut central, u16::try_from(entry.name.len()).unwrap());
                u16le(&mut central, 0);
                u16le(&mut central, 0);
                u16le(&mut central, 0);
                u16le(&mut central, 0);
                u32le(&mut central, 0);
                u32le(&mut central, offset);
                central.extend_from_slice(entry.name.as_bytes());
            }
            let directory_offset = u32::try_from(out.len()).unwrap();
            out.extend_from_slice(&central);
            out.extend_from_slice(b"PK\x05\x06");
            u16le(&mut out, 0);
            u16le(&mut out, 0);
            u16le(&mut out, u16::try_from(entries.len()).unwrap());
            u16le(&mut out, u16::try_from(entries.len()).unwrap());
            u32le(&mut out, u32::try_from(central.len()).unwrap());
            u32le(&mut out, directory_offset);
            u16le(&mut out, 0);
            out
        }

        /// A directory entry of a compound file: a storage with children, or a stream.
        pub struct Node {
            pub name: &'static str,
            pub storage: bool,
            pub children: Vec<Self>,
        }

        pub fn stream(name: &'static str) -> Node {
            Node {
                name,
                storage: false,
                children: Vec::new(),
            }
        }

        pub fn storage(name: &'static str, children: Vec<Node>) -> Node {
            Node {
                name,
                storage: true,
                children,
            }
        }

        const NOSTREAM: u32 = 0xFFFF_FFFF;

        /// A compound file with sectors of `1 << shift` bytes: sector 0 is the FAT, the directory follows
        /// in as many sectors as it needs. Siblings hang off each other's right pointer.
        #[allow(
            clippy::needless_pass_by_value,
            reason = "a test helper that takes what its callers build in place"
        )]
        pub fn cfb(shift: u16, root_children: Vec<Node>) -> Vec<u8> {
            // Flatten: (name, type, right sibling, child).
            let mut entries: Vec<(String, u8, u32, u32)> =
                vec![("Root Entry".into(), 5, NOSTREAM, NOSTREAM)];
            fn add(entries: &mut Vec<(String, u8, u32, u32)>, nodes: &[Node]) -> u32 {
                let mut first = NOSTREAM;
                let mut previous: Option<usize> = None;
                for node in nodes {
                    let id = entries.len();
                    entries.push((
                        node.name.into(),
                        if node.storage { 1 } else { 2 },
                        NOSTREAM,
                        NOSTREAM,
                    ));
                    let child = add(entries, &node.children);
                    entries[id].3 = child;
                    match previous {
                        Some(p) => entries[p].2 = u32::try_from(id).unwrap(),
                        None => first = u32::try_from(id).unwrap(),
                    }
                    previous = Some(id);
                }
                first
            }
            let child = add(&mut entries, &root_children);
            entries[0].3 = child;

            let size = 1usize << shift;
            let per_sector = size / 128;
            let directory_sectors = entries.len().div_ceil(per_sector);
            let mut out = vec![0u8; size * (1 + 1 + directory_sectors)];

            out[..8].copy_from_slice(&super::OLE_SIGNATURE);
            out[0x18..0x1A].copy_from_slice(&0x3Eu16.to_le_bytes());
            out[0x1A..0x1C].copy_from_slice(&(if shift == 9 { 3u16 } else { 4 }).to_le_bytes());
            out[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
            out[0x1E..0x20].copy_from_slice(&shift.to_le_bytes());
            out[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
            out[0x2C..0x30].copy_from_slice(&1u32.to_le_bytes());
            out[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
            out[0x38..0x3C].copy_from_slice(&4096u32.to_le_bytes());
            out[0x3C..0x40].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
            out[0x44..0x48].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
            for i in 0..109 {
                let v: u32 = if i == 0 { 0 } else { NOSTREAM };
                out[0x4C + 4 * i..0x50 + 4 * i].copy_from_slice(&v.to_le_bytes());
            }

            // The FAT, in sector 0: itself, then the directory's chain, the rest free.
            let fat = size;
            let set = |out: &mut Vec<u8>, i: usize, v: u32| {
                out[fat + 4 * i..fat + 4 * i + 4].copy_from_slice(&v.to_le_bytes());
            };
            for i in 0..size / 4 {
                set(&mut out, i, NOSTREAM);
            }
            set(&mut out, 0, 0xFFFF_FFFD);
            for s in 1..=directory_sectors {
                let next = if s == directory_sectors {
                    0xFFFF_FFFE
                } else {
                    u32::try_from(s).unwrap() + 1
                };
                set(&mut out, s, next);
            }

            for (id, (name, kind, right, child)) in entries.iter().enumerate() {
                let at = size * 2 + id * 128;
                let units: Vec<u16> = name.encode_utf16().collect();
                for (i, unit) in units.iter().enumerate() {
                    out[at + 2 * i..at + 2 * i + 2].copy_from_slice(&unit.to_le_bytes());
                }
                out[at + 0x40..at + 0x42]
                    .copy_from_slice(&u16::try_from((units.len() + 1) * 2).unwrap().to_le_bytes());
                out[at + 0x42] = *kind;
                out[at + 0x43] = 1;
                out[at + 0x44..at + 0x48].copy_from_slice(&NOSTREAM.to_le_bytes());
                out[at + 0x48..at + 0x4C].copy_from_slice(&right.to_le_bytes());
                out[at + 0x4C..at + 0x50].copy_from_slice(&child.to_le_bytes());
            }
            for id in entries.len()..directory_sectors * per_sector {
                let at = size * 2 + id * 128;
                for pointer in [0x44, 0x48, 0x4C] {
                    out[at + pointer..at + pointer + 4].copy_from_slice(&NOSTREAM.to_le_bytes());
                }
            }
            out
        }
    }

    use build::*;

    const DOCX_CT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

    fn content_types(main: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/main.xml" ContentType="{main}"/></Types>"#
        )
    }

    const XLSX_MAIN: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
    const PPTX_MAIN: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml";
    const DOCM_MAIN: &str = "application/vnd.ms-word.document.macroEnabled.main+xml";
    const DOTX_MAIN: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.template.main+xml";
    const DOTM_MAIN: &str = "application/vnd.ms-word.template.macroEnabledTemplate.main+xml";
    const XLSM_MAIN: &str = "application/vnd.ms-excel.sheet.macroEnabled.main+xml";
    const XLTX_MAIN: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml";
    const POTX_MAIN: &str =
        "application/vnd.openxmlformats-officedocument.presentationml.template.main+xml";

    const DOCUMENT_XML: &[u8] = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>hello</w:t></w:r></w:p></w:body></w:document>"#;

    fn docx() -> Vec<u8> {
        zip(&[
            deflated("[Content_Types].xml", DOCX_CT.as_bytes()),
            deflated("word/document.xml", DOCUMENT_XML),
        ])
    }

    fn odf(mimetype: &str) -> Vec<u8> {
        zip(&[
            stored("mimetype", mimetype.as_bytes()),
            deflated("content.xml", b"<office:document-content/>"),
        ])
    }

    fn doc() -> Vec<u8> {
        cfb(
            9,
            vec![
                stream("1Table"),
                stream("WordDocument"),
                stream("\u{5}SummaryInformation"),
            ],
        )
    }

    // RTF

    #[test]
    fn rtf_is_recognised_by_its_signature_alone() {
        assert_eq!(sniffed(b"{\\rtf1\\ansi hello}"), Some(Format::Rtf));
        assert_eq!(sniffed(b"{\\rtf"), Some(Format::Rtf), "cut short: damaged");
        assert_eq!(
            sniffed(b"{\\rtf1 \x00\xFF garbage"),
            Some(Format::Rtf),
            "damaged"
        );
    }

    /// moalhaddar/docx-to-pdf does not convert an RTF behind a UTF-8 byte-order mark: it answers 200 with the RTF's
    /// source typeset as plain text, not a real rendering, so the allowlist does not admit it.
    #[test]
    fn rtf_after_a_utf8_byte_order_mark_is_refused() {
        assert_eq!(sniffed(b"\xEF\xBB\xBF{\\rtf1\\ansi hello}"), None);
        assert_eq!(sniffed(b"\xEF\xBB\xBF{\\rtf"), None, "cut short");
    }

    #[test]
    fn text_that_is_not_rtf_is_refused() {
        for text in [
            &b"hello"[..],
            b"{\\rt",
            b"{\\RTF1}",
            b" {\\rtf1}",
            b"\xEF\xBB\xBF",
            b"\xEF\xBB\xBFhello, not rtf",
            b"\xEF\xBB\xBF {\\rtf1}",
            b"\xEF\xBB\xBF\xEF\xBB\xBF{\\rtf1}",
            b"\xFF\xFE{\0\\\0r\0t\0f\0",
            b"<html>{\\rtf1}</html>",
            b"%PDF-1.7",
        ] {
            assert_eq!(sniffed(text), None, "{:?}", String::from_utf8_lossy(text));
        }
    }

    #[test]
    fn an_empty_document_is_refused() {
        assert_eq!(sniffed(b""), None);
    }

    #[test]
    fn random_bytes_are_refused_with_or_without_a_containers_signature() {
        for seed in 0..200u64 {
            let bytes = noise(seed, 1 + (usize_from_u64(seed) * 37) % 5000);
            for prefix in [&b""[..], ZIP_LOCAL_HEADER, &OLE_SIGNATURE] {
                let mut input = prefix.to_vec();
                input.extend_from_slice(&bytes);
                assert_eq!(sniffed(&input), None, "seed {seed}, prefix {prefix:?}");
            }
        }
    }

    // DOCX

    #[test]
    fn docx_is_recognised_by_its_content_types() {
        assert_eq!(sniffed(&docx()), Some(Format::Docx));
        assert_eq!(
            sniffed(&zip(&[stored("[Content_Types].xml", DOCX_CT.as_bytes())])),
            Some(Format::Docx),
            "stored"
        );
        assert_eq!(
            sniffed(&zip(&[
                deflated("word/document.xml", DOCUMENT_XML),
                deflated("[content_types].XML", DOCX_CT.as_bytes()),
            ])),
            Some(Format::Docx),
            "part names are case-insensitive, and the entry need not be first"
        );
    }

    #[test]
    fn docx_whose_main_part_comes_by_a_default_is_recognised() {
        let ct = r#"<Types><Default Extension="xml" ContentType='application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'/></Types>"#;
        assert_eq!(
            sniffed(&zip(&[deflated("[Content_Types].xml", ct.as_bytes())])),
            Some(Format::Docx)
        );
    }

    #[test]
    fn docx_with_data_descriptors_is_recognised() {
        // Java's ZipOutputStream (Apache POI, docx4j) writes sizes after the data.
        let archive = zip(&[
            streamed("word/document.xml", DOCUMENT_XML),
            streamed("[Content_Types].xml", DOCX_CT.as_bytes()),
        ]);
        assert_eq!(sniffed(&archive), Some(Format::Docx));
    }

    #[test]
    fn upstreams_examples_are_docx() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("upstream/docx-to-pdf/examples");
        for example in ["ex1", "ex2", "ex3", "ex4"] {
            let path = root.join(example).join("input.docx");
            let Ok(bytes) = std::fs::read(&path) else {
                panic!("{} missing: is the submodule checked out?", path.display());
            };
            assert_eq!(sniffed(&bytes), Some(Format::Docx), "{example}");
        }
    }

    #[test]
    fn a_damaged_docx_still_passes() {
        // The identifying entry is intact; the body is garbage, missing, or the archive is cut short.
        let garbage = noise(7, 3000);
        assert_eq!(
            sniffed(&zip(&[
                deflated("[Content_Types].xml", DOCX_CT.as_bytes()),
                stored("word/document.xml", &garbage),
            ])),
            Some(Format::Docx),
            "corrupt document.xml"
        );
        assert_eq!(
            sniffed(&zip(&[deflated("[Content_Types].xml", DOCX_CT.as_bytes())])),
            Some(Format::Docx),
            "no document.xml at all"
        );
        let whole = zip(&[
            deflated("[Content_Types].xml", DOCX_CT.as_bytes()),
            deflated("word/document.xml", &noise(9, 20_000)),
        ]);
        for cut in [whole.len() - 10, whole.len() / 2, 300] {
            assert_eq!(
                sniffed(&whole[..cut]),
                Some(Format::Docx),
                "truncated to {cut} of {}",
                whole.len()
            );
        }
    }

    #[test]
    fn a_truncated_docx_with_data_descriptors_still_passes() {
        let whole = zip(&[
            streamed("_rels/.rels", b"<Relationships/>"),
            streamed("[Content_Types].xml", DOCX_CT.as_bytes()),
            streamed("word/document.xml", &noise(3, 20_000)),
        ]);
        assert_eq!(sniffed(&whole[..whole.len() / 2]), Some(Format::Docx));
    }

    #[test]
    fn a_docx_whose_identity_is_damaged_is_refused() {
        let whole = docx();
        // Cut inside the content-types entry: what identifies it is gone.
        assert_eq!(sniffed(&whole[..60]), None);
        // The content types corrupted in place.
        let mut corrupt = zip(&[stored("[Content_Types].xml", DOCX_CT.as_bytes())]);
        let at = corrupt
            .windows(10)
            .position(|w| w == b"wordproces")
            .unwrap();
        corrupt[at] = b'X';
        assert_eq!(sniffed(&corrupt), None);
        // A deflated entry whose stream is garbage.
        let mut broken = docx();
        for byte in &mut broken[49..70] {
            *byte ^= 0x5A;
        }
        assert_eq!(sniffed(&broken), None);
    }

    #[test]
    fn other_ooxml_formats_are_refused() {
        for (main, what) in [
            (XLSX_MAIN, "xlsx"),
            (PPTX_MAIN, "pptx"),
            (XLSM_MAIN, "xlsm: a macro-enabled spreadsheet, next to docm"),
            (XLTX_MAIN, "xltx: a spreadsheet template, next to dotx"),
            (DOTM_MAIN, "dotm: not among the admitted variants"),
            (POTX_MAIN, "potx: a presentation template"),
        ] {
            let archive = zip(&[deflated(
                "[Content_Types].xml",
                content_types(main).as_bytes(),
            )]);
            assert_eq!(sniffed(&archive), None, "{what}");
        }
    }

    #[test]
    fn docm_and_dotx_are_recognised_by_their_content_types() {
        for (main, format) in [(DOCM_MAIN, Format::Docm), (DOTX_MAIN, Format::Dotx)] {
            let archive = zip(&[
                deflated("[Content_Types].xml", content_types(main).as_bytes()),
                deflated("word/document.xml", DOCUMENT_XML),
            ]);
            assert_eq!(sniffed(&archive), Some(format), "{main}");
            assert_eq!(
                sniffed(&archive[..archive.len() - 30]),
                Some(format),
                "truncated: {main}"
            );
        }
        let docm = content_types(DOCM_MAIN).replace(
            "<Default Extension=\"xml\"",
            "<Default Extension=\"bin\" ContentType=\"application/vnd.ms-office.vbaProject\"/><Default Extension=\"xml\"",
        );
        assert_eq!(
            sniffed(&zip(&[deflated("[Content_Types].xml", docm.as_bytes())])),
            Some(Format::Docm),
            "with its VBA project's type declared"
        );
    }

    #[test]
    fn a_zip_without_content_types_is_refused() {
        assert_eq!(
            sniffed(&zip(&[deflated("word/document.xml", DOCUMENT_XML)])),
            None,
            "a docx's parts without [Content_Types].xml"
        );
        assert_eq!(
            sniffed(&zip(&[stored("Content_Types.xml", DOCX_CT.as_bytes())])),
            None,
            "a near miss of a name"
        );
        assert_eq!(sniffed(&zip(&[])), None, "an empty archive");
    }

    // ODT

    #[test]
    fn odt_is_recognised_by_its_mimetype() {
        assert_eq!(
            sniffed(&odf("application/vnd.oasis.opendocument.text")),
            Some(Format::Odt)
        );
        let damaged = zip(&[
            stored("mimetype", b"application/vnd.oasis.opendocument.text"),
            stored("content.xml", &noise(5, 2000)),
        ]);
        assert_eq!(sniffed(&damaged), Some(Format::Odt), "damaged content");
        assert_eq!(
            sniffed(&damaged[..damaged.len() - 30]),
            Some(Format::Odt),
            "truncated"
        );
    }

    #[test]
    fn ott_is_recognised_by_its_mimetype() {
        assert_eq!(
            sniffed(&odf("application/vnd.oasis.opendocument.text-template")),
            Some(Format::Ott)
        );
    }

    // Flat ODT

    fn flat(prolog: &str, root: &str) -> Vec<u8> {
        format!(
            "{prolog}<{root} xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\"><office:body><office:text/></office:body></office:document>"
        )
        .into_bytes()
    }

    const FLAT_ODT: &str = r#"office:document office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.text""#;

    /// What moalhaddar/docx-to-pdf converts as a flat ODT: an XML declaration first (after a UTF-8
    /// byte-order mark at most), and the root's double-quoted `office:mimetype` ending within 4000 bytes.
    #[test]
    fn flat_odt_is_recognised_by_its_root_elements_mimetype() {
        const DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";
        for (prolog, what) in [
            (DECLARATION.to_owned(), "with a declaration"),
            (format!("\u{FEFF}{DECLARATION}"), "after a byte-order mark"),
            (
                format!("{DECLARATION}<!-- a > in a comment -->\n"),
                "after a comment",
            ),
            (
                "<?xml version='1.0'?>".to_owned(),
                "a single-quoted declaration",
            ),
        ] {
            assert_eq!(
                sniffed(&flat(&prolog, FLAT_ODT)),
                Some(Format::Fodt),
                "{what}"
            );
        }
        let mut damaged = flat(DECLARATION, FLAT_ODT);
        damaged.truncate(damaged.len() - 40);
        damaged.extend_from_slice(&noise(9, 500));
        assert_eq!(sniffed(&damaged), Some(Format::Fodt), "a damaged body");
        assert_eq!(
            sniffed(&mimetype_ending_at(FLAT_MIMETYPE_WINDOW)),
            Some(Format::Fodt),
            "the mimetype ending at the window's last byte"
        );
    }

    /// A flat ODT whose root's `office:mimetype` value, its closing quote included, takes the file's
    /// first `end` bytes, padded by an attribute before it.
    fn mimetype_ending_at(end: usize) -> Vec<u8> {
        let head = "<?xml version=\"1.0\"?><office:document pad=\"";
        let tail = "\" office:mimetype=\"application/vnd.oasis.opendocument.text\"";
        let pad = "x".repeat(end - head.len() - tail.len());
        format!("{head}{pad}{tail}><office:body/></office:document>").into_bytes()
    }

    #[test]
    fn other_flat_xml_is_refused() {
        for (root, what) in [
            (
                r#"office:document office:mimetype="application/vnd.oasis.opendocument.spreadsheet""#,
                "flat ods",
            ),
            (
                r#"office:document office:mimetype="application/vnd.oasis.opendocument.presentation""#,
                "flat odp",
            ),
            (
                r#"office:document office:mimetype="application/vnd.oasis.opendocument.text-template""#,
                "flat ott",
            ),
            (r#"office:document office:version="1.3""#, "no mimetype"),
            (
                r#"office:document-content office:mimetype="application/vnd.oasis.opendocument.text""#,
                "another root element",
            ),
            (
                r#"office:document data-office:mimetype="application/vnd.oasis.opendocument.text""#,
                "a near miss of the attribute's name",
            ),
            (
                r#"office:document title='office:mimetype="application/vnd.oasis.opendocument.text"'"#,
                "the attribute inside another value",
            ),
        ] {
            assert_eq!(
                sniffed(&flat("<?xml version=\"1.0\"?>", root)),
                None,
                "{what}"
            );
        }
        assert_eq!(sniffed(&flat("", FLAT_ODT)), None, "no XML declaration");
        assert_eq!(
            sniffed(&flat(" <?xml version=\"1.0\"?>", FLAT_ODT)),
            None,
            "space before the declaration"
        );
        assert_eq!(
            sniffed(&flat(
                "<?xml version=\"1.0\"?>",
                "office:document office:mimetype='application/vnd.oasis.opendocument.text'"
            )),
            None,
            "a single-quoted mimetype"
        );
        assert_eq!(
            sniffed(&mimetype_ending_at(FLAT_MIMETYPE_WINDOW + 1)),
            None,
            "the mimetype ending past the window"
        );
        assert_eq!(
            sniffed(br#"<p>office:document office:mimetype="application/vnd.oasis.opendocument.text"</p>"#),
            None,
            "the words in text"
        );
        assert_eq!(
            sniffed(b"hello <office:document office:mimetype=\"application/vnd.oasis.opendocument.text\">"),
            None,
            "text before the root element"
        );
    }

    #[test]
    fn other_odf_formats_are_refused() {
        for mimetype in [
            "application/vnd.oasis.opendocument.spreadsheet",
            "application/vnd.oasis.opendocument.presentation",
            "application/vnd.oasis.opendocument.spreadsheet-template",
            "application/vnd.oasis.opendocument.text-master",
            "application/vnd.oasis.opendocument.text-flat-xml",
            "",
        ] {
            assert_eq!(sniffed(&odf(mimetype)), None, "{mimetype:?}");
        }
        assert_eq!(
            sniffed(&zip(&[stored(
                "MIMETYPE",
                b"application/vnd.oasis.opendocument.text"
            )])),
            None,
            "ODF names the entry exactly"
        );
    }

    // DOC

    #[test]
    fn doc_is_recognised_by_its_word_document_stream() {
        assert_eq!(sniffed(&doc()), Some(Format::Doc));
        assert_eq!(
            sniffed(&cfb(12, vec![stream("WordDocument"), stream("1Table")])),
            Some(Format::Doc),
            "4096-byte sectors"
        );
        assert_eq!(
            sniffed(&cfb(9, vec![stream("worddocument")])),
            Some(Format::Doc),
            "compound file names compare case-insensitively"
        );
    }

    #[test]
    fn doc_whose_directory_spans_sectors_is_recognised() {
        let mut children: Vec<Node> = [
            "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M",
        ]
        .into_iter()
        .map(stream)
        .collect();
        children.push(stream("WordDocument"));
        let file = cfb(9, children);
        assert!(file.len() > 512 * 5, "the directory takes several sectors");
        assert_eq!(sniffed(&file), Some(Format::Doc));
    }

    #[test]
    fn a_doc_with_a_damaged_body_still_passes() {
        // The streams' content is not read: a file whose sectors after the directory are garbage passes.
        let mut file = doc();
        file.extend_from_slice(&noise(11, 4096));
        assert_eq!(sniffed(&file), Some(Format::Doc));
    }

    #[test]
    fn other_compound_files_are_refused() {
        for (children, what) in [
            (
                vec![stream("Workbook"), stream("\u{5}SummaryInformation")],
                "xls",
            ),
            (vec![stream("Book")], "xls 5.0"),
            (
                vec![stream("PowerPoint Document"), stream("Current User")],
                "ppt",
            ),
            (
                vec![
                    stream("Workbook"),
                    storage("MBD0001", vec![stream("WordDocument")]),
                ],
                "an xls with an embedded Word object",
            ),
            (
                vec![storage("WordDocument", vec![])],
                "a storage, not a stream",
            ),
            (vec![stream("EncryptedPackage")], "an encrypted OOXML"),
            (vec![], "empty"),
        ] {
            assert_eq!(sniffed(&cfb(9, children)), None, "{what}");
        }
    }

    #[test]
    fn a_compound_file_whose_directory_is_damaged_is_refused_and_does_not_hang() {
        // The directory chain loops onto itself.
        let mut looping = doc();
        looping[512 + 4..512 + 8].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(sniffed(&looping), None);
        // Cut before the directory.
        let whole = doc();
        assert_eq!(sniffed(&whole[..700]), None);
        // A bad sector size.
        let mut shift = doc();
        shift[0x1E] = 7;
        assert_eq!(sniffed(&shift), None);
        // A sibling pointer onto itself.
        let mut tree = cfb(9, vec![stream("1Table")]);
        let at = 1024 + 128 + 0x44;
        tree[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(sniffed(&tree), None);
    }

    // Names and Content-Type play no part: sniff sees only bytes. The file-level entry point:

    #[tokio::test]
    async fn a_spooled_file_is_sniffed_from_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document");
        std::fs::write(&path, doc()).unwrap();
        assert_eq!(sniff_file(&path).await.unwrap(), Some(Format::Doc));
        std::fs::write(&path, b"plain text").unwrap();
        assert_eq!(sniff_file(&path).await.unwrap(), None);
        assert!(sniff_file(&directory.path().join("missing")).await.is_err());
    }

    // Inflation

    const MIB: u64 = 1024 * 1024;

    fn inflation_of(bytes: &[u8], limit: u64) -> Inflation {
        inflation(&mut Cursor::new(bytes), limit).unwrap()
    }

    /// A DOCX whose `word/document.xml` is `size` bytes of one repeated paragraph: the pages bomb (7.4 MiB
    /// that wrote 1.5 GiB to `/tmp`) in small.
    fn pages_bomb(size: usize) -> Vec<u8> {
        let xml = DOCUMENT_XML.repeat(size / DOCUMENT_XML.len() + 1);
        zip(&[
            deflated("[Content_Types].xml", DOCX_CT.as_bytes()),
            deflated("word/document.xml", &xml[..size]),
        ])
    }

    /// Rewrites every uncompressed size the archive declares, in the directory and in the local headers, to 1.
    fn understated(mut bytes: Vec<u8>) -> Vec<u8> {
        for (signature, field) in [(b"PK\x01\x02", 24), (b"PK\x03\x04", 22)] {
            let at: Vec<usize> = (0..bytes.len() - 4)
                .filter(|&i| &bytes[i..i + 4] == signature)
                .collect();
            for i in at {
                bytes[i + field..i + field + 4].copy_from_slice(&1u32.to_le_bytes());
            }
        }
        bytes
    }

    #[test]
    fn a_docx_within_the_limit_counts_what_its_parts_inflate_to() {
        let total = (DOCX_CT.len() + DOCUMENT_XML.len()) as u64;
        assert_eq!(inflation_of(&docx(), MIB), Inflation::Within(total));
        assert_eq!(
            inflation_of(&docx(), total),
            Inflation::Within(total),
            "at the limit"
        );
        let odt = odf("application/vnd.oasis.opendocument.text");
        assert!(matches!(inflation_of(&odt, MIB), Inflation::Within(n) if n > 39));
        let stored_part = zip(&[
            stored("[Content_Types].xml", DOCX_CT.as_bytes()),
            stored("word/media/image1.png", &noise(3, 5000)),
        ]);
        assert_eq!(
            inflation_of(&stored_part, MIB),
            Inflation::Within(DOCX_CT.len() as u64 + 5000),
            "a stored part counts its bytes"
        );
    }

    #[test]
    fn a_bomb_whose_directory_declares_past_the_limit_is_past_without_inflating() {
        let bomb = pages_bomb(4 * usize_from_u64(MIB));
        assert!(bomb.len() < 64 * 1024, "{} bytes", bomb.len());
        assert_eq!(
            inflation_of(&bomb, MIB),
            Inflation::DeclaredPast(DOCX_CT.len() as u64 + 4 * MIB)
        );
        assert!(inflation_of(&bomb, MIB).is_past());
        assert_eq!(
            inflation_of(&bomb, 5 * MIB),
            Inflation::Within(DOCX_CT.len() as u64 + 4 * MIB),
            "a higher limit lets it through"
        );
    }

    #[test]
    fn a_directory_that_understates_its_sizes_gains_nothing() {
        let bomb = understated(pages_bomb(4 * usize_from_u64(MIB)));
        assert_eq!(sniffed(&bomb), Some(Format::Docx));
        let Inflation::InflatedPast(counted) = inflation_of(&bomb, MIB) else {
            panic!("{:?}", inflation_of(&bomb, MIB));
        };
        // Counting stops within one output chunk past the limit, not at what the part inflates to.
        assert!(counted > MIB && counted <= MIB + 64 * 1024, "{counted}");
    }

    #[test]
    fn a_bomb_without_a_directory_is_counted_from_its_local_headers() {
        // Sizes in data descriptors, and the archive cut before its central directory.
        let whole = zip(&[
            streamed("[Content_Types].xml", DOCX_CT.as_bytes()),
            streamed("word/document.xml", &b"<w:p/>".repeat(400_000)),
        ]);
        let directory = whole.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        let cut = &whole[..directory];
        assert_eq!(sniffed(cut), Some(Format::Docx));
        assert!(matches!(inflation_of(cut, MIB), Inflation::InflatedPast(_)));
        assert_eq!(
            inflation_of(cut, 10 * MIB),
            Inflation::Within((DOCX_CT.len() + 2_400_000) as u64)
        );
    }

    #[test]
    fn a_damaged_part_counts_what_inflated_before_the_damage() {
        let mut bytes = pages_bomb(200_000);
        let directory = bytes.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        // Garbage over the second half of document.xml's deflate stream.
        let garbage = noise(11, 2000);
        let at = directory - 300;
        bytes[at..directory].copy_from_slice(&garbage[..300]);
        assert!(matches!(inflation_of(&bytes, MIB), Inflation::Within(n) if n < 200_000));
    }

    #[test]
    fn only_the_allowlisted_zip_formats_are_counted() {
        let xlsx_bomb = zip(&[
            deflated("[Content_Types].xml", content_types(XLSX_MAIN).as_bytes()),
            deflated(
                "xl/worksheets/sheet1.xml",
                &vec![b'x'; 4 * usize_from_u64(MIB)],
            ),
        ]);
        assert_eq!(
            inflation_of(&xlsx_bomb, MIB),
            Inflation::NotCounted,
            "the allowlist refuses it: 415"
        );
        for other in [
            doc(),
            b"{\\rtf1 hello}".to_vec(),
            b"plain text".to_vec(),
            Vec::new(),
        ] {
            assert_eq!(inflation_of(&other, 1), Inflation::NotCounted);
        }
    }

    #[tokio::test]
    async fn a_spooled_file_is_counted_from_disk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document");
        std::fs::write(&path, pages_bomb(2 * usize_from_u64(MIB))).unwrap();
        assert!(inflation_file(&path, MIB).await.unwrap().is_past());
        assert!(!inflation_file(&path, 3 * MIB).await.unwrap().is_past());
        assert!(
            inflation_file(&directory.path().join("missing"), MIB)
                .await
                .is_err()
        );
    }

    #[test]
    fn formats_have_their_telemetry_names() {
        assert_eq!(
            [Format::Docx, Format::Doc, Format::Odt, Format::Rtf].map(Format::name),
            ["docx", "doc", "odt", "rtf"]
        );
        let variants = [Format::Docm, Format::Dotx, Format::Ott, Format::Fodt];
        assert_eq!(variants.map(Format::name), ["docm", "dotx", "ott", "fodt"]);
        assert_eq!(
            variants.map(Format::extension),
            ["docm", "dotx", "ott", "fodt"]
        );
    }
}
