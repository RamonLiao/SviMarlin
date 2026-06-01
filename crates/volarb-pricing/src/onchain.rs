//! L0 on-chain pricing port — integer fixed-point mirror of DeepBook Predict's
//! `oracle::compute_price` path. Pure `u64`/`u128`, zero IO, zero external deps.
//!
//! GOLDEN-TESTED FOR SELF-CONSISTENCY ONLY. **NOT yet parity-verified against the live
//! chain** — the tick-exact L3 parity harness against on-chain `compute_price` is Part 2
//! (`docs/specs/2026-06-01-onchain-pricing-l0-port-design.md`). Do not read passing tests
//! here as "L0 == chain proven".
//!
//! Faithfulness rules (see lessons.md 2026-06-01 / 2026-05-31):
//! - Sign-magnitude `I64` (NOT two's-complement i128) — `-0` normalizes to `+0`; floor
//!   division; abort codes mapped to `OnchainError`.
//! - `w` is RAW SVI total variance (T already baked into a,b,σ,ρ,m). No annualization, no √T.
//! - Move `x as u64` aborts on overflow; we use checked `u64::try_from` everywhere — NO
//!   silent `as u64` truncation.
//!
//! Tie-break: at settlement the chain uses strict `>` so `settlement == strike` resolves
//! the UP outcome to 0, i.e. **ties resolve DOWN**. Mirrored here; stated so downstream
//! routing does not misread ATM-at-settlement direction.

use thiserror::Error;

/// Fixed-point scale (1e9), == DeepBook `FLOAT_SCALING`.
pub const SCALE: u64 = 1_000_000_000;
/// Matches DeepBook `constants::max_u64`.
pub const MAX_U64: u64 = u64::MAX;
/// ln(2) in 1e9-FP.
pub const LN2: u64 = 693_147_180;

/// Domain errors mirroring on-chain `abort(n)`. Expected for an arb bot (bad oracle state /
/// degenerate params) — the bot skips the market, it does not crash.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum OnchainError {
    #[error("i64 magnitude overflow")] // chain abort 0 (and checked cast-back guard)
    MagnitudeOverflow,
    #[error("ln(0) is undefined")] // chain abort 0 in ln
    LnZero,
    #[error("i64 div by zero")] // chain abort 1
    DivByZero,
    #[error("exp argument overflow")] // chain abort 1 in exp
    ExpOverflow,
    #[error("sqrt domain error (b == 0 or b > SCALE)")] // chain abort 2
    SqrtDomain,
    #[error("forward price not positive")] // compute_nd2 abort 3
    ForwardNonPositive,
    #[error("bracket negative")] // compute_nd2 abort 4
    BracketNegative,
    #[error("total variance w not positive")] // compute_nd2 abort 5
    WNonPositive,
}

type Res<T> = Result<T, OnchainError>;

/// Sign-magnitude integer mirroring chain `i64::I64`. Constructors normalize `-0` to `+0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I64 {
    magnitude: u64,
    is_negative: bool,
}

impl I64 {
    pub fn zero() -> Self {
        I64 { magnitude: 0, is_negative: false }
    }
    pub fn from_u64(x: u64) -> Self {
        I64 { magnitude: x, is_negative: false }
    }
    /// `-0` normalizes to `+0` (matches chain `from_parts`).
    pub fn from_parts(magnitude: u64, is_negative: bool) -> Self {
        if magnitude == 0 {
            I64::zero()
        } else {
            I64 { magnitude, is_negative }
        }
    }
    pub fn magnitude(&self) -> u64 {
        self.magnitude
    }
    pub fn is_negative(&self) -> bool {
        self.is_negative
    }
    /// Negate; `-0` stays `+0`.
    pub fn neg(&self) -> Self {
        I64::from_parts(self.magnitude, !self.is_negative)
    }
}

impl I64 {
    /// Sign-magnitude add. `abort(0)` on same-sign magnitude overflow -> MagnitudeOverflow.
    pub fn add(&self, other: &I64) -> Res<I64> {
        if self.is_negative == other.is_negative {
            let mag = self
                .magnitude
                .checked_add(other.magnitude)
                .ok_or(OnchainError::MagnitudeOverflow)?;
            Ok(I64::from_parts(mag, self.is_negative))
        } else if self.magnitude >= other.magnitude {
            Ok(I64::from_parts(self.magnitude - other.magnitude, self.is_negative))
        } else {
            Ok(I64::from_parts(other.magnitude - self.magnitude, other.is_negative))
        }
    }

    pub fn sub(&self, other: &I64) -> Res<I64> {
        self.add(&other.neg())
    }

