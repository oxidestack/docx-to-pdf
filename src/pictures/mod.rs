//! Pictures too large for LibreOffice, converted rather than lost: a silent loss is not ported, and a refusal would
//! leave the client with nothing, so the document converts with the picture made smaller or replaced, and the answer
//! says what changed. LibreOffice's headless bitmap allocator refuses a
//! bitmap whose `height × 4-byte-aligned scanline` passes [`LIBREOFFICE_MAX_BITMAP`] (`SAL_MAX_INT32 / 2`,
//! `vcl/headless/svpbmp.cxx`), and the PNG or JPEG reader then drops the picture without a word: the PDF comes out
//! with its frame empty. The scanline follows the pixel format LibreOffice 26.8 decodes to: 8 bits a pixel for
//! grayscale, 24 for RGB or a palette, 32 for any alpha (an alpha channel or a `tRNS` chunk).
//!
//! [`header`] reads that from a picture's first bytes (a PNG's `IHDR` and the chunks before `IDAT`, a JPEG's markers up
//! to `SOFn`), so the service sees the drop coming before any instance does. A PNG past the threshold is downscaled
//! ([`downscale`]), and so is a JPEG ([`mod@jpeg`]); one that cannot be is replaced by a
//! placeholder.

pub mod downscale;
pub mod jpeg;

use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use axum::http::{HeaderName, HeaderValue};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};

use crate::problem::Problem;

/// The largest bitmap, in bytes, LibreOffice's headless backend allocates: `SAL_MAX_INT32 / 2`.
pub const LIBREOFFICE_MAX_BITMAP: u64 = (i32::MAX / 2) as u64;

/// A picture format whose size [`header`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// PNG.
    Png,
    /// JPEG (baseline or progressive).
    Jpeg,
}

/// What a picture's header says about the bitmap LibreOffice would decode it into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// The format.
    pub kind: Kind,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits a pixel of LibreOffice 26.8's bitmap: 8, 24 or 32.
    pub bits: u8,
}

impl Header {
    /// The bytes of LibreOffice's bitmap: `height × 4-byte-aligned scanline`, `u64::MAX` past it (a PNG can claim
    /// 2³² - 1 a side).
    pub fn bitmap(&self) -> u64 {
        let scanline = (u64::from(self.width) * u64::from(self.bits)).div_ceil(32) * 4;
        u64::from(self.height).saturating_mul(scanline)
    }
}

/// The header of the PNG or JPEG that `head` starts, or `None` when it is neither or is cut off before its size.
pub fn header(head: &[u8]) -> Option<Header> {
    if head.starts_with(PNG_SIGNATURE) {
        png(head)
    } else if head.starts_with(&[0xFF, 0xD8]) {
        jpeg(head)
    } else {
        None
    }
}

/// A picture [`replace_oversized`] changed: its entry's name, what its header said and what was done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    /// The entry's name in the ZIP.
    pub entry: String,
    /// What its header said.
    pub header: Header,
    /// What was done to it.
    pub outcome: Outcome,
}

/// What [`replace_oversized`] did to a picture past the threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Downscaled by a power of two, same format, the fastest reduction.
    Downscaled(downscale::Downscaled),
    /// Replaced by its format's placeholder, and why it was not downscaled.
    Replaced {
        /// Why it was not downscaled.
        why: String,
    },
}

impl Outcome {
    /// `downscaled` or `replaced`: the `outcome` of `docx_to_pdf.pictures` and of the log line.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Downscaled(_) => "downscaled",
            Self::Replaced { .. } => "replaced",
        }
    }
}

/// `Docx-To-Pdf-Pictures`: on a converted document whose pictures the service changed, how many it downscaled and
/// how many it replaced (`downscaled=0, replaced=1`). Absent when it changed none.
pub const HEADER: HeaderName = HeaderName::from_static("docx-to-pdf-pictures");

/// What an instance holds besides its pictures' bitmaps, kept out of the picture budget: 86-99 MiB peaks were
/// measured for a small document with one small picture, and an RSS 142 MB above eight 256 MB bitmaps when the hard
/// limit killed the instance.
pub const BUDGET_RESERVE: u64 = 192 * 1024 * 1024;

/// What LibreOffice holds for a picture, against its bitmap: 8- and 24-bit pictures peak at about their bitmap plus
/// the instance's own memory, pictures with alpha at 2.25-2.4 times it (measured on LibreOffice 26.8: RGBA 8 192²,
/// 256 MiB, peaks at 665 MiB; RGBA 16 383², 1 GiB, at 2.4 GiB). An alpha picture is counted at 5/2 of its bitmap.
pub fn held(header: &Header) -> u64 {
    let bitmap = header.bitmap();
    if header.bits == 32 {
        bitmap.saturating_mul(5) / 2
    } else {
        bitmap
    }
}

/// The pictures step before conversion: its threshold and `docx_to_pdf.pictures`; the budget the document's
/// pictures must fit in, and how many documents it downscales at once, so a many-picture bomb is refused before a
/// minute of downscaling rather than after.
#[derive(Clone)]
pub struct Pictures {
    max_bitmap: u64,
    count: Counter<u64>,
    downscale: Downscale,
    fault: Fault,
    hard_memory: u64,
    permits: std::sync::Arc<tokio::sync::Semaphore>,
    wait: std::time::Duration,
}

/// A stage of the pictures step, where [`Pictures::with_fault`] can make it panic.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Before the ZIP's directory is listed.
    Listing,
    /// Before each entry's header is read.
    Header,
    /// After the rewrite has written its first entry.
    Rewrite,
    /// After the rewrite, before the answer header is made and the rewritten document takes the received one's place.
    Summary,
}

/// Called at each [`Stage`]: a test's way to make the step panic there. The service's does nothing.
#[doc(hidden)]
pub type Fault = fn(Stage);

/// The service's [`Fault`]: none.
#[doc(hidden)]
pub fn no_fault(_: Stage) {}

