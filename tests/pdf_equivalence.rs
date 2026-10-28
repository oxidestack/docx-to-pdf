//! PDF equivalence end to end: PDFs written by the test, read through poppler (`pdfinfo`, `pdftotext`,
//! `pdftoppm` on PATH), compared. Needs poppler-utils installed.

mod support;

use docx_to_pdf::pdf_equivalence::{Finding, Thresholds, compare_bytes};
use support::pdf::{A4, LETTER, TestPage, text_page, with_box, write};

fn letter_text() -> Vec<(f64, f64, &'static str)> {
    vec![
        (72.0, 770.0, "Dear reader,"),
        (72.0, 740.0, "The quick brown fox jumps over the lazy dog."),
        (72.0, 720.0, "Pack my box with five dozen liquor jugs."),
    ]
}

fn two_pages() -> Vec<TestPage> {
    vec![
        text_page(A4, &letter_text()),
        with_box(
            text_page(A4, &[(72.0, 770.0, "Page two")]),
            (72.0, 500.0, 200.0, 100.0),
        ),
    ]
}

fn compare(expected: &[TestPage], actual: &[TestPage]) -> docx_to_pdf::pdf_equivalence::Report {
    compare_bytes(&write(expected), &write(actual), &Thresholds::default())
        .expect("poppler reads both")
}

#[test]
fn the_same_document_is_equivalent() {
    let r = compare(&two_pages(), &two_pages());
    assert!(r.is_equivalent(), "{r}");
    assert_eq!(r.to_string(), "EQUIVALENT: 2 pages");
}

#[test]
fn text_moved_by_a_fraction_of_a_point_is_equivalent() {
    let shifted: Vec<_> = letter_text()
        .into_iter()
        .map(|(x, y, t)| (x + 0.4, y - 0.3, t))
        .collect();
    let mut actual = two_pages();
    actual[0] = text_page(A4, &shifted);
    let r = compare(&two_pages(), &actual);
    assert!(r.is_equivalent(), "{r}");
}

#[test]
fn an_extra_page_is_a_page_count_finding() {
    let mut actual = two_pages();
    actual.push(text_page(A4, &[(72.0, 770.0, "Page three")]));
    let r = compare(&two_pages(), &actual);
    assert_eq!(
        r.findings,
        vec![Finding::PageCount {
            expected: 2,
            actual: 3
        }],
        "{r}"
    );
}

#[test]
fn a_letter_page_for_an_a4_page_is_a_page_size_finding() {
    let mut actual = two_pages();
    actual[0] = text_page(LETTER, &letter_text());
    let r = compare(&two_pages(), &actual);
    assert_eq!(
        r.findings,
        vec![Finding::PageSize {
            page: 1,
            expected: A4,
            actual: LETTER
        }],
        "{r}"
    );
}

#[test]
fn a_changed_word_is_a_text_finding() {
    let mut changed = letter_text();
    changed[1].2 = "The quick brown cat jumps over the lazy dog.";
    let mut actual = two_pages();
    actual[0] = text_page(A4, &changed);
    let r = compare(&two_pages(), &actual);
    assert!(
        r.findings.iter().any(|f| matches!(
            f,
            Finding::Text { page: 1, expected, actual, .. }
                if expected.contains("fox") && actual.contains("cat")
        )),
        "{r}"
    );
}

/// A page with a formula drawn as LibreOffice draws one: its exponent a separate piece of text, raised by
/// `rise` points, which decides where `pdftotext` puts it in the extracted text.
fn formula_page(exponent: &str, rise: f64) -> TestPage {
    text_page(
        A4,
        &[
            (72.0, 770.0, "This is math:"),
            (72.0, 740.0, "A = p r"),
            (115.0, 740.0 + rise, exponent),
            (72.0, 700.0, "The end."),
        ],
    )
}