    /// `(a.mag * b.mag) / 1e9` in u128, checked cast back to u64; sign = xor of signs.
    pub fn mul_scaled(&self, other: &I64) -> Res<I64> {
        let prod = (self.magnitude as u128) * (other.magnitude as u128) / (SCALE as u128);
        let mag = u64::try_from(prod).map_err(|_| OnchainError::MagnitudeOverflow)?;
        Ok(I64::from_parts(mag, self.is_negative != other.is_negative))
    }

    /// `(a.mag * 1e9) / b.mag`; `abort(1)` if b.mag == 0 -> DivByZero; sign = xor.
    pub fn div_scaled(&self, other: &I64) -> Res<I64> {
        if other.magnitude == 0 {
            return Err(OnchainError::DivByZero);
        }
        let q = (self.magnitude as u128) * (SCALE as u128) / (other.magnitude as u128);
        let mag = u64::try_from(q).map_err(|_| OnchainError::MagnitudeOverflow)?;
        Ok(I64::from_parts(mag, self.is_negative != other.is_negative))
    }

    /// `mul_scaled(self, self).magnitude` — always non-negative.
    pub fn square_scaled(&self) -> Res<u64> {
        Ok(self.mul_scaled(self)?.magnitude)
    }
}

/// DeepBook `math::mul`: `floor(a*b / 1e9)`, round DOWN, u128 intermediate.
/// Checked cast-back: Move's `x as u64` aborts on overflow (does NOT wrap) — mirror as
/// MagnitudeOverflow rather than a silent Rust wrap (lessons.md 2026-06-01).
fn db_mul(a: u64, b: u64) -> Res<u64> {
    let p = (a as u128) * (b as u128) / (SCALE as u128);
    u64::try_from(p).map_err(|_| OnchainError::MagnitudeOverflow)
}

/// DeepBook `math::div`: `floor(a*1e9 / b)`, round DOWN. `b == 0` aborts on chain -> DivByZero.
fn db_div(a: u64, b: u64) -> Res<u64> {
    if b == 0 {
        return Err(OnchainError::DivByZero);
    }
    let q = (a as u128) * (SCALE as u128) / (b as u128);
    u64::try_from(q).map_err(|_| OnchainError::MagnitudeOverflow)
}

/// Taylor `Σ_{n=0..12} r^n/n!` in 1e9-FP, `r` in `[0, ln2)`.
fn exp_series(r: u64) -> u64 {
    let mut term: u64 = SCALE; // n = 0 term = 1.0
    let mut sum: u64 = SCALE;
    for n in 1u64..=12 {
        term = ((term as u128) * (r as u128) / ((n as u128) * (SCALE as u128))) as u64;
        if term == 0 {
            break;
        }
        sum += term;
    }
    sum
}

/// Predict `math::exp(&I64) -> u64` in 1e9-FP.
/// Positive-arg overflow guard at 23.638153699; `2^k` scaling via bit shift (checked).
fn exp(x: &I64) -> Res<u64> {
    if x.magnitude() == 0 {
        return Ok(SCALE);
    }
    if !x.is_negative() && x.magnitude() > 23_638_153_699 {
        return Err(OnchainError::ExpOverflow);
    }
    let k = x.magnitude() / LN2;
    let r = x.magnitude() - k * LN2; // in [0, ln2)
    let base = exp_series(r); // in [1.0, 2.0)*1e9
    if !x.is_negative() {
        let scaled = (base as u128) << k;
        u64::try_from(scaled).map_err(|_| OnchainError::ExpOverflow)
    } else {
        let recip = (SCALE as u128) * (SCALE as u128) / (base as u128);
        Ok((recip >> k) as u64)
    }
}

/// `sqrt_u128(a)`: bit-length initial guess, 7 Newton iterations, final down-adjust.
fn sqrt_u128(a: u128) -> u128 {
    if a == 0 {
        return 0;
    }
    let bits = 128 - a.leading_zeros();
    let mut x = 1u128 << ((bits + 1) / 2);
    for _ in 0..7 {
        x = (x + a / x) / 2;
    }
    if x * x > a {
        x -= 1;
    }
    x
}

/// Predict `math::sqrt(a, b) -> u64`: FP sqrt of `a/b`-ish. `0 < b <= 1e9` else `abort(2)`.
/// In the oracle path `b == 1e9` (inv == 1) -> `floor(sqrt(a*1e9))`.
fn sqrt(a: u64, b: u64) -> Res<u64> {
    if b == 0 || b > SCALE {
        return Err(OnchainError::SqrtDomain);
    }
    let inv = (SCALE / b) as u128; // b <= 1e9 -> inv >= 1
    let radicand = (a as u128) * inv * (SCALE as u128);
    let r = sqrt_u128(radicand) / inv;
    u64::try_from(r).map_err(|_| OnchainError::MagnitudeOverflow)
}

