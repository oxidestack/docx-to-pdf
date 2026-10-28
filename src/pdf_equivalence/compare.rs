//! The comparison over facts already extracted from two PDFs: pure, so every rule of the verdict and the
//! report is testable without a PDF.

use super::raster::{Raster, Tolerances, diff};
use std::fmt;
use std::fmt::Write as _;

use crate::num::{ToF64, usize_from_u64};

/// What the comparison reads from one PDF.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// Its pages, in order.
    pub pages: Vec<Page>,
}

/// One page: its size in points (width, height), its extracted text, its rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Width and height in points.
    pub size: (f64, f64),
    /// The text `pdftotext` extracts.
    pub text: String,
    /// The page rendered at [`Thresholds::dpi`].
    pub raster: Raster,
}

/// The tolerances of the verdict. The defaults were calibrated on 18 corpus documents converted by
/// moalhaddar/docx-to-pdf (LibreOffice 7.3) and by LibreOffice 25.2 with the same fonts, and checked on 39 more and
/// on 26.8: the same version twice differs by no pixel; drift between versions
/// (text nudged by a pixel) stayed at or below 1.74% of the inked pixels, while the smallest difference
/// visible by eye measured 2.62% (2.49% in gray). At 50 dpi a radius of 2 pixels already hid a visible
/// difference. The fill tolerance makes fills and shading count: pastel shading lies 25 to 30 from white, inside the
/// colour tolerance, and with the fill rule tdf157011's shaded tracked changes measure 84.68% instead of 3.43%
/// while no verdict of the calibration set changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Thresholds {
    /// Two page sizes are equal when each side differs by at most this many points (1 pt is 0.35 mm:
    /// LibreOffice versions round a paper size differently, e.g. 297.64 against 297.01 pt).
    pub size_tolerance_pt: f64,
    /// The resolution pages are rendered at.
    pub dpi: u32,
    /// Two pixels match when no channel of their colours differs by more than this (0..=255); a pixel is
    /// ink when it is further than this from white.
    pub colour_tolerance: u8,
    /// Where both renderings are flat around a pixel (a fill, shading, blank paper), its colours differ when
    /// a channel differs by more than this, and it is ink when it is further than this from white.
    pub fill_tolerance: u8,
    /// A pixel may match any pixel this many pixels away on the other rendering.
    pub radius_px: usize,
    /// A page differs when more than this share of its inked pixels differ.
    pub max_differing_fraction: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            size_tolerance_pt: 1.0,
            dpi: 50,
            colour_tolerance: 64,
            fill_tolerance: 16,
            radius_px: 1,
            max_differing_fraction: 0.02,
        }
    }
}

/// One reason two PDFs are not equivalent. Pages are 1-based.
#[derive(Debug, Clone, PartialEq)]
pub enum Finding {
    /// The documents have different numbers of pages.
    PageCount {
        /// The expected document's pages.
        expected: usize,
        /// The actual document's pages.
        actual: usize,
    },
    /// A page's size differs past [`Thresholds::size_tolerance_pt`].
    PageSize {
        /// The page.
        page: usize,
        /// The expected page's width and height in points.
        expected: (f64, f64),
        /// The actual page's width and height in points.
        actual: (f64, f64),
    },
    /// The page's text, whitespace runs collapsed, differs first at character `offset`; `expected` and
    /// `actual` are the text around it on each side.
    Text {
        /// The page.
        page: usize,
        /// Where the texts first differ, in characters.
        offset: usize,
        /// The expected text around `offset`.
        expected: String,
        /// The actual text around `offset`.
        actual: String,
    },
    /// The document's words, compared without their order ([`document_words_difference`]), differ: `missing` are
    /// the expected words the actual document lacks, `extra` the actual's words the expected lacks, each sorted.
    Words {
        /// The expected words the actual document lacks.
        missing: Vec<String>,
        /// The actual document's words the expected lacks.
        extra: Vec<String>,
    },
    /// The renderings differ: `fraction` of the inked pixels have no match, more than `threshold`; the
    /// differing pixels lie within `bbox_pt`, `(x0, y0, x1, y1)` in points from the top left.
    Rendering {
        /// The page.
        page: usize,
        /// The share of inked pixels without a match.
        fraction: f64,
        /// [`Thresholds::max_differing_fraction`].
        threshold: f64,
        /// Where the differing pixels lie: `(x0, y0, x1, y1)` in points from the top left.
        bbox_pt: (f64, f64, f64, f64),
    },
}

