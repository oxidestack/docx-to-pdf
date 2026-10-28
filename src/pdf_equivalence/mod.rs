//! PDF equivalence: two PDFs of the same input are equal when their page count, page sizes
//! and extracted text are equal and every page, rendered at a fixed resolution, differs by less than a
//! threshold. Bytes are never compared.
//!
//! [`compare()`] judges facts already read; [`compare_files`] and [`compare_bytes`] read them through poppler
//! ([`poppler`]) first. The report names every difference: which check, which page, by how much, where.

pub mod compare;
pub mod poppler;
pub mod raster;

pub use compare::{
    Document, Finding, Page, Report, Thresholds, compare, document_words_difference,
};
pub use poppler::Error;
pub use raster::{Raster, Rgb};

use std::path::Path;

/// Compares the PDF at `actual` with the reference at `expected`.
pub fn compare_files(expected: &Path, actual: &Path, t: &Thresholds) -> Result<Report, Error> {
    let e = poppler::read(expected, t.dpi)?;
    let a = poppler::read(actual, t.dpi)?;
    Ok(compare(&e, &a, t))
}

/// Compares two PDFs held in memory, as the scenario harness has them from HTTP answers.
pub fn compare_bytes(expected: &[u8], actual: &[u8], t: &Thresholds) -> Result<Report, Error> {
    let dir = tempfile::tempdir().map_err(|e| Error(format!("no temporary directory: {e}")))?;
    let (e, a) = (
        dir.path().join("expected.pdf"),
        dir.path().join("actual.pdf"),
    );
    std::fs::write(&e, expected).map_err(|err| Error(format!("{}: {err}", e.display())))?;
    std::fs::write(&a, actual).map_err(|err| Error(format!("{}: {err}", a.display())))?;
    compare_files(&e, &a, t)
}