/// Reduce `x > 1e9` to mantissa in `[1e9, 2e9)` by halving; `shift` = number of halvings.
/// Bytecode-confirmed: shift set {32,16,8,4,2,1}, condition `(x >> s) >= 1e9` (math.mv re-disasm).
fn normalize(mut x: u64) -> (u64, u32) {
    let mut shift = 0u32;
    for s in [32u32, 16, 8, 4, 2, 1] {
        if (x >> s) >= SCALE {
            x >>= s;
            shift += s;
        }
    }
    (x, shift)
}

/// `ln` of mantissa in `[1e9, 2e9)` plus `shift*ln2`, via the atanh series.
///
/// Op-order is transcribed from the on-chain `math::ln_u128` bytecode (re-disassembled
/// 2026-06-01), NOT a real-valued-equivalent rearrangement — truncation order is load-bearing
/// for the eventual Part-2 bit-exact parity:
/// - Horner folds `*y2` INTO each step: `acc = y2*C5`, then `acc = y2*(C_i + acc)` for C4..C0,
///   giving `acc = y2*(C0 + y2*(C1 + ... + y2*C5))`.
/// - The `2*y` factor is applied as a SINGLE `mul_scaled(2*y, 1e9 + acc)` — i.e. one truncation,
///   not `2 * mul_scaled(y, bracket)` (which would truncate then double).
fn ln_u128(mantissa: u64, shift: u32) -> Res<I64> {
    // y = (m - 1e9) / (m + 1e9), 1e9-scaled. m >= 1e9 so numerator >= 0.
    let num = I64::from_u64(mantissa - SCALE);
    let den = I64::from_u64(mantissa + SCALE);
    let y = num.div_scaled(&den)?; // non-negative, < 1e9
    let y2 = I64::from_u64(y.square_scaled()?); // y^2, non-negative
    // coeffs 1/3 .. 1/13
    const C: [u64; 6] = [333_333_333, 200_000_000, 142_857_143, 111_111_111, 90_909_091, 76_923_077];
    // acc = y2 * C5, then fold: acc = y2 * (C_i + acc) for C4..C0
    let mut acc = y2.mul_scaled(&I64::from_u64(C[5]))?;
    for &c in C[..5].iter().rev() {
        acc = y2.mul_scaled(&acc.add(&I64::from_u64(c))?)?;
    }
    // bracket = 1e9 + acc  (the leading atanh `y` term lives in the `1e9`)
    let bracket = acc.add(&I64::from_u64(SCALE))?;
    // result = mul_scaled(2*y, bracket) + shift*ln2  — single truncation on the 2*y term
    let two_y = I64::from_parts(
        y.magnitude().checked_mul(2).ok_or(OnchainError::MagnitudeOverflow)?,
        y.is_negative(),
    );
    let series = two_y.mul_scaled(&bracket)?;
    let shift_term = I64::from_u64(
        (shift as u64).checked_mul(LN2).ok_or(OnchainError::MagnitudeOverflow)?,
    );
    series.add(&shift_term)
}

