//! A minimal PDF writer for tests: pages of a given size whose content stream is written by the test,
//! with Helvetica as `/F1`. Enough for poppler to read, render and extract text from; no third-party file.

use std::fmt::Write as _;
pub struct TestPage {
    pub size: (f64, f64),
    pub content: String,
}

pub const A4: (f64, f64) = (595.28, 841.89);
pub const LETTER: (f64, f64) = (612.0, 792.0);

/// A page with one line of text per entry, `(x, y, text)` in points from the bottom left, 12 pt Helvetica.
pub fn text_page(size: (f64, f64), lines: &[(f64, f64, &str)]) -> TestPage {
    let mut content = String::new();
    for (x, y, text) in lines {
        writeln!(content, "BT /F1 12 Tf {x} {y} Td ({text}) Tj ET").unwrap();
    }
    TestPage { size, content }
}

/// Adds a filled black rectangle `(x, y, w, h)` (points, bottom left) to the page.
pub fn with_box(mut page: TestPage, rect: (f64, f64, f64, f64)) -> TestPage {
    let (x, y, w, h) = rect;
    writeln!(page.content, "0 g {x} {y} {w} {h} re f").unwrap();
    page
}

pub fn write(pages: &[TestPage]) -> Vec<u8> {
    let n = pages.len();
    // Objects: 1 catalog, 2 pages, 3 font, then a page and its content per page.
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Count {n} /Kids [{}] >>",
            (0..n)
                .map(|i| format!("{} 0 R", 4 + 2 * i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_owned(),
    ];
    for (i, p) in pages.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            p.size.0,
            p.size.1,
            5 + 2 * i
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            p.content.len() + 1,
            p.content
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}