impl Pictures {
    /// Pictures whose bitmap passes `max_bitmap` are changed; `docx_to_pdf.pictures` is registered on `meter`.
    pub fn new(meter: &Meter, max_bitmap: u64) -> Self {
        Self {
            max_bitmap,
            count: meter
                .u64_counter("docx_to_pdf.pictures")
                .with_description(
                    "Pictures changed before conversion because LibreOffice would drop them or they pass the \
                     bitmap threshold, by what was done",
                )
                .build(),
            downscale: downscale_picture,
            fault: no_fault,
            hard_memory: crate::config::DEFAULT_INSTANCE_HARD_MEMORY,
            permits: std::sync::Arc::new(tokio::sync::Semaphore::new(
                crate::config::DEFAULT_POOL_SIZE,
            )),
            wait: crate::config::DEFAULT_POOL_WAIT_TIMEOUT,
        }
    }

    /// The same step with the budget derived from `hard_memory` (`DOCX_TO_PDF_INSTANCE_HARD_MEMORY`): a
    /// document whose pictures, as LibreOffice will hold them after the planned downscale ([`held`]), sum past
    /// `hard_memory` less [`BUDGET_RESERVE`] is refused 413 before any picture is decoded. 0 = no budget.
    #[must_use]
    pub fn with_hard_memory(self, hard_memory: u64) -> Self {
        Self {
            hard_memory,
            ..self
        }
    }

    /// The same step downscaling at most `documents` documents at once (`DOCX_TO_PDF_POOL_SIZE`); a
    /// document with a picture to downscale waits up to `wait` (`DOCX_TO_PDF_POOL_WAIT_TIMEOUT`) for its turn, then is
    /// refused 503 as the pool refuses it. A document with nothing to downscale never waits.
    #[must_use]
    pub fn with_concurrency(self, documents: usize, wait: std::time::Duration) -> Self {
        Self {
            permits: std::sync::Arc::new(tokio::sync::Semaphore::new(documents.max(1))),
            wait,
            ..self
        }
    }

    /// What the hard memory limit leaves the document's pictures, if there is one.
    fn budget(&self) -> Option<u64> {
        (self.hard_memory > 0).then(|| self.hard_memory.saturating_sub(BUDGET_RESERVE))
    }

    /// The same step with `downscale` in place of [`downscale_picture`]: a test's way to make one fail or panic.
    #[doc(hidden)]
    #[must_use]
    pub fn with_downscale(self, downscale: Downscale) -> Self {
        Self { downscale, ..self }
    }

    /// The same step with `fault` called at each [`Stage`]: a test's way to make it panic anywhere.
    #[doc(hidden)]
    #[must_use]
    pub fn with_fault(self, fault: Fault) -> Self {
        Self { fault, ..self }
    }

    /// Downscales or replaces the oversized pictures of the DOCX or ODT at `path` in place, logging and counting
    /// each; a document with none is left untouched. Returns the header to answer with, if any.
    ///
    /// A panic anywhere in the step leaves the document as it was received: it is logged as an error,
    /// counted as `outcome=failed`, and the document is converted without the step, so it is never a 500.
    ///
    /// Before any picture is decoded, the pictures' headers are summed against the budget ([`Self::with_hard_memory`]):
    /// past it, 413 with [`crate::conversion::OVER_MEMORY`]. Only a document with a picture to change then waits for
    /// one of the step's permits ([`Self::with_concurrency`]): past the wait, 503. An I/O error is logged and is a 500.
    pub async fn prepare(&self, path: &Path) -> Result<Option<HeaderValue>, Problem> {
        let io_error = |error: io::Error| {
            tracing::error!(%error, "the document's pictures could not be checked");
            Problem::Internal
        };
        let max_bitmap = self.max_bitmap;
        let plan = {
            let path = path.to_owned();
            tokio::task::spawn_blocking(move || plan_file(&path, max_bitmap)).await
        };
        let plan = match plan {
            Ok(plan) => plan.map_err(io_error)?,
            Err(error) => return self.failed(error).map_err(io_error),
        };
        if let Some(budget) = self.budget()
            && plan.held > budget
        {
            tracing::warn!(
                outcome = "over_budget",
                pictures = plan.pictures,
                oversized = plan.oversized,
                held_bytes = plan.held,
                budget_bytes = budget,
                hard_memory_bytes = self.hard_memory,
                "the document's pictures, after the planned downscale, need more memory than the hard limit \
                 leaves; refused before any is decoded"
            );
            return Err(Problem::TooLarge(crate::conversion::OVER_MEMORY));
        }
        if plan.oversized == 0 {
            return Ok(None);
        }
        let Ok(Ok(_permit)) =
            tokio::time::timeout(self.wait, self.permits.clone().acquire_owned()).await
        else {
            tracing::warn!(
                wait_seconds = self.wait.as_secs_f64(),
                "no turn of the pictures step came free within the wait limit"
            );
            return Err(Problem::Unavailable {
                detail: crate::conversion::unavailable(crate::pool::AcquireError::Timeout),
                retry_after: crate::conversion::RETRY_AFTER,
            });
        };
        let (path, downscale, fault) = (path.to_owned(), self.downscale, self.fault);
        let task =
            tokio::task::spawn_blocking(move || replace_file(&path, max_bitmap, downscale, fault))
                .await;
        let Prepared {
            changed,
            counts,
            header,
        } = match task {
            Ok(prepared) => prepared.map_err(io_error)?,
            Err(error) => return self.failed(error).map_err(io_error),
        };
        let max_bitmap_bytes = self.max_bitmap.min(LIBREOFFICE_MAX_BITMAP);
        for picture in &changed {
            let (entry, h) = (&picture.entry, &picture.header);
            match &picture.outcome {
                Outcome::Downscaled(done) => tracing::warn!(
                    outcome = "downscaled",
                    entry = %entry,
                    width = h.width,
                    height = h.height,
                    bitmap_bytes = h.bitmap(),
                    max_bitmap_bytes,
                    factor = done.factor,
                    downscaled_width = done.width,
                    downscaled_height = done.height,
                    "a picture's decoded bitmap passes the threshold; downscaled before conversion"
                ),
                Outcome::Replaced { why } => tracing::warn!(
                    outcome = "replaced",
                    entry = %entry,
                    width = h.width,
                    height = h.height,
                    bitmap_bytes = h.bitmap(),
                    max_bitmap_bytes,
                    why = %why,
                    "a picture's decoded bitmap passes the threshold; replaced by a placeholder before conversion"
                ),
            }
        }
        for (outcome, n) in counts.into_iter().filter(|(_, n)| *n > 0) {
            self.count.add(n, &[KeyValue::new("outcome", outcome)]);
        }
        Ok(header)
    }

