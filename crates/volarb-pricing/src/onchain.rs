//! L0 on-chain pricing port — integer fixed-point mirror of DeepBook Predict's
//! `oracle::compute_price` path. Pure `u64`/`u128`, zero IO, zero external deps.
//!
//! PARITY-VERIFIED against testnet (Part 2, 2026-06-13). The L3 harness
//! (`tests/onchain_parity.rs`) replays frozen devInspect fixtures bit-exact:
//! - 216 math cases (ln/exp/sqrt/normal_cdf + i64 scaled ops + DeepBook math::mul/div),
//!   boundary + abort coverage, against pkg `0xf5ea…5138` (Immutable).
//! - 11 e2e cases: the full `compute_nd2` composition transcribed op-for-op from `oracle.mv`
//!   bytecode and run as chained PTBs over a live `OracleSVI` (11 strikes) — TRUE chain parity.
//! - 12 settled cases (real chain settlement value, incl. `strike == settlement`): the settled
//!   branch is `s > K ? 1e9 : 0` with NO chain math, so these are a port self-consistency check
//!   of the strict-`>` tie-break direction on real inputs, NOT a chain-recomputed parity.
//!   See `docs/specs/2026-06-13-l0-parity-basis-findings.md`.
//!
//! Caveat: `oracle::compute_price` is `public(friend)`, so it cannot be devInspect-called
//! directly; the e2e fixture reconstructs it from public primitives (faithful to bytecode order,
//! native add/div done off-chain). Only one non-settled oracle was live at capture.
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
        I64 {
            magnitude: 0,
            is_negative: false,
        }
    }
    pub fn from_u64(x: u64) -> Self {
        I64 {
            magnitude: x,
            is_negative: false,
        }
    }
    /// `-0` normalizes to `+0` (matches chain `from_parts`).
    pub fn from_parts(magnitude: u64, is_negative: bool) -> Self {
        if magnitude == 0 {
            I64::zero()
        } else {
            I64 {
                magnitude,
                is_negative,
            }
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
            Ok(I64::from_parts(
                self.magnitude - other.magnitude,
                self.is_negative,
            ))
        } else {
            Ok(I64::from_parts(
                other.magnitude - self.magnitude,
                other.is_negative,
            ))
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
/// Exposed for the L3 parity harness (Part 2). Mirrors chain op-for-op.
pub fn db_mul(a: u64, b: u64) -> Res<u64> {
    let p = (a as u128) * (b as u128) / (SCALE as u128);
    u64::try_from(p).map_err(|_| OnchainError::MagnitudeOverflow)
}

/// DeepBook `math::div`: `floor(a*1e9 / b)`, round DOWN. `b == 0` aborts on chain -> DivByZero.
/// Exposed for the L3 parity harness (Part 2). Mirrors chain op-for-op.
pub fn db_div(a: u64, b: u64) -> Res<u64> {
    if b == 0 {
        return Err(OnchainError::DivByZero);
    }
    let q = (a as u128) * (SCALE as u128) / (b as u128);
    u64::try_from(q).map_err(|_| OnchainError::MagnitudeOverflow)
}

/// Taylor `Σ_{n=0..12} r^n/n!` in 1e9-FP, `r` in `[0, ln2)`.
fn exp_series(r: u64) -> Res<u64> {
    let mut term: u64 = SCALE; // n = 0 term = 1.0
    let mut sum: u64 = SCALE;
    for n in 1u64..=12 {
        // term stays <= 1e9 (multiply by r < ln2 < 1e9, divide by n*1e9), but use a checked
        // cast-back per the module's no-silent-`as u64` rule.
        let next = (term as u128) * (r as u128) / ((n as u128) * (SCALE as u128));
        term = u64::try_from(next).map_err(|_| OnchainError::MagnitudeOverflow)?;
        if term == 0 {
            break;
        }
        sum += term;
    }
    Ok(sum)
}

/// Predict `math::exp(&I64) -> u64` in 1e9-FP.
/// Positive-arg overflow guard at 23.638153699; `2^k` scaling via bit shift (checked).
/// Exposed for the L3 parity harness (Part 2). Mirrors chain op-for-op.
pub fn exp(x: &I64) -> Res<u64> {
    if x.magnitude() == 0 {
        return Ok(SCALE);
    }
    if !x.is_negative() && x.magnitude() > 23_638_153_699 {
        return Err(OnchainError::ExpOverflow);
    }
    let k = x.magnitude() / LN2;
    let r = x.magnitude() - k * LN2; // in [0, ln2)
    let base = exp_series(r)?; // in [1.0, 2.0)*1e9
    if !x.is_negative() {
        // Positive guard caps mag at 23.638e9 -> k <= 34, so `<< k` cannot reach the 128-bit limit.
        let scaled = (base as u128) << k;
        u64::try_from(scaled).map_err(|_| OnchainError::ExpOverflow)
    } else {
        // Negative arg: reciprocal then shift right. recip <= 1e9 so any k >= 30 zeros it; the
        // chain's exp early-returns 0 once it shifts to zero. Guard k >= 128 to avoid an
        // out-of-range u128 shift (Rust: debug panic / release wrap) for huge-magnitude inputs.
        let recip = (SCALE as u128) * (SCALE as u128) / (base as u128);
        let shifted = if k >= 128 { 0 } else { recip >> k };
        u64::try_from(shifted).map_err(|_| OnchainError::MagnitudeOverflow)
    }
}

/// `sqrt_u128(a)`: bit-length initial guess, 7 Newton iterations, final down-adjust.
fn sqrt_u128(a: u128) -> u128 {
    if a == 0 {
        return 0;
    }
    let bits = 128 - a.leading_zeros();
    let mut x = 1u128 << bits.div_ceil(2);
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
/// Exposed for the L3 parity harness (Part 2). Mirrors chain op-for-op.
pub fn sqrt(a: u64, b: u64) -> Res<u64> {
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
    const C: [u64; 6] = [
        333_333_333,
        200_000_000,
        142_857_143,
        111_111_111,
        90_909_091,
        76_923_077,
    ];
    // acc = y2 * C5, then fold: acc = y2 * (C_i + acc) for C4..C0
    let mut acc = y2.mul_scaled(&I64::from_u64(C[5]))?;
    for &c in C[..5].iter().rev() {
        acc = y2.mul_scaled(&acc.add(&I64::from_u64(c))?)?;
    }
    // bracket = 1e9 + acc  (the leading atanh `y` term lives in the `1e9`)
    let bracket = acc.add(&I64::from_u64(SCALE))?;
    // result = mul_scaled(2*y, bracket) + shift*ln2  — single truncation on the 2*y term
    let two_y = I64::from_parts(
        y.magnitude()
            .checked_mul(2)
            .ok_or(OnchainError::MagnitudeOverflow)?,
        y.is_negative(),
    );
    let series = two_y.mul_scaled(&bracket)?;
    let shift_term = I64::from_u64(
        (shift as u64)
            .checked_mul(LN2)
            .ok_or(OnchainError::MagnitudeOverflow)?,
    );
    series.add(&shift_term)
}

/// Predict `math::ln(x) -> I64`, `x` in 1e9-FP, `x > 0` (else LnZero).
/// Exposed for the L3 parity harness (Part 2). Mirrors chain op-for-op.
pub fn ln(x: u64) -> Res<I64> {
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

// --- normal_cdf, transcribed from on-chain `math::normal_cdf_u128` bytecode (re-disasm 2026-06-01) ---
//
// Two regimes split at the constants below; NO separate 8e9 clamp (the chain saturates at the
// B-break via the else branch — verified in bytecode). Each regime is TWO interleaved Horner-fold
// chains (numerator + denominator), step `acc = (acc + c) * var / 1e9` == `db_mul(acc + c, var)`.
// IMPORTANT: the findings-doc grouping mis-assigned `11` — it is regime-B's numerator LEADING
// coeff (chain init `db_mul(11, mag)`), NOT a denominator coeff. Pairing is bytecode-confirmed.
const NCDF_A_BREAK: u64 = 662_910_000;
const NCDF_B_BREAK: u64 = 5_656_854_249;
// Regime A (polynomials in z = mag^2/1e9). Numerator init coeff = A_C11; denominator init = z (1.0).
const A_C7: u64 = 2_235_252_035;
const A_C8: u64 = 161_028_231_069;
const A_C9: u64 = 1_067_689_485_460;
const A_C10: u64 = 18_154_981_253_344;
const A_C11: u64 = 65_682_338;
const A_C12: u64 = 47_202_581_905;
const A_C13: u64 = 976_098_551_738;
const A_C14: u64 = 10_260_932_208_619;
const A_C15: u64 = 45_507_789_335_027;
// Regime B (polynomials in mag). Numerator init coeff = B_C25 (== 11); denominator init = mag (1.0).
const B_C17: u64 = 398_941_512;
const B_C18: u64 = 8_883_149_794;
const B_C19: u64 = 93_506_656_132;
const B_C20: u64 = 597_270_276_395;
const B_C21: u64 = 2_494_537_585_290;
const B_C22: u64 = 6_848_190_450_536;
const B_C23: u64 = 11_602_651_437_647;
const B_C24: u64 = 9_842_714_838_384;
const B_C25: u64 = 11;
const B_C26: u64 = 22_266_688_044;
const B_C27: u64 = 235_387_901_782;
const B_C28: u64 = 1_519_377_599_408;
const B_C29: u64 = 6_485_558_298_267;
const B_C30: u64 = 18_615_571_640_885;
const B_C31: u64 = 34_900_952_721_146;
const B_C32: u64 = 38_912_003_286_093;
const B_C33: u64 = 19_685_429_676_860;

/// Horner-fold chain: `acc` then for each `c`: `acc = (acc + c) * var / 1e9` (`db_mul(acc+c, var)`).
/// The final `+ last` add (no trailing multiply) is applied by the caller.
fn poly_fold(init: u64, coeffs: &[u64], var: u64) -> Res<u64> {
    let mut acc = init;
    for &c in coeffs {
        let s = acc.checked_add(c).ok_or(OnchainError::MagnitudeOverflow)?;
        acc = db_mul(s, var)?;
    }
    Ok(acc)
}

/// Predict `math::normal_cdf(&I64) -> u64` (probability in 1e9-FP).
/// Exposed for the L3 parity harness (Part 2). Mirrors chain op-for-op.
pub fn normal_cdf(x: &I64) -> Res<u64> {
    const HALF: u64 = SCALE / 2;
    let mag = x.magnitude();
    if mag < NCDF_A_BREAK {
        // regime A: val = mag * num/den ; result = 0.5 -/+ val
        let z = db_mul(mag, mag)?;
        let num = poly_fold(db_mul(A_C11, z)?, &[A_C7, A_C8, A_C9], z)?
            .checked_add(A_C10)
            .ok_or(OnchainError::MagnitudeOverflow)?;
        let den = poly_fold(z, &[A_C12, A_C13, A_C14], z)?
            .checked_add(A_C15)
            .ok_or(OnchainError::MagnitudeOverflow)?;
        let val = db_mul(mag, db_div(num, den)?)?;
        // Chain does plain `0.5 -/+ val` (Move Sub aborts on underflow). checked_sub keeps that
        // fail-loud semantic instead of a silent release wrap if val ever exceeds 0.5.
        return Ok(if x.is_negative() {
            HALF.checked_sub(val)
                .ok_or(OnchainError::MagnitudeOverflow)?
        } else {
            HALF + val
        });
    }
    if mag < NCDF_B_BREAK {
        // regime B: tail = R(mag) * exp(-mag^2/2) ; result = tail or 1e9 - tail
        let num = poly_fold(
            db_mul(B_C25, mag)?,
            &[B_C17, B_C18, B_C19, B_C20, B_C21, B_C22, B_C23],
            mag,
        )?
        .checked_add(B_C24)
        .ok_or(OnchainError::MagnitudeOverflow)?;
        let den = poly_fold(mag, &[B_C26, B_C27, B_C28, B_C29, B_C30, B_C31, B_C32], mag)?
            .checked_add(B_C33)
            .ok_or(OnchainError::MagnitudeOverflow)?;
        let r = db_div(num, den)?;
        let half_sq = db_mul(mag, mag)? / 2; // mag^2/(2*1e9); floor identity matches chain's mag*mag/(1e9*2)
        let e = exp(&I64::from_parts(half_sq, true))?; // exp(-mag^2/2)
        let tail = db_mul(r, e)?;
        // chain: `1e9 - tail` (Move Sub aborts on underflow) — checked to stay fail-loud.
        return Ok(if x.is_negative() {
            tail
        } else {
            SCALE
                .checked_sub(tail)
                .ok_or(OnchainError::MagnitudeOverflow)?
        });
    }
    // mag >= B-break: saturate
    Ok(if x.is_negative() { 0 } else { SCALE })
}

/// Subset of on-chain `oracle::OracleSVI` needed to price one expiry. All FP fields 1e9-scaled.
/// `w` is RAW total variance — `a,b,sigma,rho,m` already bake in T. No annualization, no √T.
#[derive(Debug, Clone)]
pub struct OnchainOracle {
    pub forward: u64,
    pub a: u64,
    pub b: u64,
    pub sigma: u64,
    pub rho: I64,
    pub m: I64,
    pub settlement: Option<u64>,
}

impl OnchainOracle {
    /// `compute_nd2`: N(d2) with `d2 = (ln(F/K) - w/2)/sqrt(w)`, `w` raw SVI total variance.
    /// Mirrors on-chain `oracle::compute_nd2` op-for-op (findings §compute_nd2).
    fn compute_nd2(&self, strike: u64) -> Res<u64> {
        if self.forward == 0 {
            return Err(OnchainError::ForwardNonPositive);
        }
        // k = ln(K/F) = ln(db_div(K, F))
        let k = ln(db_div(strike, self.forward)?)?;
        let diff = k.sub(&self.m)?; // k - m
        let inner = diff
            .square_scaled()?
            .checked_add(db_mul(self.sigma, self.sigma)?)
            .ok_or(OnchainError::MagnitudeOverflow)?; // (k-m)^2 + sigma^2
        let sqrt_t = I64::from_u64(sqrt(inner, SCALE)?); // sqrt((k-m)^2 + sigma^2)
        let rho_term = self.rho.mul_scaled(&diff)?; // rho*(k-m)
        let bracket = rho_term.add(&sqrt_t)?;
        if bracket.is_negative() {
            return Err(OnchainError::BracketNegative);
        }
        // w = a + b*bracket  (raw total variance)
        let w = self
            .a
            .checked_add(db_mul(self.b, bracket.magnitude())?)
            .ok_or(OnchainError::MagnitudeOverflow)?;
        if w == 0 {
            return Err(OnchainError::WNonPositive);
        }
        let sqrt_w = I64::from_u64(sqrt(w, SCALE)?);
        let half_w = I64::from_u64(w / 2);
        let numer = k.add(&half_w)?; // ln(K/F) + w/2
        let d = numer.div_scaled(&sqrt_w)?;
        let d2 = d.neg(); // (ln(F/K) - w/2)/sqrt(w)
        normal_cdf(&d2)
    }

    /// UP price (cash-or-nothing digital). Settled: strict `>` so `s == K` -> 0 (ties resolve DOWN).
    pub fn compute_price(&self, strike: u64) -> Res<u64> {
        match self.settlement {
            Some(s) => Ok(if s > strike { SCALE } else { 0 }),
            None => self.compute_nd2(strike),
        }
    }

    /// `(up, down)` where `down = SCALE - up`.
    pub fn binary_price_pair(&self, strike: u64) -> Res<(u64, u64)> {
        let up = self.compute_price(strike)?;
        let down = SCALE
            .checked_sub(up)
            .ok_or(OnchainError::MagnitudeOverflow)?;
        Ok((up, down))
    }
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
        assert_eq!(
            I64::from_u64(3).add(&I64::from_u64(4)).unwrap(),
            I64::from_u64(7)
        );
        assert_eq!(
            I64::from_u64(3).add(&I64::from_parts(4, true)).unwrap(),
            I64::from_parts(1, true)
        );
        assert_eq!(
            I64::from_u64(5).add(&I64::from_parts(5, true)).unwrap(),
            I64::zero()
        );
        assert_eq!(
            I64::from_u64(10).sub(&I64::from_u64(4)).unwrap(),
            I64::from_u64(6)
        );
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
        assert_eq!(
            db_mul(MAX_U64, MAX_U64),
            Err(OnchainError::MagnitudeOverflow)
        );
    }

    /// Assert two 1e9-FP integers agree within `tol` units (floor-truncation tolerance).
    fn approx_fp(got: u64, expected: u64, tol: u64) {
        let d = got.abs_diff(expected);
        assert!(
            d <= tol,
            "got {got}, expected {expected} (+/-{tol}), diff {d}"
        );
    }

    #[test]
    fn exp_anchors_and_overflow() {
        assert_eq!(exp(&I64::zero()).unwrap(), SCALE);
        approx_fp(exp(&I64::from_u64(LN2)).unwrap(), 2 * SCALE, 50);
        approx_fp(exp(&I64::from_parts(LN2, true)).unwrap(), SCALE / 2, 50);
        assert_eq!(
            exp(&I64::from_u64(23_638_153_700)),
            Err(OnchainError::ExpOverflow)
        );
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
            I64::from_u64(2 * SCALE)
                .mul_scaled(&I64::from_u64(3 * SCALE))
                .unwrap(),
            I64::from_u64(6 * SCALE)
        );
        assert_eq!(
            I64::from_u64(2 * SCALE)
                .mul_scaled(&I64::from_parts(3 * SCALE, true))
                .unwrap(),
            I64::from_parts(6 * SCALE, true)
        );
        assert_eq!(
            I64::from_u64(SCALE + 1)
                .mul_scaled(&I64::from_u64(SCALE + 1))
                .unwrap(),
            I64::from_u64(SCALE + 2)
        );
        assert_eq!(
            I64::from_u64(6 * SCALE)
                .div_scaled(&I64::from_u64(3 * SCALE))
                .unwrap(),
            I64::from_u64(2 * SCALE)
        );
        assert_eq!(
            I64::from_u64(1).div_scaled(&I64::zero()),
            Err(OnchainError::DivByZero)
        );
        assert_eq!(
            I64::from_parts(2 * SCALE, true).square_scaled().unwrap(),
            4 * SCALE
        );
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

    #[test]
    fn normal_cdf_anchors_and_clamps() {
        // N(0) == 0.5 exactly
        assert_eq!(normal_cdf(&I64::zero()).unwrap(), SCALE / 2);
        // saturation past the B-break (8.0 is well past it)
        assert_eq!(normal_cdf(&I64::from_u64(8 * SCALE + 1)).unwrap(), SCALE);
        assert_eq!(
            normal_cdf(&I64::from_parts(8 * SCALE + 1, true)).unwrap(),
            0
        );
        // symmetry: N(x) + N(-x) ~ 1.0
        let xp = normal_cdf(&I64::from_u64(SCALE)).unwrap();
        let xm = normal_cdf(&I64::from_parts(SCALE, true)).unwrap();
        approx_fp(xp + xm, SCALE, 50);
        // known value N(1.0) ~ 0.841344746 (regime B)
        approx_fp(normal_cdf(&I64::from_u64(SCALE)).unwrap(), 841_344_746, 200);
        // N(-1.96) ~ 0.0249979 (regime B)
        approx_fp(
            normal_cdf(&I64::from_parts(1_960_000_000, true)).unwrap(),
            24_997_895,
            500,
        );
        // regime A small value N(0.5) ~ 0.691462461
        approx_fp(
            normal_cdf(&I64::from_u64(500_000_000)).unwrap(),
            691_462_461,
            200,
        );
    }

    #[test]
    fn normal_cdf_monotone_across_regime_breaks() {
        // sweep across both regime breaks; must be non-decreasing (within floor truncation)
        let mut prev = 0u64;
        let mut x = 0u64;
        while x <= 7 * SCALE {
            let v = normal_cdf(&I64::from_u64(x)).unwrap();
            assert!(v + 2 >= prev, "non-monotone at x={x}: {v} < {prev}");
            prev = v;
            x += 10_000_000; // 0.01 steps -> crosses 0.66291 and 5.65685
        }
    }

    /// ATM-ish oracle: F = 100, small variance.
    fn sample_oracle() -> OnchainOracle {
        OnchainOracle {
            forward: 100 * SCALE,
            a: 10_000_000,                           // 0.01 base variance
            b: 100_000_000,                          // 0.1
            sigma: 200_000_000,                      // 0.2
            rho: I64::from_parts(300_000_000, true), // -0.3
            m: I64::zero(),
            settlement: None,
        }
    }

    #[test]
    fn compute_price_atm_in_unit_range() {
        let o = sample_oracle();
        let p = o.compute_price(100 * SCALE).unwrap(); // K == F -> k == 0
        // ATM digital under positive variance: d2 = -w/(2*sqrt(w)) < 0 -> N(d2) < 0.5
        assert!(p < SCALE / 2, "ATM N(d2) should be < 0.5, got {p}");
        assert!(p > 0 && p < SCALE);
    }

    #[test]
    fn compute_price_settled_strict_gt() {
        let mut o = sample_oracle();
        o.settlement = Some(120 * SCALE);
        assert_eq!(o.compute_price(100 * SCALE).unwrap(), SCALE); // s > K -> 1.0
        assert_eq!(o.compute_price(120 * SCALE).unwrap(), 0); // s == K -> 0 (ties DOWN)
        assert_eq!(o.compute_price(130 * SCALE).unwrap(), 0); // s < K -> 0
    }

    #[test]
    fn binary_pair_sums_to_one() {
        let o = sample_oracle();
        let (up, down) = o.binary_price_pair(100 * SCALE).unwrap();
        assert_eq!(up + down, SCALE);
    }

    #[test]
    fn forward_zero_errors() {
        let mut o = sample_oracle();
        o.forward = 0;
        assert_eq!(
            o.compute_price(100 * SCALE),
            Err(OnchainError::ForwardNonPositive)
        );
    }

    // --- Monkey tests (test.md mandate: try to break it; never panic, always typed Err) ---

    #[test]
    fn monkey_extreme_strikes_never_panic() {
        let o = sample_oracle();
        // strike == 1 (deep ITM for UP) and MAX_U64 (deep OTM): Ok or typed Err, never panic
        let _ = o.compute_price(1);
        let _ = o.compute_price(MAX_U64);
        let mut o0 = sample_oracle();
        o0.forward = 0;
        assert_eq!(
            o0.compute_price(SCALE),
            Err(OnchainError::ForwardNonPositive)
        );
    }

    #[test]
    fn monkey_degenerate_params_typed_errors() {
        assert_eq!(
            I64::from_u64(1).div_scaled(&I64::zero()),
            Err(OnchainError::DivByZero)
        );
        assert_eq!(
            I64::from_u64(MAX_U64).add(&I64::from_u64(MAX_U64)),
            Err(OnchainError::MagnitudeOverflow)
        );
        assert_eq!(sqrt(SCALE, 0), Err(OnchainError::SqrtDomain));
        assert_eq!(sqrt(SCALE, SCALE + 1), Err(OnchainError::SqrtDomain));
        assert_eq!(ln(0), Err(OnchainError::LnZero));
    }

    #[test]
    fn monkey_neg_zero_normalizes_everywhere() {
        assert_eq!(I64::from_parts(0, true), I64::zero());
        assert_eq!(I64::from_u64(0).neg(), I64::zero());
        assert_eq!(
            I64::from_u64(5).mul_scaled(&I64::zero()).unwrap(),
            I64::zero()
        );
        let mut o = sample_oracle();
        o.m = I64::from_parts(0, true);
        o.rho = I64::from_parts(0, true);
        assert!(o.compute_price(100 * SCALE).is_ok());
    }

    #[test]
    fn monkey_w_nonpositive_when_a_zero_and_bracket_zero() {
        // a = 0, b = 0, sigma = 0 -> at K==F: k=0, inner=0, bracket=0, w=0 -> WNonPositive (no panic)
        let o = OnchainOracle {
            forward: 100 * SCALE,
            a: 0,
            b: 0,
            sigma: 0,
            rho: I64::zero(),
            m: I64::zero(),
            settlement: None,
        };
        assert_eq!(
            o.compute_price(100 * SCALE),
            Err(OnchainError::WNonPositive)
        );
    }

    #[test]
    fn monkey_exp_large_negative_saturates_zero() {
        // huge negative arg -> k = mag/ln2 >> 128. Must saturate to 0 (chain zeros out),
        // NOT panic on the u128 `>> k` shift. Regression guard for the negative-branch fix.
        assert_eq!(exp(&I64::from_parts(500_000_000_000, true)).unwrap(), 0);
        assert_eq!(exp(&I64::from_parts(MAX_U64, true)).unwrap(), 0);
    }

    #[test]
    fn monkey_normal_cdf_full_sweep_no_panic_monotone() {
        // dense sweep both signs across full domain; never panic, non-decreasing in x
        let mut prev = 0u64;
        let mut x = 0u64;
        while x <= 9 * SCALE {
            let pos = normal_cdf(&I64::from_u64(x)).unwrap();
            let neg = normal_cdf(&I64::from_parts(x, true)).unwrap();
            assert!(pos + 2 >= prev, "non-monotone at {x}");
            // symmetry within truncation band
            assert!(
                pos.abs_diff(SCALE - neg) <= 50,
                "asymmetry at {x}: {pos} vs {}",
                SCALE - neg
            );
            prev = pos;
            x += 1_000_000; // 0.001 steps
        }
    }
}
