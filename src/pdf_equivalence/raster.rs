//! A page rendered to 8-bit RGB, and the tolerant pixel comparison of two renderings.

use crate::num::ToF64;

/// One pixel: red, green, blue, 255 each for white.
pub type Rgb = [u8; 3];

/// White.
pub const WHITE: Rgb = [255, 255, 255];

/// A rendered page: 8-bit RGB, row-major, at `dpi`.
#[derive(Clone, PartialEq, Eq)]
pub struct Raster {
    /// Pixels a row.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// The resolution it was rendered at.
    pub dpi: u32,
    /// `width × height` pixels, row by row from the top.
    pub pixels: Vec<Rgb>,
}

impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Raster({}x{} @ {} dpi)",
            self.width, self.height, self.dpi
        )
    }
}

impl Raster {
    /// A white page.
    pub fn blank(width: usize, height: usize, dpi: u32) -> Self {
        Self {
            width,
            height,
            dpi,
            pixels: vec![WHITE; width * height],
        }
    }

    fn at(&self, x: usize, y: usize) -> Rgb {
        self.pixels[y * self.width + x]
    }
}

/// How far apart two colours are: the largest difference of one channel (0..=255). A gray against a gray is
/// their difference in gray; a pastel against white is as far as its most saturated channel.
pub fn distance(p: Rgb, q: Rgb) -> u8 {
    (0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap_or(0)
}

/// A neighbourhood is flat when no channel varies in it by more than this. At the resolutions compared, the
/// inside of a fill or of a blank margin renders as one exact colour, while a glyph's or a line's edge, which
/// anti-aliasing draws differently from one renderer to the next, spans the range between ink and paper.
const FLAT_RANGE: u8 = 4;

/// The comparison's tolerances, in the units of [`distance`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tolerances {
    /// Two pixels match when their colours are at most this far apart; a pixel is ink when it is further
    /// than this from white.
    pub colour: u8,
    /// Where both renderings are flat around a pixel (a fill, shading, a blank area), its two colours differ
    /// when they are further apart than this, and the pixel is ink when it is further than this from white.
    pub fill: u8,
    /// A pixel may match any pixel this many pixels away on the other rendering; flatness is judged over the
    /// same square.
    pub radius: usize,
}

/// How two renderings of one page differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelDiff {
    /// Pixels that differ: no counterpart of a close colour within the radius on the other side, or another
    /// fill colour where both sides are flat.
    pub differing: usize,
    /// The page's ink on either side: pixels further from white than the colour tolerance, and pixels inside
    /// a flat area further from white than the fill tolerance.
    pub inked: usize,
    /// The differing pixels' bounding box in pixels, `(x0, y0, x1, y1)`, ends exclusive.
    pub bbox: Option<(usize, usize, usize, usize)>,
}

impl PixelDiff {
    /// The differing pixels as a share of the inked ones; 0 for two blank pages.
    pub fn fraction(&self) -> f64 {
        if self.inked == 0 {
            0.0
        } else {
            self.differing.to_f64() / self.inked.to_f64()
        }
    }
}

/// Compares two renderings over the area they share, by two rules.
///
/// - A pixel differs when the other rendering has no pixel within `radius` (a square neighbourhood) whose
///   colour is within the colour tolerance of it, looked at from both sides: a glyph moved by a pixel, or
///   anti-aliased a little differently, matches; a missing, added or displaced shape does not.
/// - A pixel also differs when both renderings are flat around it and its two colours are further apart than
///   the fill tolerance: a pastel fill or a light shading added, removed or recoloured. The colour tolerance
///   alone cannot see it, being as wide as anti-aliasing needs, and a fill's edge moved by a pixel is not
///   flat on both sides, so it stays drift.
pub fn diff(expected: &Raster, actual: &Raster, t: Tolerances) -> PixelDiff {
    let width = expected.width.min(actual.width);
    let height = expected.height.min(actual.height);
    let radius = t.radius;
    let window = |x: usize, y: usize| {
        let (x0, x1) = (x.saturating_sub(radius), (x + radius + 1).min(width));
        let (y0, y1) = (y.saturating_sub(radius), (y + radius + 1).min(height));
        (y0..y1).flat_map(move |qy| (x0..x1).map(move |qx| (qx, qy)))
    };
    let matched = |from: &Raster, to: &Raster, x: usize, y: usize| -> bool {
        let v = from.at(x, y);
        window(x, y).any(|(qx, qy)| distance(to.at(qx, qy), v) <= t.colour)
    };
    let flat = |r: &Raster, x: usize, y: usize| -> bool {
        let (mut lo, mut hi) = (WHITE, [0; 3]);
        for (qx, qy) in window(x, y) {
            let p = r.at(qx, qy);
            for c in 0..3 {
                lo[c] = lo[c].min(p[c]);
                hi[c] = hi[c].max(p[c]);
            }
        }
        (0..3).all(|c| hi[c] - lo[c] <= FLAT_RANGE)
    };
    let mut differing = 0;
    let mut inked = 0;
    let mut bbox: Option<(usize, usize, usize, usize)> = None;
    for y in 0..height {
        for x in 0..width {
            let (e, a) = (expected.at(x, y), actual.at(x, y));
            let (we, wa) = (distance(e, WHITE), distance(a, WHITE));
            let dark = we > t.colour || wa > t.colour;
            // Flatness only matters for pixels a fill could account for; most of a page is blank paper.
            let (fe, fa) = if dark || we > t.fill || wa > t.fill {
                (flat(expected, x, y), flat(actual, x, y))
            } else {
                (false, false)
            };
            if dark || (fe && we > t.fill) || (fa && wa > t.fill) {
                inked += 1;
            }
            let other_fill = fe && fa && distance(e, a) > t.fill;
            if other_fill || !matched(expected, actual, x, y) || !matched(actual, expected, x, y) {
                differing += 1;
                bbox = Some(match bbox {
                    None => (x, y, x + 1, y + 1),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
                });
            }
        }
    }
    PixelDiff {
        differing,
        inked,
        bbox,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_the_largest_channel_difference() {
        assert_eq!(distance([0, 0, 0], [0, 0, 0]), 0);
        assert_eq!(distance([10, 10, 10], [74, 74, 74]), 64);
        // tdf157011's tracked-change pink and blue, against white.
        assert_eq!(distance([252, 230, 244], WHITE), 25);
        assert_eq!(distance([225, 242, 250], WHITE), 30);
    }
}