    /// A blocking part of the step that did not finish: a panic is logged, counted as `outcome=failed`, and the
    /// document converted as it was received; anything else is an error.
    fn failed(&self, error: tokio::task::JoinError) -> io::Result<Option<HeaderValue>> {
        if !error.is_panic() {
            return Err(io::Error::other(error));
        }
        let payload = error.into_panic();
        tracing::error!(
            outcome = "failed",
            panic = %panic_message(&*payload),
            "the pictures step panicked; the document is converted as it was received"
        );
        self.count.add(1, &[KeyValue::new("outcome", "failed")]);
        Ok(None)
    }
}

/// What [`plan`] read from a document's pictures before any is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    /// The PNGs and JPEGs whose header was read.
    pub pictures: u64,
    /// Those past the threshold, to be downscaled or replaced.
    pub oversized: u64,
    /// What LibreOffice will hold for them all ([`held`]), each oversized one at its planned downscale and one that
    /// no factor brings under the threshold at nothing (it gets the placeholder).
    pub held: u64,
}

/// Reads the headers of the ZIP `source`'s PNGs and JPEGs, decoding none, and sums them as [`Plan`] says. The sum
/// errs high: a picture that will be replaced (interlaced, CMYK, damaged) is counted at its downscale, and a picture
/// in the package that no part shows is counted as if one did. A source that is not a ZIP this can read plans nothing.
pub fn plan<R: Read + Seek>(source: &mut R, max_bitmap: u64) -> io::Result<Plan> {
    let Some(listing) = crate::allowlist::zip_listing(source)? else {
        return Ok(Plan::default());
    };
    let bound = max_bitmap.min(LIBREOFFICE_MAX_BITMAP);
    let mut plan = Plan::default();
    for entry in &listing {
        let Some(h) =
            crate::allowlist::zip_head(source, entry, HEAD)?.and_then(|head| header(&head))
        else {
            continue;
        };
        plan.pictures += 1;
        let kept = if h.bitmap() > bound {
            plan.oversized += 1;
            downscale::factor(&h, bound).map(|factor| Header {
                width: h.width.div_ceil(factor),
                height: h.height.div_ceil(factor),
                ..h
            })
        } else {
            Some(h)
        };
        plan.held = plan.held.saturating_add(kept.as_ref().map_or(0, held));
    }
    Ok(plan)
}

/// [`plan`] of the file at `path`.
fn plan_file(path: &Path, max_bitmap: u64) -> io::Result<Plan> {
    plan(
        &mut io::BufReader::new(std::fs::File::open(path)?),
        max_bitmap,
    )
}