/// Predict `math::ln(x) -> I64`, `x` in 1e9-FP, `x > 0` (else LnZero).
fn ln(x: u64) -> Res<I64> {
    if x == 0 {
        return Err(OnchainError::LnZero);
    }
    if x == SCALE {
        return Ok(I64::zero());
    }
    if x < SCALE {
        // -ln(1e18 / x) = neg(ln(SCALE*SCALE/x))
        let arg = u64::try_from((SCALE as u128) * (SCALE as u128) / (x as u128))
            .map_err(|_| OnchainError::MagnitudeOverflow)?;
        return Ok(ln(arg)?.neg());
    }
    let (mantissa, shift) = normalize(x);
    ln_u128(mantissa, shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i64_constructors_normalize_neg_zero() {
        assert_eq!(I64::zero(), I64::from_parts(0, true)); // -0 -> +0
        assert_eq!(I64::zero(), I64::from_parts(0, false));
        assert_eq!(I64::from_u64(5), I64::from_parts(5, false));
        assert_eq!(I64::from_u64(5).neg(), I64::from_parts(5, true));
        assert_eq!(I64::zero().neg(), I64::zero());
        let x = I64::from_parts(7, true);
        assert_eq!(x.magnitude(), 7);
        assert!(x.is_negative());
    }

    #[test]
    fn i64_add_sub_sign_magnitude() {
        assert_eq!(I64::from_u64(3).add(&I64::from_u64(4)).unwrap(), I64::from_u64(7));
        assert_eq!(
            I64::from_u64(3).add(&I64::from_parts(4, true)).unwrap(),
            I64::from_parts(1, true)
        );
        assert_eq!(I64::from_u64(5).add(&I64::from_parts(5, true)).unwrap(), I64::zero());
        assert_eq!(I64::from_u64(10).sub(&I64::from_u64(4)).unwrap(), I64::from_u64(6));
        assert_eq!(
            I64::from_u64(MAX_U64).add(&I64::from_u64(1)),
            Err(OnchainError::MagnitudeOverflow)
        );
    }

    #[test]
    fn db_mul_div_round_down() {
        assert_eq!(db_mul(2 * SCALE, 3 * SCALE).unwrap(), 6 * SCALE);
        assert_eq!(db_mul(SCALE + 1, SCALE + 1).unwrap(), SCALE + 2);
        assert_eq!(db_div(6 * SCALE, 4 * SCALE).unwrap(), SCALE + SCALE / 2);
        assert_eq!(db_div(SCALE, 3 * SCALE).unwrap(), 333_333_333);
        assert_eq!(db_div(SCALE, 0), Err(OnchainError::DivByZero));
        assert_eq!(db_mul(MAX_U64, MAX_U64), Err(OnchainError::MagnitudeOverflow));
    }

    /// Assert two 1e9-FP integers agree within `tol` units (floor-truncation tolerance).
    fn approx_fp(got: u64, expected: u64, tol: u64) {
        let d = got.abs_diff(expected);
        assert!(d <= tol, "got {got}, expected {expected} (+/-{tol}), diff {d}");
    }

    #[test]
    fn exp_anchors_and_overflow() {
        assert_eq!(exp(&I64::zero()).unwrap(), SCALE);
        approx_fp(exp(&I64::from_u64(LN2)).unwrap(), 2 * SCALE, 50);
        approx_fp(exp(&I64::from_parts(LN2, true)).unwrap(), SCALE / 2, 50);
        assert_eq!(exp(&I64::from_u64(23_638_153_700)), Err(OnchainError::ExpOverflow));
        assert!(exp(&I64::from_u64(23_638_153_699)).is_ok());
    }

    #[test]
    fn sqrt_perfect_and_domain() {
        assert_eq!(sqrt(4 * SCALE, SCALE).unwrap(), 2 * SCALE);
        approx_fp(sqrt(2 * SCALE, SCALE).unwrap(), 1_414_213_562, 5);
        assert_eq!(sqrt(0, SCALE).unwrap(), 0);
        assert_eq!(sqrt(SCALE, 0), Err(OnchainError::SqrtDomain));
        assert_eq!(sqrt(SCALE, SCALE + 1), Err(OnchainError::SqrtDomain));
    }

    #[test]
    fn i64_mul_div_square_scaled() {
        assert_eq!(
            I64::from_u64(2 * SCALE).mul_scaled(&I64::from_u64(3 * SCALE)).unwrap(),
            I64::from_u64(6 * SCALE)
        );
        assert_eq!(
            I64::from_u64(2 * SCALE).mul_scaled(&I64::from_parts(3 * SCALE, true)).unwrap(),
            I64::from_parts(6 * SCALE, true)
        );
        assert_eq!(
            I64::from_u64(SCALE + 1).mul_scaled(&I64::from_u64(SCALE + 1)).unwrap(),
            I64::from_u64(SCALE + 2)
        );
        assert_eq!(
            I64::from_u64(6 * SCALE).div_scaled(&I64::from_u64(3 * SCALE)).unwrap(),
            I64::from_u64(2 * SCALE)
        );
        assert_eq!(
            I64::from_u64(1).div_scaled(&I64::zero()),
            Err(OnchainError::DivByZero)
        );
        assert_eq!(I64::from_parts(2 * SCALE, true).square_scaled().unwrap(), 4 * SCALE);
    }

    #[test]
    fn ln_anchors_and_branches() {
        assert_eq!(ln(SCALE).unwrap(), I64::zero());
        assert_eq!(ln(0), Err(OnchainError::LnZero));
        let l2 = ln(2 * SCALE).unwrap();
        assert!(!l2.is_negative());
        approx_fp(l2.magnitude(), LN2, 1000);
        let lhalf = ln(SCALE / 2).unwrap();
        assert!(lhalf.is_negative());
        approx_fp(lhalf.magnitude(), LN2, 1000);
        approx_fp(ln(2_718_281_828).unwrap().magnitude(), SCALE, 1000);
    }
}
