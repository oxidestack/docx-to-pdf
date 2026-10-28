//! The numeric conversions the cast lints flag and that have no lossless form, each named for what it
//! does so a call site says which rounding it relies on.
//!
//! - [`ToF64`]: an integer as the nearest `f64`. Exact up to 2^53, which bounds every count, size and duration this
//!   service handles (bytes of a document, pixels of a picture, nanoseconds of a run fit with room to spare); past
//!   it the value rounds to the nearest representable one, as the `as` it replaces did.
//! - `saturating_*`: an `f64` toward zero into an integer, clamped to the integer's range, NaN to 0 — the semantics of
//!   Rust's float-to-integer `as`, written as a function so the caller's rounding (`floor`, `ceil`, `round`) stays
//!   visible next to it.
//! - [`usize_from_u64`]: lossless, the service being built for 64-bit targets only (asserted at compile time).

/// An integer as the nearest `f64`; exact up to 2^53 (see the module docs).
pub trait ToF64 {
    /// The nearest `f64`.
    fn to_f64(self) -> f64;
}

impl ToF64 for usize {
    #[expect(
        clippy::cast_precision_loss,
        reason = "rounding to the nearest f64 is the conversion; exact up to 2^53 (tested below)"
    )]
    fn to_f64(self) -> f64 {
        self as f64
    }
}

impl ToF64 for u64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "rounding to the nearest f64 is the conversion; exact up to 2^53 (tested below)"
    )]
    fn to_f64(self) -> f64 {
        self as f64
    }
}

impl ToF64 for i32 {
    fn to_f64(self) -> f64 {
        f64::from(self)
    }
}

impl ToF64 for i64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "rounding to the nearest f64 is the conversion; exact within ±2^53 (tested below)"
    )]
    fn to_f64(self) -> f64 {
        self as f64
    }
}

// The service is built for linux/amd64 and linux/arm64 only: a `usize` holds every `u64`.
const _: () = assert!(usize::BITS == u64::BITS, "a usize must hold every u64");

/// A `u64` (a length or an offset read from a file, a configured size) as a `usize`; lossless, `usize` being 64 bits
/// wide on every target this builds for (asserted at compile time above).
#[expect(
    clippy::cast_possible_truncation,
    reason = "usize is 64 bits wide, asserted at compile time next to this function"
)]
pub const fn usize_from_u64(n: u64) -> usize {
    n as usize
}

/// `x` toward zero as a `u64`, clamped to `0..=u64::MAX`, NaN to 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the saturating float-to-integer `as` is the conversion (tested below)"
)]
pub const fn saturating_u64(x: f64) -> u64 {
    x as u64
}

/// `x` toward zero as an `i64`, clamped to `i64::MIN..=i64::MAX`, NaN to 0.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the saturating float-to-integer `as` is the conversion (tested below)"
)]
pub const fn saturating_i64(x: f64) -> i64 {
    x as i64
}

/// `x` toward zero as a `usize`, clamped to `0..=usize::MAX`, NaN to 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the saturating float-to-integer `as` is the conversion (tested below)"
)]
pub const fn saturating_usize(x: f64) -> usize {
    x as usize
}

/// `x` toward zero as a `u32`, clamped to `0..=u32::MAX`, NaN to 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the saturating float-to-integer `as` is the conversion (tested below)"
)]
pub const fn saturating_u32(x: f64) -> u32 {
    x as u32
}

/// `x` toward zero as a `u8`, clamped to `0..=255`, NaN to 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the saturating float-to-integer `as` is the conversion (tested below)"
)]
pub const fn saturating_u8(x: f64) -> u8 {
    x as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_53: u64 = 1 << 53;

    #[test]
    fn integers_are_exact_up_to_two_to_the_53() {
        assert_eq!(TWO_53.to_f64(), 9_007_199_254_740_992.0);
        assert_eq!((TWO_53 - 1).to_f64(), 9_007_199_254_740_991.0);
        assert_eq!(
            usize::try_from(TWO_53).unwrap().to_f64(),
            9_007_199_254_740_992.0
        );
        assert_eq!(
            i64::try_from(TWO_53).unwrap().to_f64(),
            9_007_199_254_740_992.0
        );
        assert_eq!(
            (-i64::try_from(TWO_53).unwrap()).to_f64(),
            -9_007_199_254_740_992.0
        );
        // Past 2^53 an odd value rounds to an even neighbour, as `as` does.
        assert_eq!((TWO_53 + 1).to_f64(), 9_007_199_254_740_992.0);
        assert_eq!(u64::MAX.to_f64(), 18_446_744_073_709_551_616.0);
    }

    #[test]
    fn every_u64_is_a_usize() {
        assert_eq!(usize_from_u64(u64::MAX), usize::MAX);
        assert_eq!(usize_from_u64(0), 0);
    }

    #[test]
    fn floats_go_toward_zero_and_saturate() {
        assert_eq!(saturating_u64(2.9), 2);
        assert_eq!(saturating_u64(-0.5), 0);
        assert_eq!(saturating_u64(-1e30), 0);
        assert_eq!(saturating_u64(1e30), u64::MAX);
        assert_eq!(saturating_u64(f64::NAN), 0);
        assert_eq!(saturating_i64(-2.9), -2);
        assert_eq!(saturating_i64(1e30), i64::MAX);
        assert_eq!(saturating_i64(-1e30), i64::MIN);
        assert_eq!(saturating_i64(f64::NAN), 0);
        assert_eq!(saturating_usize(7.99), 7);
        assert_eq!(saturating_usize(-3.0), 0);
        assert_eq!(saturating_usize(f64::INFINITY), usize::MAX);
        assert_eq!(saturating_u32(4_294_967_296.0), u32::MAX);
        assert_eq!(saturating_u32(-1.0), 0);
        assert_eq!(saturating_u8(255.9), 255);
        assert_eq!(saturating_u8(256.0), 255);
        assert_eq!(saturating_u8(-0.1), 0);
        assert_eq!(saturating_u8(f64::NAN), 0);
    }
}