/// What [`replace_file`] did: the pictures it changed, how many of each outcome, and the header to answer with.
struct Prepared {
    changed: Vec<Changed>,
    counts: [(&'static str, u64); 2],
    header: Option<HeaderValue>,
}

/// [`replace_oversized`] from the file at `path` into a sibling, which then takes its place; the sibling is removed
/// when nothing was changed, on an error and on a panic. Downscaled pictures are written beside it first. Everything
/// that can fail or panic runs before the rename, so the received document is either replaced whole or left as it
/// was.
fn replace_file(
    path: &Path,
    max_bitmap: u64,
    downscale: Downscale,
    fault: Fault,
) -> io::Result<Prepared> {
    let mut sibling = Sibling(PathBuf::from(path));
    sibling.0.set_extension("pictures");
    let scratch = path.parent().unwrap_or_else(|| Path::new("."));
    let mut source = std::fs::File::open(path)?;
    let mut out = io::BufWriter::new(std::fs::File::create(&sibling.0)?);
    let changed =
        replace_oversized_faulted(&mut source, &mut out, max_bitmap, scratch, downscale, fault)?;
    drop(out);
    fault(Stage::Summary);
    let mut counts = [("downscaled", 0u64), ("replaced", 0u64)];
    for picture in &changed {
        let name = picture.outcome.name();
        counts
            .iter_mut()
            .find(|(n, _)| *n == name)
            .expect("named")
            .1 += 1;
    }
    let header = (!changed.is_empty()).then(|| {
        HeaderValue::from_str(&format!(
            "downscaled={}, replaced={}",
            counts[0].1, counts[1].1
        ))
        .expect("ASCII")
    });
    if !changed.is_empty() {
        std::fs::rename(&sibling.0, path)?;
    }
    Ok(Prepared {
        changed,
        counts,
        header,
    })
}

/// The rewritten document beside the received one, removed when dropped unless it has taken the received one's place:
/// no sibling outlives [`replace_file`], whether it returns or unwinds.
struct Sibling(PathBuf);

impl Drop for Sibling {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The placeholders, one per format so the entry's content type stays right: a light grey box, framed and crossed.
pub const PNG_PLACEHOLDER: &[u8] = include_bytes!("placeholder.png");
/// The JPEG placeholder, the same box as [`PNG_PLACEHOLDER`].
pub const JPEG_PLACEHOLDER: &[u8] = include_bytes!("placeholder.jpg");

/// Writes to `out` the ZIP `source` with every PNG and JPEG whose bitmap passes `max_bitmap` (capped at
/// [`LIBREOFFICE_MAX_BITMAP`]) changed: downscaled under it where it can be (its new bytes written to a file in
/// `scratch` first, removed when done), anything else replaced by its format's placeholder. Returns those pictures.
/// When there are none, or `source` is not a ZIP this can rewrite, nothing is written and the list is empty.
pub fn replace_oversized<R: Read + Seek, W: Write>(
    source: &mut R,
    out: &mut W,
    max_bitmap: u64,
    scratch: &Path,
) -> io::Result<Vec<Changed>> {
    replace_oversized_with(source, out, max_bitmap, scratch, downscale_picture)
}

/// Downscales one picture of a format to at most `bound` bytes of LibreOffice's bitmap, from its decoded entry to
/// `out`, any scratch file it needs made in the given directory (the step's `scratch`); the error says why it is
/// not downscaled.
pub type Downscale =
    fn(Kind, &mut dyn Read, &mut dyn Write, u64, &Path) -> Result<downscale::Downscaled, String>;

/// The [`Downscale`] of the service: [`downscale::png`] or [`jpeg::downscale_in`].
pub fn downscale_picture(
    kind: Kind,
    input: &mut dyn Read,
    out: &mut dyn Write,
    bound: u64,
    scratch: &Path,
) -> Result<downscale::Downscaled, String> {
    match kind {
        Kind::Png => downscale::png(input, out, bound).map_err(|r| r.to_string()),
        Kind::Jpeg => jpeg::downscale_in(input, out, bound, scratch).map_err(|r| r.to_string()),
    }
}

/// [`replace_oversized`] with `downscale` doing each picture's downscale. A downscale that panics is caught: that
/// picture keeps its placeholder, `why` saying `panicked: <message>`, and the next one is tried.
pub fn replace_oversized_with<R: Read + Seek, W: Write>(
    source: &mut R,
    out: &mut W,
    max_bitmap: u64,
    scratch: &Path,
    downscale: Downscale,
) -> io::Result<Vec<Changed>> {
    replace_oversized_faulted(source, out, max_bitmap, scratch, downscale, no_fault)
}

/// [`replace_oversized_with`] calling `fault` at each [`Stage`] it reaches.
fn replace_oversized_faulted<R: Read + Seek, W: Write>(
    source: &mut R,
    out: &mut W,
    max_bitmap: u64,
    scratch: &Path,
    downscale: Downscale,
    fault: Fault,
) -> io::Result<Vec<Changed>> {
    fault(Stage::Listing);
    let Some(listing) = crate::allowlist::zip_listing(source)? else {
        return Ok(Vec::new());
    };
    let bound = max_bitmap.min(LIBREOFFICE_MAX_BITMAP);
    let mut changed = Vec::new();
    let mut contents = Vec::with_capacity(listing.len());
    for entry in &listing {
        fault(Stage::Header);
        let found = crate::allowlist::zip_head(source, entry, HEAD)?
            .and_then(|head| header(&head))
            .filter(|h| h.bitmap() > bound);
        let Some(h) = found else {
            contents.push(Content::Kept);
            continue;
        };
        let (content, outcome) = match h.kind {
            Kind::Png | Kind::Jpeg => {
                match downscale_entry(source, entry, h.kind, bound, scratch, downscale)? {
                    Ok((file, done)) => (file, Outcome::Downscaled(done)),
                    Err(why) => {
                        let placeholder = match h.kind {
                            Kind::Png => PNG_PLACEHOLDER,
                            Kind::Jpeg => JPEG_PLACEHOLDER,
                        };
                        (Content::Bytes(placeholder), Outcome::Replaced { why })
                    }
                }
            }
        };
        contents.push(content);
        changed.push(Changed {
            entry: String::from_utf8_lossy(&entry.name).into_owned(),
            header: h,
            outcome,
        });
    }
    if changed.is_empty() {
        return Ok(changed);
    }
    rewrite(source, out, &listing, &mut contents, fault)?;
    Ok(changed)
}

/// An entry's content in the rewritten ZIP.
enum Content {
    /// Its compressed bytes as they are.
    Kept,
    /// These bytes, stored.
    Bytes(&'static [u8]),
    /// The bytes of this file, stored: `len` of them, whose CRC-32 is `crc`.
    File {
        file: std::fs::File,
        crc: u32,
        len: u32,
    },
}

/// Downscales the PNG or JPEG of `entry` (stored or deflated) under `bound` into a scratch file. The outer error is
/// the scratch file's; the inner one says why the picture is not downscaled.
fn downscale_entry<R: Read + Seek>(
    source: &mut R,
    entry: &crate::allowlist::zip::Listed,
    kind: Kind,
    bound: u64,
    scratch: &Path,
    downscale: Downscale,
) -> io::Result<Result<(Content, downscale::Downscaled), String>> {
    const STORED: u16 = 0;
    const DEFLATED: u16 = 8;
    let method = u16::from_le_bytes([entry.record[10], entry.record[11]]);
    source.seek(io::SeekFrom::Start(entry.data))?;
    let raw = source.by_ref().take(entry.compressed);
    let mut file = tempfile::tempfile_in(scratch)?;
    let mut sink = Crc {
        out: io::BufWriter::new(file.try_clone()?),
        hasher: crc32fast::Hasher::new(),
        len: 0,
    };
    // Unwind safety: a panic leaves `source` somewhere in the entry, and every later read of it seeks first; `sink`
    // and `file` are dropped with the picture, which keeps its placeholder.
    let mut one = |input: &mut dyn Read| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            downscale(kind, input, &mut sink, bound, scratch)
        }))
        .unwrap_or_else(|payload| Err(format!("panicked: {}", panic_message(&*payload))))
    };
    let done = match method {
        STORED => one(&mut { raw }),
        DEFLATED => one(&mut flate2::read::DeflateDecoder::new(raw)),
        other => return Ok(Err(format!("compression method {other} is not read"))),
    };
    let done = match done {
        Ok(done) => done,
        Err(why) => return Ok(Err(why)),
    };
    sink.out.flush()?;
    let Crc { out, hasher, len } = sink;
    drop(out);
    let crc = hasher.finalize();
    let Ok(len) = u32::try_from(len) else {
        return Ok(Err("the downscaled picture would need ZIP64".into()));
    };
    file.seek(io::SeekFrom::Start(0))?;
    Ok(Ok((Content::File { file, crc, len }, done)))
}

/// What a panic said: its message when it is a string, else that it said nothing readable.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message")
}

/// A writer that counts and checksums what passes through it.
struct Crc<W> {
    out: W,
    hasher: crc32fast::Hasher,
    len: u64,
}

impl<W: Write> Write for Crc<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// How much of an entry [`header`] is given: a PNG's chunks before `IDAT` (an ICC profile among them) or a JPEG's
/// segments before its frame header (EXIF and its thumbnail among them) fit, with room.
const HEAD: usize = 1 << 20;

/// General-purpose flag bit 3: the sizes and CRC follow the data. A rewritten local header carries them itself.
const DATA_DESCRIPTOR: u16 = 1 << 3;

