//! Reading a PDF's facts through poppler's command-line tools on PATH: `pdfinfo` for the page sizes,
//! `pdftotext` for each page's text, `pdftoppm` for each page rendered to RGB. Both PDFs of a comparison
//! go through the same poppler, so its version does not decide a verdict.

use super::compare::{Document, Page};
use super::raster::Raster;
use std::fmt;
use std::path::Path;
use std::process::Command;

/// A PDF poppler could not read, or a poppler tool that is missing or failed.
#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// Reads the facts of the PDF at `path`, its pages rendered at `dpi`.
pub fn read(path: &Path, dpi: u32) -> Result<Document, Error> {
    let sizes = page_sizes(path)?;
    let texts = page_texts(path, sizes.len())?;
    let rasters = renderings(path, dpi, sizes.len())?;
    let pages = sizes
        .into_iter()
        .zip(texts)
        .zip(rasters)
        .map(|((size, text), raster)| Page { size, text, raster })
        .collect();
    Ok(Document { pages })
}

fn run(tool: &str, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>, Error> {
    let out = Command::new(tool).args(args).output().map_err(|e| {
        Error(format!(
            "{tool} could not be started ({e}); PDF equivalence needs poppler-utils on PATH"
        ))
    })?;
    if !out.status.success() {
        return Err(Error(format!(
            "{tool} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// `pdfinfo -f 1 -l <n>` prints `Pages: <n>` and a `Page <i> size: <w> x <h> pts` line per page.
fn page_sizes(path: &Path) -> Result<Vec<(f64, f64)>, Error> {
    let info = String::from_utf8_lossy(&run("pdfinfo", &[path.as_os_str()])?).into_owned();
    let count: usize = info
        .lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| {
            Error(format!(
                "pdfinfo printed no page count for {}",
                path.display()
            ))
        })?;
    let last = count.to_string();
    let out = run(
        "pdfinfo",
        &[
            "-f".as_ref(),
            "1".as_ref(),
            "-l".as_ref(),
            last.as_ref(),
            path.as_os_str(),
        ],
    )?;
    let sizes: Vec<(f64, f64)> = String::from_utf8_lossy(&out)
        .lines()
        .filter_map(parse_size_line)
        .collect();
    if sizes.len() != count {
        return Err(Error(format!(
            "pdfinfo printed {} page sizes for {count} pages of {}",
            sizes.len(),
            path.display()
        )));
    }
    Ok(sizes)
}

fn parse_size_line(line: &str) -> Option<(f64, f64)> {
    let rest = line.strip_prefix("Page")?;
    let (_, dims) = rest.split_once("size:")?;
    let mut words = dims.split_whitespace();
    let w = words.next()?.parse().ok()?;
    (words.next()? == "x").then_some(())?;
    let h = words.next()?.parse().ok()?;
    Some((w, h))
}

/// `pdftotext` ends every page with a form feed.
fn page_texts(path: &Path, count: usize) -> Result<Vec<String>, Error> {
    let out = run(
        "pdftotext",
        &[
            "-enc".as_ref(),
            "UTF-8".as_ref(),
            path.as_os_str(),
            "-".as_ref(),
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let mut pages: Vec<String> = text.split('\u{c}').map(str::to_owned).collect();
    pages.truncate(count);
    pages.resize(count, String::new());
    Ok(pages)
}

fn renderings(path: &Path, dpi: u32, count: usize) -> Result<Vec<Raster>, Error> {
    let dir = tempfile::tempdir().map_err(|e| Error(format!("no temporary directory: {e}")))?;
    let prefix = dir.path().join("p");
    let dpi_arg = dpi.to_string();
    run(
        "pdftoppm",
        &[
            "-r".as_ref(),
            dpi_arg.as_ref(),
            "-forcenum".as_ref(),
            path.as_os_str(),
            prefix.as_os_str(),
        ],
    )?;
    // `p-1.ppm` or `p-01.ppm`: the digits are padded to the page count's width.
    let mut files: Vec<(usize, std::path::PathBuf)> = std::fs::read_dir(dir.path())
        .map_err(|e| Error(format!("pdftoppm's output: {e}")))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?;
            let n = stem.strip_prefix("p-")?.parse().ok()?;
            Some((n, p))
        })
        .collect();
    files.sort();
    if files.len() != count {
        return Err(Error(format!(
            "pdftoppm rendered {} pages of {count} of {}",
            files.len(),
            path.display()
        )));
    }
    files
        .iter()
        .map(|(_, p)| {
            let bytes = std::fs::read(p).map_err(|e| Error(format!("{}: {e}", p.display())))?;
            parse_ppm(&bytes, dpi)
                .ok_or_else(|| Error(format!("{} is not a binary PPM", p.display())))
        })
        .collect()
}

/// A binary PPM (`P6`) with a maximum of 255, as `pdftoppm` writes it.
fn parse_ppm(bytes: &[u8], dpi: u32) -> Option<Raster> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while bytes.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        if bytes[i] == b'#' {
            while *bytes.get(i)? != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while !bytes.get(i)?.is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&bytes[start..i]).ok()?);
    }
    let (width, height): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    if fields[0] != "P6" || fields[3] != "255" {
        return None;
    }
    let pixels = bytes
        .get(i + 1..i + 1 + 3 * width * height)?
        .as_chunks::<3>()
        .0
        .to_vec();
    Some(Raster {
        width,
        height,
        dpi,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pdfinfo_size_line_reads_as_points() {
        assert_eq!(
            parse_size_line("Page    1 size: 595.28 x 841.89 pts (A4)"),
            Some((595.28, 841.89))
        );
        assert_eq!(parse_size_line("Pages:          2"), None);
        assert_eq!(parse_size_line("Page    2 rot:  0"), None);
    }

    #[test]
    fn a_binary_ppm_with_a_comment_reads_as_a_raster() {
        let mut ppm = b"P6\n# made by a test\n2 1\n255\n".to_vec();
        ppm.extend_from_slice(&[0, 128, 255, 252, 230, 244]);
        let r = parse_ppm(&ppm, 50).unwrap();
        assert_eq!((r.width, r.height, r.dpi), (2, 1, 50));
        assert_eq!(r.pixels, vec![[0, 128, 255], [252, 230, 244]]);
        assert_eq!(parse_ppm(b"P5\n1 1\n255\n\0", 50), None);
    }
}