/// The verdict and every reason behind it. `expected` is the reference (moalhaddar/docx-to-pdf's PDF), `actual`
/// this service's.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// The pages compared: the expected document's.
    pub pages: usize,
    /// Every reason the documents are not equivalent; none when they are.
    pub findings: Vec<Finding>,
}

impl Report {
    /// Whether nothing was found.
    pub fn is_equivalent(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Compares every fact of `actual` with `expected`'s and keeps every difference, not only the first: when
/// the page counts differ, the pages both have are still compared, so the report shows where they part.
pub fn compare(expected: &Document, actual: &Document, t: &Thresholds) -> Report {
    let mut findings = Vec::new();
    if expected.pages.len() != actual.pages.len() {
        findings.push(Finding::PageCount {
            expected: expected.pages.len(),
            actual: actual.pages.len(),
        });
    }
    for (i, (e, a)) in expected.pages.iter().zip(&actual.pages).enumerate() {
        let page = i + 1;
        let same = same_size(e.size, a.size, t.size_tolerance_pt);
        if !same {
            findings.push(Finding::PageSize {
                page,
                expected: e.size,
                actual: a.size,
            });
        }
        if let Some(finding) = text_difference(page, &e.text, &a.text) {
            findings.push(finding);
        }
        // Renderings of pages of different sizes differ by construction; the size finding says why.
        if same && let Some(finding) = rendering_difference(page, &e.raster, &a.raster, t) {
            findings.push(finding);
        }
    }
    Report {
        pages: expected.pages.len(),
        findings,
    }
}

/// Characters of context shown on each side of the first text difference.
const TEXT_CONTEXT: usize = 16;

/// Text is compared with every whitespace run collapsed to one space: line breaks and spacing are layout,
/// which the rendering check judges, while a changed, missing or extra character is content. The order of a
/// formula's characters is extraction, not content ([`first_untolerated_segment`]); a finding names the first
/// difference that is not one.
fn text_difference(page: usize, expected: &str, actual: &str) -> Option<Finding> {
    let e = normalise_text(expected);
    let a = normalise_text(actual);
    if e == a {
        return None;
    }
    let (from_e, from_a) = first_untolerated_segment(&e, &a)?;
    let (e_rest, a_rest) = (&e[from_e..], &a[from_a..]);
    let k = e_rest
        .iter()
        .zip(a_rest)
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| e_rest.len().min(a_rest.len()));
    let excerpt = |chars: &[char], at: usize| -> String {
        let start = at.saturating_sub(TEXT_CONTEXT);
        let end = (at + TEXT_CONTEXT).min(chars.len());
        chars[start.min(end)..end].iter().collect()
    };
    Some(Finding::Text {
        page,
        offset: from_e + k,
        expected: excerpt(&e, from_e + k),
        actual: excerpt(&a, from_a + k),
    })
}

/// Characters a tab leader repeats to fill its tab: how many fit is layout.
const LEADERS: &[char] = &['.', '_', '-', '\u{b7}', '\u{2026}'];

/// The words of the whole document compared as a multiset, for a reference from another LibreOffice version:
/// its layout may fit a line more or less on a page, which moves every later page break, and may
/// place a floating table, a footnote or a text box elsewhere in the extracted flow. Neither changes what the
/// document says; a changed, missing or extra word does. The page count says whether the breaks moved.
pub fn document_words_difference(expected: &Document, actual: &Document) -> Option<Finding> {
    let words = |d: &Document| {
        let mut count: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
        for p in &d.pages {
            let text: String = normalise_text(&p.text).into_iter().collect();
            for w in text.split(' ').filter(|w| !w.is_empty()) {
                *count.entry(w.to_owned()).or_default() += 1;
            }
        }
        count
    };
    let (mut e, a) = (words(expected), words(actual));
    for (w, n) in a {
        *e.entry(w).or_default() -= n;
    }
    let (mut missing, mut extra) = (Vec::new(), Vec::new());
    for (w, n) in e {
        let side = if n > 0 { &mut missing } else { &mut extra };
        side.extend(std::iter::repeat_n(w, usize_from_u64(n.unsigned_abs())));
    }
    (!missing.is_empty() || !extra.is_empty()).then_some(Finding::Words { missing, extra })
}

/// How many words of each side a [`Finding::Words`] shows.
const WORDS_SHOWN: usize = 12;

/// Whitespace runs become one space; a run of three or more of one leader character becomes three.
fn normalise_text(text: &str) -> Vec<char> {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: Vec<char> = Vec::with_capacity(joined.len());
    for c in joined.chars() {
        let n = out.len();
        if LEADERS.contains(&c) && n >= 3 && out[n - 3..].iter().all(|&p| p == c) {
            continue;
        }
        out.push(c);
    }
    out
}

/// `pdftotext` extracts a formula's characters in reading order by position, so an exponent, a limit or a
/// bracket lands before or after its neighbours depending on where a LibreOffice version places it by a
/// pixel (`"p+1 ± 5 4"` against `"±5 p+1 4"`). Poppler marks no formula, and the font
/// does not either (LibreOffice 26.8 draws formula letters and operators in the body font), so a formula is
/// recognised in the text alone:
///
/// - a whitespace-separated token holding a run of [`WORD_LETTERS`] or more letters is a *word*; the words of
///   both texts must be equal and in the same order;
/// - the tokens between two consecutive words (or before the first, after the last) form a *segment*; a
///   segment holding a math symbol ([`is_math_symbol`]) on both sides is a formula, compared as a multiset of
///   its non-whitespace characters; every other segment must be equal token for token.
///
/// So a formula's characters may come in any order and with any spacing, while a character changed, missing
/// or added in it still differs, as does a character moved across a word. The limits: a formula with a
/// function name of three letters or more (`sin`, `mod`) is split by it into two formulas, each compared on
/// its own; and the order of digits and letters is not checked inside any run of short tokens that holds a
/// math symbol, formula or not (`1 + 2 = 3` equals `3 = 2 + 1`).
///
/// Returns where the first segment that is not equal this way starts in each text, or `None` when every
/// segment is.
fn first_untolerated_segment(expected: &[char], actual: &[char]) -> Option<(usize, usize)> {
    let (e, a) = (segments(expected), segments(actual));
    for (es, asg) in e.iter().zip(&a) {
        let tolerated = es.word == asg.word
            && (es.tokens == asg.tokens || is_formula_pair(&es.tokens, &asg.tokens));
        if !tolerated {
            return Some((es.start, asg.start));
        }
    }
    // Texts with different numbers of words differ at the latest in the shorter one's last segment, which
    // ends without a word where the other has one.
    None
}

/// Letters in a row that make a token a word rather than part of a formula: formula variables are one or
/// two letters (`x`, `nx`), while words of prose are mostly longer.
const WORD_LETTERS: usize = 3;

/// The non-word tokens between two words, and the word that ends them (none for the last segment).
/// `start` is the character offset in the normalised text where the segment begins: 0, or the space after the
/// previous word.
struct Segment {
    start: usize,
    tokens: Vec<String>,
    word: Option<String>,
}

/// The normalised text (single spaces) split at its words.
fn segments(text: &[char]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut current = Segment {
        start: 0,
        tokens: Vec::new(),
        word: None,
    };
    let mut at = 0;
    for token in text.split(|&c| c == ' ') {
        let (end, next) = (at + token.len(), at + token.len() + 1);
        if !token.is_empty() {
            let token: String = token.iter().collect();
            if is_word(&token) {
                current.word = Some(token);
                // The next segment starts at the space after the word, so text that ends there differs there.
                let start = end;
                out.push(std::mem::replace(
                    &mut current,
                    Segment {
                        start,
                        tokens: Vec::new(),
                        word: None,
                    },
                ));
            } else {
                current.tokens.push(token);
            }
        }
        at = next;
    }
    out.push(current);
    out
}

fn is_word(token: &str) -> bool {
    let mut letters = 0;
    for c in token.chars() {
        letters = if c.is_alphabetic() { letters + 1 } else { 0 };
        if letters >= WORD_LETTERS {
            return true;
        }
    }
    false
}

fn is_formula_pair(expected: &[String], actual: &[String]) -> bool {
    let chars = |tokens: &[String]| -> Vec<char> {
        let mut v: Vec<char> = tokens.iter().flat_map(|t| t.chars()).collect();
        v.sort_unstable();
        v
    };
    let has_math = |tokens: &[String]| tokens.iter().flat_map(|t| t.chars()).any(is_math_symbol);
    has_math(expected) && has_math(actual) && chars(expected) == chars(actual)
}

/// A mathematical operator or relation, as a formula has and prose mostly does not. The hyphen-minus is not
/// one: it joins words and ranges.
fn is_math_symbol(c: char) -> bool {
    matches!(c, '+' | '<' | '=' | '>' | '±' | '×' | '÷' | '¬')
        || matches!(c as u32, 0x2200..=0x22ff | 0x2a00..=0x2aff | 0x27c0..=0x27ef | 0x2980..=0x29ff)
}

fn rendering_difference(page: usize, e: &Raster, a: &Raster, t: &Thresholds) -> Option<Finding> {
    let tolerances = Tolerances {
        colour: t.colour_tolerance,
        fill: t.fill_tolerance,
        radius: t.radius_px,
    };
    let d = diff(e, a, tolerances);
    let fraction = d.fraction();
    if fraction <= t.max_differing_fraction {
        return None;
    }
    let pt = |px: usize| px.to_f64() * 72.0 / f64::from(e.dpi);
    let (x0, y0, x1, y1) = d.bbox.unwrap_or_default();
    Some(Finding::Rendering {
        page,
        fraction,
        threshold: t.max_differing_fraction,
        bbox_pt: (pt(x0), pt(y0), pt(x1), pt(y1)),
    })
}

fn same_size(e: (f64, f64), a: (f64, f64), tolerance: f64) -> bool {
    (e.0 - a.0).abs() <= tolerance && (e.1 - a.1).abs() <= tolerance
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageCount { expected, actual } => {
                write!(f, "page count: expected {expected}, actual {actual}")
            }
            Self::PageSize {
                page,
                expected: (ew, eh),
                actual: (aw, ah),
            } => write!(
                f,
                "page {page} size: expected {ew:.2} x {eh:.2} pt, actual {aw:.2} x {ah:.2} pt"
            ),
            Self::Words { missing, extra } => {
                let shown = |w: &[String]| {
                    let mut s = format!("{:?}", &w[..w.len().min(WORDS_SHOWN)]);
                    if w.len() > WORDS_SHOWN {
                        let _ = write!(s, " and {} more", w.len() - WORDS_SHOWN);
                    }
                    s
                };
                write!(
                    f,
                    "the document's words differ: missing {}, extra {}",
                    shown(missing),
                    shown(extra)
                )
            }
            Self::Text {
                page,
                offset,
                expected,
                actual,
            } => write!(
                f,
                "page {page} text differs at character {offset}: expected {expected:?}, actual {actual:?}"
            ),
            Self::Rendering {
                page,
                fraction,
                threshold,
                bbox_pt: (x0, y0, x1, y1),
            } => write!(
                f,
                "page {page} rendering: {:.2}% of inked pixels differ (threshold {:.2}%), \
                 within x {x0:.0}..{x1:.0} pt, y {y0:.0}..{y1:.0} pt from the top left",
                fraction * 100.0,
                threshold * 100.0
            ),
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_equivalent() {
            let s = if self.pages == 1 { "" } else { "s" };
            return write!(f, "EQUIVALENT: {} page{s}", self.pages);
        }
        let n = self.findings.len();
        let s = if n == 1 { "" } else { "s" };
        write!(f, "NOT EQUIVALENT: {n} finding{s}")?;
        for finding in &self.findings {
            write!(f, "\n- {finding}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf_equivalence::raster::{Rgb, WHITE};

    const A4: (f64, f64) = (595.28, 841.89);
    const LETTER: (f64, f64) = (612.0, 792.0);

    fn page(size: (f64, f64), text: &str) -> Page {
        rendered(size, text, Raster::blank(60, 80, 72))
    }

    fn rendered(size: (f64, f64), text: &str, raster: Raster) -> Page {
        Page {
            size,
            text: text.to_owned(),
            raster,
        }
    }

    /// A 60 x 80 px page at 72 dpi (a pixel is a point) with black boxes `(x, y, w, h)` on it.
    fn boxes(rects: &[(usize, usize, usize, usize)]) -> Raster {
        let mut r = Raster::blank(60, 80, 72);
        for &(x, y, w, h) in rects {
            for yy in y..y + h {
                for xx in x..x + w {
                    r.pixels[yy * 60 + xx] = [0; 3];
                }
            }
        }
        r
    }

    fn one_page(raster: Raster) -> Document {
        doc(vec![rendered((60.0, 80.0), "t", raster)])
    }

    #[test]
    fn a_shape_moved_by_one_pixel_is_rendering_drift_not_a_difference() {
        let e = one_page(boxes(&[(10, 10, 20, 10)]));
        let a = one_page(boxes(&[(11, 9, 20, 10)]));
        assert!(run(&e, &a).is_equivalent());
    }

    #[test]
    fn anti_aliasing_within_the_colour_tolerance_is_not_a_difference() {
        let e = one_page(boxes(&[(10, 10, 20, 10)]));
        // The box's edge drawn lighter and tinted, as another renderer's anti-aliasing may draw it.
        let mut a = boxes(&[(10, 10, 20, 10)]);
        for y in 10..20 {
            for x in 10..30 {
                if x == 10 || x == 29 || y == 10 || y == 19 {
                    a.pixels[y * 60 + x] = [40, 30, 50];
                }
            }
        }
        assert!(run(&e, &one_page(a)).is_equivalent());
    }

    #[test]
    fn a_solid_shape_recoloured_is_a_rendering_finding() {
        let e = one_page(boxes(&[(10, 10, 20, 10)]));
        let mut a = boxes(&[(10, 10, 20, 10)]);
        for v in a.pixels.iter_mut().filter(|v| **v == [0; 3]) {
            *v = [0, 0, 48];
        }
        assert!(!run(&e, &one_page(a)).is_equivalent());
    }

    #[test]
    fn a_shape_moved_by_ten_pixels_is_a_rendering_finding_with_where_and_how_much() {
        let e = one_page(boxes(&[(10, 10, 20, 10), (10, 50, 40, 10)]));
        let a = one_page(boxes(&[(10, 20, 20, 10), (10, 50, 40, 10)]));
        let r = run(&e, &a);
        let [
            Finding::Rendering {
                page,
                fraction,
                threshold,
                bbox_pt,
            },
        ] = r.findings.as_slice()
        else {
            panic!("{r}");
        };
        assert_eq!(*page, 1);
        // Inked: 20x20 (both boxes' union) + 40x10 = 800; differing: the 2 x 20x10 strips that do not overlap
        // after the 1-pixel radius is allowed (rows 10..19 vs 20..29: 9 + 9 rows) = 18 x 20 = 360.
        assert!((fraction - 360.0 / 800.0).abs() < 1e-9, "{fraction}");
        assert_eq!(*threshold, Thresholds::default().max_differing_fraction);
        assert_eq!(*bbox_pt, (10.0, 10.0, 30.0, 30.0));
        assert_eq!(
            r.findings[0].to_string(),
            "page 1 rendering: 45.00% of inked pixels differ (threshold 2.00%), within x 10..30 pt, y 10..30 pt from the top left"
        );
    }

    /// The pink tdf157011's tracked changes shade their cells with in LibreOffice 26.8.
    const PASTEL_PINK: Rgb = [252, 230, 244];

    /// `boxes`' text with a cell `(x, y, w, h)` shaded in `colour` behind it.
    #[allow(
        clippy::many_single_char_names,
        reason = "the formula's own names (x, y, w, h; r, g, b; the DCT's indices)"
    )]
    fn shaded(cell: (usize, usize, usize, usize), colour: Rgb) -> Raster {
        let mut r = boxes(&[(10, 10, 20, 4), (10, 30, 30, 4)]);
        let (x, y, w, h) = cell;
        for yy in y..y + h {
            for xx in x..x + w {
                if r.pixels[yy * 60 + xx] == WHITE {
                    r.pixels[yy * 60 + xx] = colour;
                }
            }
        }
        r
    }

    #[test]
    fn a_pastel_shaded_cell_is_a_rendering_finding_whose_box_covers_the_cell() {
        let e = one_page(shaded((0, 0, 0, 0), PASTEL_PINK));
        let a = one_page(shaded((5, 25, 40, 15), PASTEL_PINK));
        let r = run(&e, &a);
        let [Finding::Rendering { bbox_pt, .. }] = r.findings.as_slice() else {
            panic!("{r}");
        };
        // The cell's interior differs; its outermost ring touches white on one side and is drift-tolerant.
        assert_eq!(*bbox_pt, (6.0, 26.0, 44.0, 39.0));
    }

    #[test]
    fn a_pastel_shaded_cell_moved_by_one_pixel_is_drift_not_a_difference() {
        let e = one_page(shaded((5, 25, 40, 15), PASTEL_PINK));
        let a = one_page(shaded((6, 24, 40, 15), PASTEL_PINK));
        assert!(run(&e, &a).is_equivalent());
    }

    #[test]
    fn a_page_of_another_size_is_not_also_reported_as_a_rendering_difference() {
        let e = doc(vec![rendered(
            (60.0, 80.0),
            "t",
            boxes(&[(10, 10, 20, 10)]),
        )]);
        let a = doc(vec![rendered((80.0, 60.0), "t", Raster::blank(80, 60, 72))]);
        let r = run(&e, &a);
        assert!(
            matches!(r.findings.as_slice(), [Finding::PageSize { .. }]),
            "{r}"
        );
    }

    fn doc(pages: Vec<Page>) -> Document {
        Document { pages }
    }

    fn run(expected: &Document, actual: &Document) -> Report {
        compare(expected, actual, &Thresholds::default())
    }

    #[test]
    fn the_same_facts_are_equivalent() {
        let d = doc(vec![page(A4, "Hello"), page(A4, "World")]);
        let r = run(&d, &d.clone());
        assert!(r.is_equivalent(), "{r}");
        assert_eq!(r.to_string(), "EQUIVALENT: 2 pages");
    }

    #[test]
    fn a_missing_page_is_a_page_count_finding() {
        let e = doc(vec![page(A4, "a"), page(A4, "b"), page(A4, "c")]);
        let a = doc(vec![page(A4, "a"), page(A4, "b")]);
        let r = run(&e, &a);
        assert_eq!(
            r.findings,
            vec![Finding::PageCount {
                expected: 3,
                actual: 2
            }]
        );
        assert_eq!(
            r.to_string(),
            "NOT EQUIVALENT: 1 finding\n- page count: expected 3, actual 2"
        );
    }

    #[test]
    fn a_page_of_another_size_is_named_with_both_sizes() {
        let e = doc(vec![page(A4, "a"), page(A4, "b")]);
        let a = doc(vec![page(A4, "a"), page(LETTER, "b")]);
        let r = run(&e, &a);
        assert_eq!(
            r.findings,
            vec![Finding::PageSize {
                page: 2,
                expected: A4,
                actual: LETTER
            }]
        );
        assert_eq!(
            r.findings[0].to_string(),
            "page 2 size: expected 595.28 x 841.89 pt, actual 612.00 x 792.00 pt"
        );
    }

    #[test]
    fn a_size_within_the_tolerance_is_equal() {
        let e = doc(vec![page((595.28, 841.89), "a")]);
        let a = doc(vec![page((595.3, 841.9), "a")]);
        assert!(run(&e, &a).is_equivalent());
    }

    #[test]
    fn a_page_size_rounded_differently_by_another_libreoffice_is_equal() {
        // A6 landscape: LibreOffice 7.3 writes 297.64 pt, 25.2 writes 297.01 pt (seen in the calibration).
        let e = doc(vec![page((419.56, 297.64), "a")]);
        let a = doc(vec![page((419.56, 297.01), "a")]);
        assert!(run(&e, &a).is_equivalent());
        let b = doc(vec![page((419.56, 295.5), "a")]);
        assert!(!run(&e, &b).is_equivalent());
    }

    #[test]
    fn a_tab_leader_of_another_length_is_equal() {
        let e = doc(vec![page(A4, "Heading1................2\nHeading2......3")]);
        let a = doc(vec![page(
            A4,
            "Heading1...................2\nHeading2.........3",
        )]);
        assert!(run(&e, &a).is_equivalent());
        let b = doc(vec![page(
            A4,
            "Heading1...................4\nHeading2.........3",
        )]);
        assert!(!run(&e, &b).is_equivalent());
    }

    #[test]
    fn text_that_differs_only_in_whitespace_is_equal() {
        let e = doc(vec![page(A4, "Lorem ipsum dolor\nsit amet,\n\n")]);
        let a = doc(vec![page(A4, "Lorem  ipsum\ndolor sit   amet,")]);
        assert!(run(&e, &a).is_equivalent());
    }

    #[test]
    fn a_changed_word_is_a_text_finding_at_its_offset_with_context() {
        let e = doc(vec![
            page(A4, "x"),
            page(A4, "The quick brown fox jumps over the lazy dog"),
        ]);
        let a = doc(vec![
            page(A4, "x"),
            page(A4, "The quick brown cat jumps over the lazy dog"),
        ]);
        let r = run(&e, &a);
        assert_eq!(
            r.findings,
            vec![Finding::Text {
                page: 2,
                offset: 16,
                expected: "The quick brown fox jumps over t".to_owned(),
                actual: "The quick brown cat jumps over t".to_owned(),
            }]
        );
        assert_eq!(
            r.findings[0].to_string(),
            "page 2 text differs at character 16: expected \"The quick brown fox jumps over t\", actual \"The quick brown cat jumps over t\""
        );
    }

    #[test]
    fn the_document_words_are_equal_where_only_the_page_breaks_moved() {
        // text-10p: LibreOffice 26.8 fits one line more on a page than 7.3.
        let e = doc(vec![page(A4, "one two\nthree"), page(A4, "four five")]);
        let a = doc(vec![page(A4, "one two three\nfour"), page(A4, "five")]);
        assert!(!run(&e, &a).is_equivalent());
        assert_eq!(document_words_difference(&e, &a), None);
    }

    #[test]
    fn the_document_words_are_equal_where_a_floating_table_is_extracted_elsewhere() {
        // report-text-5p: the table's cells come before the paragraph on one side, after it on the other.
        let e = doc(vec![page(A4, "Results. 3.08 732.04 The sample")]);
        let a = doc(vec![page(A4, "Results. The sample 3.08 732.04")]);
        assert_eq!(document_words_difference(&e, &a), None);
    }

    #[test]
    fn a_changed_word_is_one_missing_and_one_extra() {
        let e = doc(vec![page(A4, "one two"), page(A4, "three four four")]);
        let a = doc(vec![page(A4, "one two three"), page(A4, "for four")]);
        let finding = document_words_difference(&e, &a).expect("a difference");
        assert_eq!(
            finding,
            Finding::Words {
                missing: vec!["four".to_owned()],
                extra: vec!["for".to_owned()],
            }
        );
        assert_eq!(
            finding.to_string(),
            "the document's words differ: missing [\"four\"], extra [\"for\"]"
        );
    }

    fn text_equal(expected: &str, actual: &str) -> bool {
        run(&doc(vec![page(A4, expected)]), &doc(vec![page(A4, actual)])).is_equivalent()
    }

    #[test]
    fn a_formula_whose_characters_are_extracted_in_another_order_is_equal() {
        // tableCurrupt, moalhaddar/docx-to-pdf against LibreOffice 25.2.
        assert!(text_equal(
            "has two square roots p+1 ± 5 4 mod p, which",
            "has two square roots ±5 p+1 4 mod p, which"
        ));
        // EquationWithAboveAndBelowCaption, moalhaddar/docx-to-pdf against 25.2: an exponent pulled to another line.
        assert!(text_equal(
            "Equation 1\n\nEquation 2\n\n( 1+ x )n=1+\n\nnx n ( n−1 ) x 2\n+\n+…\n1!\n2!",
            "Equation 1\n\nEquation 2\nn\n\n( 1+ x ) =1+\n\n2\n\nnx n ( n−1 ) x\n+\n+…\n1!\n2!"
        ));
        // strict, moalhaddar/docx-to-pdf against 26.8: spacing inside the formula.
        assert!(text_equal(
            "This is math:\n\nA=π r\n\n2",
            "This is math:\n\nA = π r2"
        ));
    }

    #[test]
    fn a_changed_character_inside_a_formula_is_a_text_finding() {
        let e = doc(vec![page(A4, "has two square roots p+1 ± 5 4 mod p")]);
        let a = doc(vec![page(A4, "has two square roots ±5 p+1 3 mod p")]);
        let r = run(&e, &a);
        assert!(
            matches!(r.findings.as_slice(), [Finding::Text { page: 1, .. }]),
            "{r}"
        );
        // EquationWithAboveAndBelowCaption on 26.8 draws the minus as a hyphen: a changed character.
        assert!(!text_equal("x k an−k k=0 k", "x k a n - k k =0 k"));
    }

    #[test]
    fn a_missing_or_added_character_inside_a_formula_is_a_text_finding() {
        assert!(!text_equal("roots p+1 ± 5 4 mod p", "roots ±5 p+1 mod p"));
        assert!(!text_equal(
            "roots p+1 ± 5 4 mod p",
            "roots ±5 p+1 4 4 mod p"
        ));
    }

    #[test]
    fn a_text_finding_after_a_reordered_formula_names_the_difference_not_the_formula() {
        // tableCurrupt, moalhaddar/docx-to-pdf against 25.2: the formula is tolerated, the TOC entry after it is not.
        let e = doc(vec![page(A4, "roots √ −1 mod p ... 5. Conclusions")]);
        let a = doc(vec![page(A4, "roots √−1 mod p 5. ... Conclusions")]);
        let r = run(&e, &a);
        assert_eq!(
            r.findings,
            vec![Finding::Text {
                page: 1,
                offset: 17,
                expected: "oots √ −1 mod p ... 5. Conclusio".to_owned(),
                actual: "roots √−1 mod p 5. ... Conclusio".to_owned(),
            }],
            "{r}"
        );
    }

    #[test]
    fn reordered_words_are_a_text_finding_even_next_to_a_formula() {
        // tdf149089: content reordered, no formula.
        assert!(!text_equal("LibreOffice 测试文档", "测试文档 LibreOffice"));
        assert!(!text_equal("total = alpha beta", "total = beta alpha"));
        // A formula's characters moved past a word belong to another formula.
        assert!(!text_equal("x + 1 then y", "x + then 1 y"));
    }

    #[test]
    fn reordered_characters_without_a_math_symbol_are_a_text_finding() {
        // tdf142407: CJK punctuation in another order.
        assert!(!text_equal("、 、 。 、 。", "、。 、。 、"));
        assert!(!text_equal("pages 1 2 3 here", "pages 3 2 1 here"));
    }

    #[test]
    fn text_cut_short_differs_where_it_ends() {
        let e = doc(vec![page(A4, "alpha beta gamma")]);
        let a = doc(vec![page(A4, "alpha beta")]);
        let r = run(&e, &a);
        assert_eq!(
            r.findings,
            vec![Finding::Text {
                page: 1,
                offset: 10,
                expected: "alpha beta gamma".to_owned(),
                actual: "alpha beta".to_owned(),
            }]
        );
    }
}