/// Writes the entries of `listing` in order, each with its compressed bytes as they are, or stored with the content
/// `contents` gives it; then the central directory, its records patched to match.
fn rewrite<R: Read + Seek, W: Write>(
    source: &mut R,
    out: &mut W,
    listing: &[crate::allowlist::zip::Listed],
    contents: &mut [Content],
    fault: Fault,
) -> io::Result<()> {
    let too_large = || io::Error::other("the rewritten ZIP would need ZIP64");
    let mut directory = Vec::new();
    let mut offset = 0u64;
    for (entry, content) in listing.iter().zip(contents.iter_mut()) {
        let mut record = entry.record.clone();
        let flags = u16::from_le_bytes([record[8], record[9]]) & !DATA_DESCRIPTOR;
        record[8..10].copy_from_slice(&flags.to_le_bytes());
        let stored = match content {
            Content::Kept => None,
            Content::Bytes(bytes) => Some((
                crc32fast::hash(bytes),
                u32::try_from(bytes.len()).map_err(|_| too_large())?,
            )),
            Content::File { crc, len, .. } => Some((*crc, *len)),
        };
        if let Some((crc, len)) = stored {
            let size = len.to_le_bytes();
            record[10..12].copy_from_slice(&0u16.to_le_bytes()); // stored
            record[16..20].copy_from_slice(&crc.to_le_bytes());
            record[20..24].copy_from_slice(&size);
            record[24..28].copy_from_slice(&size);
        }
        record[42..46].copy_from_slice(
            &u32::try_from(offset)
                .map_err(|_| too_large())?
                .to_le_bytes(),
        );

        let mut local = Vec::with_capacity(30 + entry.name.len());
        local.extend_from_slice(b"PK\x03\x04");
        local.extend_from_slice(&record[6..28]); // version needed, flags, method, time, date, CRC, sizes
        local.extend_from_slice(
            &u16::try_from(entry.name.len())
                .map_err(|_| too_large())?
                .to_le_bytes(),
        );
        local.extend_from_slice(&0u16.to_le_bytes()); // no extra field
        local.extend_from_slice(&entry.name);
        out.write_all(&local)?;
        let written = match content {
            Content::Bytes(bytes) => {
                out.write_all(bytes)?;
                bytes.len() as u64
            }
            Content::File { file, .. } => io::copy(file, out)?,
            Content::Kept => {
                source.seek(io::SeekFrom::Start(entry.data))?;
                io::copy(&mut source.by_ref().take(entry.compressed), out)?
            }
        };
        offset += local.len() as u64 + written;
        directory.extend_from_slice(&record);
        fault(Stage::Rewrite);
    }
    let count = u16::try_from(listing.len()).map_err(|_| too_large())?;
    let mut end = b"PK\x05\x06\0\0\0\0".to_vec();
    end.extend_from_slice(&count.to_le_bytes());
    end.extend_from_slice(&count.to_le_bytes());
    end.extend_from_slice(
        &u32::try_from(directory.len())
            .map_err(|_| too_large())?
            .to_le_bytes(),
    );
    end.extend_from_slice(
        &u32::try_from(offset)
            .map_err(|_| too_large())?
            .to_le_bytes(),
    );
    end.extend_from_slice(&0u16.to_le_bytes());
    out.write_all(&directory)?;
    out.write_all(&end)?;
    out.flush()
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// `IHDR` gives the size and colour type; a `tRNS` before `IDAT` adds alpha (`vcl/source/filter/png/PngImageReader.cxx`).
fn png(head: &[u8]) -> Option<Header> {
    let ihdr = head.get(8..33)?;
    if &ihdr[4..8] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(ihdr[8..12].try_into().ok()?);
    let height = u32::from_be_bytes(ihdr[12..16].try_into().ok()?);
    let colour = ihdr[17];
    let mut alpha = matches!(colour, 4 | 6);
    let mut at = 33;
    while let Some(chunk) = head.get(at..at + 8) {
        let length = u32::from_be_bytes(chunk[..4].try_into().ok()?) as usize;
        match &chunk[4..8] {
            b"IDAT" => break,
            b"tRNS" => alpha = true,
            _ => {}
        }
        at = at.checked_add(12)?.checked_add(length)?;
    }
    let bits = match (alpha, colour) {
        (true, _) => 32,
        (false, 0) => 8,
        (false, _) => 24,
    };
    Some(Header {
        kind: Kind::Png,
        width,
        height,
        bits,
    })
}

/// The frame header (`SOF0`-`SOF15` but `DHT`, `JPG` and `DAC`) gives the size and the components: one is held
/// gray at 8 bits, more as RGB at 24.
fn jpeg(head: &[u8]) -> Option<Header> {
    let mut at = 2;
    loop {
        if *head.get(at)? != 0xFF {
            return None;
        }
        while *head.get(at)? == 0xFF {
            at += 1;
        }
        let marker = head[at];
        at += 1;
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            continue; // no length
        }
        let length = usize::from(u16::from_be_bytes(head.get(at..at + 2)?.try_into().ok()?));
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let frame = head.get(at + 2..at + 8)?;
            let height = u16::from_be_bytes([frame[1], frame[2]]);
            let width = u16::from_be_bytes([frame[3], frame[4]]);
            return Some(Header {
                kind: Kind::Jpeg,
                width: width.into(),
                height: height.into(),
                bits: if frame[5] == 1 { 8 } else { 24 },
            });
        }
        if marker == 0xDA {
            return None; // scan data before any frame header
        }
        at += length;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG's signature and `IHDR`, then `chunks` (type, data) and an `IDAT`. CRCs are zero: nothing here checks them.
    fn png(
        width: u32,
        height: u32,
        depth: u8,
        colour: u8,
        chunks: &[(&[u8; 4], &[u8])],
    ) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0; 4]);
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[depth, colour, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        for (kind, data) in chunks {
            chunk(kind, data);
        }
        chunk(b"IDAT", &[0; 8]);
        out
    }

    /// A JPEG's SOI, an APP0 and a DQT before its frame header, as an encoder writes them, then the frame `sof`.
    fn jpeg(sof: u8, width: u16, height: u16, components: u8) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        out.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        out.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        out.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
        out.extend_from_slice(&[1; 64]);
        out.extend_from_slice(&[0xFF, 0xFF, sof]); // a fill byte before the marker is allowed
        let length = 8 + 3 * u16::from(components);
        out.extend_from_slice(&length.to_be_bytes());
        out.push(8);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.push(components);
        for id in 0..components {
            out.extend_from_slice(&[id + 1, 0x11, 0]);
        }
        out
    }

    /// A ZIP of `entries` (name, content, deflated), as an independent writer makes it.
    fn archive(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        use ::zip::write::SimpleFileOptions;
        let mut writer = ::zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, content, deflated) in entries {
            let method = if *deflated {
                ::zip::CompressionMethod::Deflated
            } else {
                ::zip::CompressionMethod::Stored
            };
            writer
                .start_file(
                    *name,
                    SimpleFileOptions::default().compression_method(method),
                )
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// The entries of the ZIP `bytes` in order, read by an independent reader that checks every CRC.
    fn entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut archive = ::zip::ZipArchive::new(io::Cursor::new(bytes)).unwrap();
        (0..archive.len())
            .map(|i| {
                let mut file = archive.by_index(i).unwrap();
                let mut content = Vec::new();
                file.read_to_end(&mut content).unwrap();
                (file.name().unwrap().into_owned(), content)
            })
            .collect()
    }

    fn replace(zip: &[u8], max_bitmap: u64) -> (Vec<Changed>, Vec<u8>) {
        let mut out = Vec::new();
        let scratch = tempfile::tempdir().unwrap();
        let changed = replace_oversized(
            &mut io::Cursor::new(zip),
            &mut out,
            max_bitmap,
            scratch.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_dir(scratch.path()).unwrap().count(),
            0,
            "no scratch file left behind"
        );
        (changed, out)
    }

    /// A real RGB PNG, encoded by the `png` crate: a gradient, so every block averages to something of its own.
    fn real_png(width: u32, height: u32, interlaced: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = ::png::Encoder::new(&mut out, width, height);
        encoder.set_color(::png::ColorType::Rgb);
        encoder.set_depth(::png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let pixels: Vec<u8> = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    [
                        x.to_le_bytes()[0],
                        y.to_le_bytes()[0],
                        (x ^ y).to_le_bytes()[0],
                    ]
                })
            })
            .collect();
        writer.write_image_data(&pixels).unwrap();
        writer.finish().unwrap();
        if interlaced {
            out[28] = 1; // IHDR's interlace method; the CRC follows
            let crc = crc32fast::hash(&out[12..29]);
            out[29..33].copy_from_slice(&crc.to_be_bytes());
        }
        out
    }

    /// Width, height and colour type of the PNG `bytes`, read by the `png` crate to its end.
    fn decoded(bytes: &[u8]) -> (u32, u32, ::png::ColorType) {
        let mut reader = ::png::Decoder::new(io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut pixels).unwrap();
        (frame.width, frame.height, frame.color_type)
    }

    const CT: &[u8] = br#"<Types><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn a_png_past_the_threshold_is_downscaled_in_place_and_every_other_entry_kept() {
        // RGB 300 × 200 holds 180 000 bytes; halved, 150 × 100 holds 45 000 (a 450-byte scanline), under 50 000.
        let big = real_png(300, 200, false);
        let small = real_png(10, 10, false);
        for deflated in [false, true] {
            let zip = archive(&[
                ("[Content_Types].xml", CT, true),
                ("word/document.xml", b"<w:document/>", true),
                ("word/media/image1.png", &big, deflated),
                ("word/media/image2.png", &small, true),
            ]);
            let (changed, out) = replace(&zip, 50_000);
            assert_eq!(
                changed,
                [Changed {
                    entry: "word/media/image1.png".into(),
                    header: header(&big).unwrap(),
                    outcome: Outcome::Downscaled(downscale::Downscaled {
                        factor: 2,
                        width: 150,
                        height: 100
                    }),
                }],
                "deflated: {deflated}"
            );
            let entries = entries(&out);
            assert_eq!(entries[0], ("[Content_Types].xml".to_owned(), CT.to_vec()));
            assert_eq!(entries[1].1, b"<w:document/>");
            assert_eq!(entries[2].0, "word/media/image1.png");
            assert_eq!(decoded(&entries[2].1), (150, 100, ::png::ColorType::Rgb));
            assert_eq!(
                entries[3],
                ("word/media/image2.png".to_owned(), small.clone())
            );
        }
    }

    #[test]
    fn an_interlaced_png_keeps_the_placeholder() {
        let big = real_png(300, 200, true);
        let zip = archive(&[("word/media/image1.png", &big, true)]);
        let (changed, out) = replace(&zip, 50_000);
        assert_eq!(changed[0].outcome.name(), "replaced");
        assert_eq!(
            changed[0].outcome,
            Outcome::Replaced {
                why: "interlaced (Adam7)".into()
            }
        );
        assert_eq!(entries(&out)[0].1, PNG_PLACEHOLDER);
    }

    #[test]
    fn a_damaged_picture_past_the_threshold_is_replaced_and_every_other_entry_kept() {
        let big = png(9459, 9459, 8, 2, &[]); // 268 446 420 bytes at 24 bits: past 256 MiB; its IDAT is no zlib
        let small = png(100, 100, 8, 6, &[]);
        let zip = archive(&[
            ("[Content_Types].xml", CT, true),
            ("word/document.xml", b"<w:document/>", true),
            ("word/media/image1.png", &big, false),
            ("word/media/image2.png", &small, true),
        ]);
        let (replaced, out) = replace(&zip, 256 * MIB);
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].entry, "word/media/image1.png");
        assert_eq!(replaced[0].header, header(&big).unwrap());
        assert!(
            matches!(&replaced[0].outcome, Outcome::Replaced { why } if why.starts_with("damaged")),
            "{:?}",
            replaced[0].outcome
        );
        assert_eq!(
            entries(&out),
            [
                ("[Content_Types].xml".to_owned(), CT.to_vec()),
                ("word/document.xml".to_owned(), b"<w:document/>".to_vec()),
                ("word/media/image1.png".to_owned(), PNG_PLACEHOLDER.to_vec()),
                ("word/media/image2.png".to_owned(), small),
            ]
        );
    }

    #[test]
    fn a_baseline_jpeg_past_the_threshold_is_downscaled_in_place() {
        // RGB 300 × 200 holds 180 000 bytes; halved, 150 × 100 holds 45 000, under 50 000.
        let mut big = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut big, 85);
        encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
        let pixels: Vec<u8> = (0..200u32)
            .flat_map(|y| {
                (0..300u32).flat_map(move |x| [x.to_le_bytes()[0], y.to_le_bytes()[0], 99])
            })
            .collect();
        encoder
            .encode(&pixels, 300, 200, jpeg_encoder::ColorType::Rgb)
            .unwrap();
        for deflated in [false, true] {
            let zip = archive(&[
                ("[Content_Types].xml", CT, true),
                ("word/media/image1.jpeg", &big, deflated),
            ]);
            let (changed, out) = replace(&zip, 50_000);
            assert_eq!(
                changed,
                [Changed {
                    entry: "word/media/image1.jpeg".into(),
                    header: header(&big).unwrap(),
                    outcome: Outcome::Downscaled(downscale::Downscaled {
                        factor: 2,
                        width: 150,
                        height: 100
                    }),
                }],
                "deflated: {deflated}"
            );
            let entries = entries(&out);
            assert_eq!(entries[0], ("[Content_Types].xml".to_owned(), CT.to_vec()));
            let mut decoder = jpeg_decoder::Decoder::new(entries[1].1.as_slice());
            decoder.decode().unwrap();
            let info = decoder.info().unwrap();
            assert_eq!((info.width, info.height), (150, 100));
        }
    }

    #[test]
    fn a_downscale_that_panics_keeps_the_placeholder_and_the_next_picture_is_still_downscaled() {
        fn panics_on_png(
            kind: Kind,
            input: &mut dyn Read,
            out: &mut dyn Write,
            bound: u64,
            scratch: &Path,
        ) -> Result<downscale::Downscaled, String> {
            assert!(kind != Kind::Png, "a forced panic");
            downscale_picture(kind, input, out, bound, scratch)
        }
        let png = real_png(300, 200, false);
        let mut jpeg = Vec::new();
        jpeg_encoder::Encoder::new(&mut jpeg, 85)
            .encode(
                &vec![99; 300 * 200 * 3],
                300,
                200,
                jpeg_encoder::ColorType::Rgb,
            )
            .unwrap();
        let zip = archive(&[
            ("word/media/image1.png", &png, true),
            ("word/media/image2.jpeg", &jpeg, true),
        ]);
        let mut out = Vec::new();
        let scratch = tempfile::tempdir().unwrap();
        let changed = replace_oversized_with(
            &mut io::Cursor::new(&zip),
            &mut out,
            50_000,
            scratch.path(),
            panics_on_png,
        )
        .unwrap();
        assert_eq!(
            changed[0].outcome,
            Outcome::Replaced {
                why: "panicked: a forced panic".into()
            }
        );
        assert_eq!(changed[1].outcome.name(), "downscaled");
        let entries = entries(&out);
        assert_eq!(entries[0].1, PNG_PLACEHOLDER);
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_arithmetic_coded_jpeg_is_replaced_by_the_jpeg_placeholder_wherever_it_is() {
        // A progressive JPEG is downscaled; arithmetic coding is still refused.
        let big = jpeg(0xC9, 20000, 20000, 3);
        let zip = archive(&[
            (
                "mimetype",
                b"application/vnd.oasis.opendocument.text",
                false,
            ),
            ("Pictures/a.jpg", &big, true),
        ]);
        let (replaced, out) = replace(&zip, 256 * MIB);
        assert_eq!(replaced.len(), 1);
        assert_eq!(
            replaced[0].outcome,
            Outcome::Replaced {
                why: "arithmetic, lossless or hierarchical coding (SOF9)".into()
            }
        );
        assert_eq!(
            entries(&out)[1],
            ("Pictures/a.jpg".to_owned(), JPEG_PLACEHOLDER.to_vec())
        );
    }

    #[test]
    fn a_picture_at_the_threshold_is_kept_and_nothing_is_written() {
        // RGB 9458²: a 28 376-byte scanline (9458 × 3 = 28 374, aligned to 4), 268 380 208 bytes, under
        // 256 MiB = 268 435 456; 9459² is 28 380 × 9459 = 268 446 420, past it.
        let zip = archive(&[
            ("[Content_Types].xml", CT, true),
            ("word/media/image1.png", &png(9458, 9458, 8, 2, &[]), true),
        ]);
        let (replaced, out) = replace(&zip, 256 * MIB);
        assert_eq!(replaced, [] as [Changed; 0]);
        assert_eq!(out, [] as [u8; 0]);
    }

    #[test]
    fn the_threshold_is_capped_at_libreoffices_bound() {
        let at = |side| archive(&[("word/media/i.png", &png(side, side, 8, 2, &[]), true)]);
        assert!(
            replace(&at(18918), u64::MAX).0.is_empty(),
            "RGB 18 918² is kept by LibreOffice"
        );
        assert_eq!(
            replace(&at(18919), u64::MAX).0.len(),
            1,
            "RGB 18 919² is dropped by it"
        );
    }

    #[test]
    fn a_zip_without_a_readable_directory_is_left_alone() {
        let mut zip = archive(&[("word/media/i.png", &png(20000, 20000, 8, 0, &[]), true)]);
        zip.truncate(zip.len() - 30); // the end-of-directory record cut
        let (replaced, out) = replace(&zip, 256 * MIB);
        assert!(replaced.is_empty() && out.is_empty());
    }

    fn bitmap(head: &[u8]) -> u64 {
        header(head).expect("a header").bitmap()
    }

    // Expected buffers and kept/dropped verdicts: measured to the pixel on LibreOffice 26.8.

    #[test]
    fn the_largest_size_a_png_can_claim_does_not_overflow_the_bitmap() {
        // Found by fuzzing (tests/pictures_fuzz.rs): 2³² - 1 a side at 32 bits is past 2⁶⁴ bytes.
        assert_eq!(bitmap(&png(u32::MAX, u32::MAX, 8, 6, &[])), u64::MAX);
        let zip = archive(&[(
            "word/media/i.png",
            &png(u32::MAX, u32::MAX, 8, 6, &[]),
            true,
        )]);
        assert_eq!(replace(&zip, 256 * MIB).0.len(), 1, "replaced, not kept");
    }

    #[test]
    fn a_grayscale_png_is_held_at_8_bits() {
        assert_eq!(bitmap(&png(32767, 32767, 8, 0, &[])), 1_073_709_056);
        assert_eq!(bitmap(&png(32768, 32768, 8, 0, &[])), 1_073_741_824);
        assert_eq!(
            bitmap(&png(32768, 32768, 16, 0, &[])),
            1_073_741_824,
            "16-bit is scaled to 8"
        );
    }

    #[test]
    fn an_rgb_or_palette_png_is_held_at_24_bits() {
        assert_eq!(bitmap(&png(18918, 18918, 8, 2, &[])), 1_073_710_008);
        assert_eq!(bitmap(&png(18919, 18919, 8, 2, &[])), 1_073_842_440);
        assert_eq!(bitmap(&png(18919, 18919, 8, 3, &[])), 1_073_842_440);
    }

    #[test]
    fn any_alpha_in_a_png_is_held_at_32_bits() {
        assert_eq!(bitmap(&png(16383, 16383, 8, 6, &[])), 1_073_610_756);
        assert_eq!(bitmap(&png(16384, 16384, 8, 6, &[])), 1_073_741_824);
        assert_eq!(
            bitmap(&png(16384, 16384, 8, 4, &[])),
            1_073_741_824,
            "gray + alpha"
        );
        let trns: (&[u8; 4], &[u8]) = (b"tRNS", &[0, 0]);
        let iccp: (&[u8; 4], &[u8]) = (b"iCCP", &[0; 300]);
        assert_eq!(
            bitmap(&png(16384, 16384, 8, 0, &[iccp, trns])),
            1_073_741_824,
            "gray + tRNS"
        );
        assert_eq!(
            bitmap(&png(16384, 16384, 8, 3, &[trns])),
            1_073_741_824,
            "palette + tRNS"
        );
    }

    #[test]
    fn a_trns_after_the_pixels_does_not_count() {
        // A tRNS is only valid before IDAT; nothing after IDAT is read.
        let mut late = png(18918, 18918, 8, 2, &[]);
        late.extend_from_slice(&[0, 0, 0, 2]);
        late.extend_from_slice(b"tRNS\0\0\0\0\0\0");
        assert_eq!(bitmap(&late), 1_073_710_008);
    }

    #[test]
    fn a_jpeg_is_held_at_8_bits_gray_and_24_otherwise() {
        assert_eq!(bitmap(&jpeg(0xC0, 32767, 32767, 1)), 1_073_709_056);
        assert_eq!(
            bitmap(&jpeg(0xC2, 32768, 32768, 1)),
            1_073_741_824,
            "progressive"
        );
        assert_eq!(bitmap(&jpeg(0xC0, 18918, 18918, 3)), 1_073_710_008);
        assert_eq!(bitmap(&jpeg(0xC1, 18919, 18919, 3)), 1_073_842_440);
        assert_eq!(header(&jpeg(0xC0, 10, 20, 3)).unwrap().kind, Kind::Jpeg);
    }

    #[test]
    fn the_bound_falls_between_the_measured_kept_and_dropped_pictures() {
        for kept in [1_073_709_056, 1_073_710_008, 1_073_610_756] {
            assert!(kept <= LIBREOFFICE_MAX_BITMAP);
        }
        for dropped in [1_073_741_824, 1_073_842_440] {
            assert!(dropped > LIBREOFFICE_MAX_BITMAP);
        }
    }

    #[test]
    fn anything_else_has_no_header() {
        assert_eq!(header(b""), None);
        assert_eq!(header(b"GIF89a\x01\x00\x01\x00"), None);
        assert_eq!(
            header(&png(10, 10, 8, 2, &[])[..20]),
            None,
            "cut inside IHDR"
        );
        assert_eq!(header(&jpeg(0xC0, 10, 10, 3)[..30]), None, "cut before SOF");
        // DHT (C4), JPG (C8) and DAC (CC) share the SOFn range but carry no size.
        let dht = jpeg(0xC4, 10, 10, 3);
        assert_eq!(header(&dht), None);
    }

    /// A panic at any stage of [`replace_file`], a rewrite cut short among them, leaves the received
    /// document as it was and nothing beside it.
    #[test]
    fn a_panic_at_any_stage_leaves_the_document_as_it_was_and_nothing_beside_it() {
        fn listing(stage: Stage) {
            assert!(stage != Stage::Listing, "a forced panic");
        }
        fn header(stage: Stage) {
            assert!(stage != Stage::Header, "a forced panic");
        }
        fn rewrite(stage: Stage) {
            assert!(stage != Stage::Rewrite, "a forced panic");
        }
        fn summary(stage: Stage) {
            assert!(stage != Stage::Summary, "a forced panic");
        }
        let zip = archive(&[
            ("word/document.xml", b"<w:document/>", true),
            ("word/media/image1.png", &real_png(300, 200, false), true),
            ("word/media/image2.png", &real_png(10, 10, false), true),
        ]);
        for fault in [listing as Fault, header, rewrite, summary] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("document");
            std::fs::write(&path, &zip).unwrap();
            let caught =
                std::panic::catch_unwind(|| replace_file(&path, 50_000, downscale_picture, fault));
            assert!(caught.is_err(), "the fault panics");
            assert_eq!(std::fs::read(&path).unwrap(), zip);
            let left: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(left, ["document"]);
        }
    }

    #[test]
    fn without_a_fault_the_same_document_is_rewritten_in_place() {
        let zip = archive(&[("word/media/image1.png", &real_png(300, 200, false), true)]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("document");
        std::fs::write(&path, &zip).unwrap();
        let prepared = replace_file(&path, 50_000, downscale_picture, no_fault).unwrap();
        assert_eq!(prepared.changed.len(), 1);
        assert_eq!(prepared.header.unwrap(), "downscaled=1, replaced=0");
        assert_ne!(std::fs::read(&path).unwrap(), zip);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