#[test]
fn a_formula_whose_characters_are_extracted_in_another_order_is_equivalent() {
    // Raised a little the exponent stays on the formula's line; raised a lot it gets its own line, before.
    let (e, a) = (
        write(&[formula_page("2", 4.0)]),
        write(&[formula_page("2", 14.0)]),
    );
    let text = |pdf: &[u8]| {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.pdf");
        std::fs::write(&p, pdf).unwrap();
        let out = std::process::Command::new("pdftotext")
            .args([p.as_os_str(), "-".as_ref()])
            .output()
            .unwrap_or_else(|e| {
                panic!(
                    "pdftotext could not be started ({e}); this test needs poppler-utils on PATH"
                )
            });
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_ne!(
        text(&e),
        text(&a),
        "the test must reorder the extracted text"
    );
    let r = compare_bytes(
        &e,
        &a,
        &Thresholds {
            max_differing_fraction: 1.0,
            ..Thresholds::default()
        },
    )
    .expect("poppler reads both");
    assert!(r.is_equivalent(), "{r}");
    let changed = compare(&[formula_page("2", 4.0)], &[formula_page("3", 14.0)]);
    assert!(
        changed
            .findings
            .iter()
            .any(|f| matches!(f, Finding::Text { page: 1, .. })),
        "{changed}"
    );
}

#[test]
fn a_moved_picture_is_a_rendering_finding_on_its_page_with_where() {
    let mut actual = two_pages();
    actual[1] = with_box(
        text_page(A4, &[(72.0, 770.0, "Page two")]),
        (72.0, 400.0, 200.0, 100.0),
    );
    let r = compare(&two_pages(), &actual);
    let [
        Finding::Rendering {
            page: 2,
            fraction,
            bbox_pt: (x0, y0, x1, y1),
            ..
        },
    ] = r.findings.as_slice()
    else {
        panic!("{r}");
    };
    assert!(*fraction > 0.5, "{r}");
    // The box spans y 242..342 pt from the top (A4 is 841.89 high) on one side and 342..442 on the other.
    assert!((x0 - 72.0).abs() < 3.0 && (x1 - 272.0).abs() < 3.0, "{r}");
    assert!((y0 - 242.0).abs() < 3.0 && (y1 - 442.0).abs() < 3.0, "{r}");
}

/// Adds a rectangle `(x, y, w, h)` filled with an RGB colour (0..=1 each) to the page, drawn before its text,
/// as a shaded table cell is.
#[allow(
    clippy::many_single_char_names,
    reason = "the formula's own names (x, y, w, h; r, g, b; the DCT's indices)"
)]
fn shaded(mut page: TestPage, rect: (f64, f64, f64, f64), rgb: (f64, f64, f64)) -> TestPage {
    let (x, y, w, h) = rect;
    let (r, g, b) = rgb;
    page.content = format!("{r} {g} {b} rg {x} {y} {w} {h} re f\n{}", page.content);
    page
}

/// The pink tdf157011's tracked changes shade their cells with in LibreOffice 26.8 (252, 230, 244).
const PASTEL_PINK: (f64, f64, f64) = (0.988, 0.902, 0.957);

#[test]
fn a_pastel_shaded_cell_is_a_rendering_finding_with_the_cell_in_its_box() {
    let mut actual = two_pages();
    actual[0] = shaded(
        text_page(A4, &letter_text()),
        (66.0, 712.0, 300.0, 40.0),
        PASTEL_PINK,
    );
    let r = compare(&two_pages(), &actual);
    let [
        Finding::Rendering {
            page: 1,
            bbox_pt: (x0, y0, x1, y1),
            ..
        },
    ] = r.findings.as_slice()
    else {
        panic!("{r}");
    };
    // The cell spans y 89.89..129.89 pt from the top; the box reaches into it from every side.
    assert!(
        *x0 < 80.0 && *x1 > 350.0 && *y0 < 100.0 && *y1 > 120.0,
        "{r}"
    );
}

#[test]
fn a_pastel_shaded_cell_moved_by_a_fraction_of_a_point_is_equivalent() {
    let cell = |dx: f64, dy: f64| {
        shaded(
            text_page(A4, &letter_text()),
            (66.0 + dx, 712.0 + dy, 300.0, 40.0),
            PASTEL_PINK,
        )
    };
    let mut expected = two_pages();
    expected[0] = cell(0.0, 0.0);
    let mut actual = two_pages();
    actual[0] = cell(0.4, -0.3);
    let r = compare(&expected, &actual);
    assert!(r.is_equivalent(), "{r}");
}

#[test]
fn bytes_that_are_not_a_pdf_are_an_error_naming_the_tool() {
    let err =
        compare_bytes(b"not a pdf", &write(&two_pages()), &Thresholds::default()).unwrap_err();
    assert!(err.to_string().contains("pdfinfo"), "{err}");
}
