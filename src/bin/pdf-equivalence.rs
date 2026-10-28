//! `pdf-equivalence <expected.pdf> <actual.pdf> [--dpi N] [--colour-tolerance N] [--fill-tolerance N] [--radius N]
//! [--max-fraction F]`: PDF equivalence (`docx_to_pdf::pdf_equivalence`) from the command line, for the corpus and by
//! hand. Prints the report; exits 0 when equivalent, 1 when not, 2 when a PDF could not be read.

use docx_to_pdf::pdf_equivalence::{Thresholds, compare_files};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match parse(std::env::args().skip(1)) {
        Ok((expected, actual, t)) => match compare_files(&expected, &actual, &t) {
            Ok(report) => {
                println!("{report}");
                if report.is_equivalent() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(e) => {
                eprintln!("pdf-equivalence: {e}");
                ExitCode::from(2)
            }
        },
        Err(e) => {
            eprintln!(
                "pdf-equivalence: {e}\nusage: pdf-equivalence <expected.pdf> <actual.pdf> [--dpi N] \
                 [--colour-tolerance N] [--fill-tolerance N] [--radius N] [--max-fraction F]"
            );
            ExitCode::from(2)
        }
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<(PathBuf, PathBuf, Thresholds), String> {
    let mut t = Thresholds::default();
    let mut files = Vec::new();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--dpi" => t.dpi = value(&arg)?.parse().map_err(|e| format!("--dpi: {e}"))?,
            "--colour-tolerance" => {
                t.colour_tolerance = value(&arg)?
                    .parse()
                    .map_err(|e| format!("--colour-tolerance: {e}"))?;
            }
            "--fill-tolerance" => {
                t.fill_tolerance = value(&arg)?
                    .parse()
                    .map_err(|e| format!("--fill-tolerance: {e}"))?;
            }
            "--radius" => {
                t.radius_px = value(&arg)?.parse().map_err(|e| format!("--radius: {e}"))?;
            }
            "--max-fraction" => {
                t.max_differing_fraction = value(&arg)?
                    .parse()
                    .map_err(|e| format!("--max-fraction: {e}"))?;
            }
            _ if arg.starts_with("--") => return Err(format!("unknown option {arg}")),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    match <[PathBuf; 2]>::try_from(files) {
        Ok([e, a]) => Ok((e, a, t)),
        Err(_) => Err("two PDFs expected".to_owned()),
    }
}
