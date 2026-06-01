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
}
