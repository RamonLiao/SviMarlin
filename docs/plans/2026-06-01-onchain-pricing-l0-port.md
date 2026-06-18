# On-chain Pricing L0 Port (Plan B, Part 1) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the on-chain `oracle::compute_price` path to Rust as pure integer fixed-point math (1e9 scale, sign-magnitude `I64`) in `volarb-pricing`, so the bot can compute the *executable* binary price the same way the chain does.

**Architecture:** One new file `crates/volarb-pricing/src/onchain.rs`, zero new dependencies, pure `u64`/`u128`. Functions ported in strict dependency order: `I64` → DeepBook `mul`/`div` → Predict `ln`/`exp`/`sqrt`/`normal_cdf` → `compute_nd2`/`compute_price`/`binary_price_pair`. Every Move op that aborts maps to a typed `OnchainError` so the bot skips a bad market instead of crashing. **No `as u64` truncation anywhere** — narrowing casts use `u64::try_from` (Move's `as u64` aborts on overflow; Rust's wraps silently — see lessons.md 2026-06-01).

**Tech Stack:** Rust (edition 2024), `thiserror` (already a dep), `cargo test`/`clippy`/`fmt`.

**Sources of truth:**
- Formulas + constants: `docs/specs/2026-05-31-onchain-pricing-decompile-findings.md`
- Scope + error model + testing: `docs/specs/2026-06-01-onchain-pricing-l0-port-design.md`

**SCOPE GUARD (read first):** This is **offline self-consistency + hand/real-value golden + monkey** only. **No** network, **no** fixtures, **no** `devInspect`, **no** bit-exact-to-chain assertion. "tests pass" here does **NOT** mean "parity proven" — that is Part 2. Every commit message and the module doc must keep that caveat loud.

**Bytecode-pairing caveat:** The findings doc lists `normal_cdf` regime-A/B constants and the `ln_u128` series coeffs, but the exact constant→Horner-step pairing lived in a now-deleted `/tmp/dis_math.txt`. Tasks 5 and 7 therefore begin with a **re-disassembly step** (recipe in findings §Reproduce) to lock op-order from bytecode. Until Part 2 measures chain parity, those two functions are golden-tested against the *true real-valued* function within a documented integer tolerance, not pinned to the chain.

---

## File Structure

- **Create:** `crates/volarb-pricing/src/onchain.rs` — the entire L0 port (types, error enum, all ported fns, `#[cfg(test)]` module). One file, one responsibility: mirror the chain's integer pricing path.
- **Modify:** `crates/volarb-pricing/src/lib.rs` — add `pub mod onchain;`. No re-export of internals; keep `PricingError` (L1) and `OnchainError` (L0) separate types (different failure domains).
- **No** `Cargo.toml` change (zero new deps).

Module-internal layout of `onchain.rs`, top to bottom: constants → `OnchainError` → `I64` → DeepBook `mul`/`div` → `ln`/`exp`/`sqrt`/`normal_cdf` → `OnchainOracle` + `compute_nd2`/`compute_price`/`binary_price_pair` → `#[cfg(test)] mod tests`.

---

### Task 1: Scaffold module — constants, error enum, `I64` type + constructors

**Files:**
- Create: `crates/volarb-pricing/src/onchain.rs`
- Modify: `crates/volarb-pricing/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to a new test module at the bottom of `onchain.rs` (create the file with just the test first to drive the API):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i64_constructors_normalize_neg_zero() {
        assert_eq!(I64::zero(), I64::from_parts(0, true)); // -0 -> +0
        assert_eq!(I64::zero(), I64::from_parts(0, false));
        assert_eq!(I64::from_u64(5), I64::from_parts(5, false));
        // neg of a positive flips sign; neg of zero stays +0
        assert_eq!(I64::from_u64(5).neg(), I64::from_parts(5, true));
        assert_eq!(I64::zero().neg(), I64::zero());
        // accessors
        let x = I64::from_parts(7, true);
        assert_eq!(x.magnitude(), 7);
        assert!(x.is_negative());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-pricing onchain::tests::i64_constructors`
Expected: FAIL — `onchain` module / `I64` not found (compile error).

- [ ] **Step 3: Write minimal implementation**

Put this at the TOP of `crates/volarb-pricing/src/onchain.rs` (above the test module):

```rust
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
```

Then add to `crates/volarb-pricing/src/lib.rs`, after the existing `pub mod` lines:

```rust
pub mod onchain;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-pricing onchain::tests::i64_constructors`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-pricing/src/onchain.rs crates/volarb-pricing/src/lib.rs
git commit -m "feat(pricing): L0 onchain module scaffold — I64 type, constants, error enum

Part 1 offline port. Self-consistency only; chain parity is Part 2."
```

---

### Task 2: `I64` arithmetic — add/sub/mul_scaled/div_scaled/square_scaled

**Files:**
- Modify: `crates/volarb-pricing/src/onchain.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    #[test]
    fn i64_add_sub_sign_magnitude() {
        // same sign adds magnitudes
        assert_eq!(I64::from_u64(3).add(&I64::from_u64(4)).unwrap(), I64::from_u64(7));
        // opposite sign subtracts; sign of larger magnitude
        assert_eq!(
            I64::from_u64(3).add(&I64::from_parts(4, true)).unwrap(),
            I64::from_parts(1, true)
        );
        // exact cancellation -> +0
        assert_eq!(I64::from_u64(5).add(&I64::from_parts(5, true)).unwrap(), I64::zero());
        // sub is add(neg)
        assert_eq!(I64::from_u64(10).sub(&I64::from_u64(4)).unwrap(), I64::from_u64(6));
        // overflow on same-sign add
        assert_eq!(
            I64::from_u64(MAX_U64).add(&I64::from_u64(1)),
            Err(OnchainError::MagnitudeOverflow)
        );
    }

    #[test]
    fn i64_mul_div_square_scaled() {
        // mul_scaled: (a*b)/1e9, sign = xor. 2.0 * 3.0 = 6.0
        assert_eq!(
            I64::from_u64(2 * SCALE).mul_scaled(&I64::from_u64(3 * SCALE)).unwrap(),
            I64::from_u64(6 * SCALE)
        );
        // opposite signs -> negative
        assert_eq!(
            I64::from_u64(2 * SCALE).mul_scaled(&I64::from_parts(3 * SCALE, true)).unwrap(),
            I64::from_parts(6 * SCALE, true)
        );
        // floor truncation: (1.000000001 * 1.000000001) floors the 1e-18 tail away
        assert_eq!(
            I64::from_u64(SCALE + 1).mul_scaled(&I64::from_u64(SCALE + 1)).unwrap(),
            I64::from_u64(SCALE + 2) // 1.000000002000000001 -> floor at 1e9 -> 1.000000002
        );
        // div_scaled: (a*1e9)/b. 6.0 / 3.0 = 2.0
        assert_eq!(
            I64::from_u64(6 * SCALE).div_scaled(&I64::from_u64(3 * SCALE)).unwrap(),
            I64::from_u64(2 * SCALE)
        );
        // div by zero
        assert_eq!(
            I64::from_u64(1).div_scaled(&I64::zero()),
            Err(OnchainError::DivByZero)
        );
        // square_scaled is non-negative magnitude
        assert_eq!(I64::from_parts(2 * SCALE, true).square_scaled().unwrap(), 4 * SCALE);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-pricing onchain::tests::i64_add_sub onchain::tests::i64_mul_div`
Expected: FAIL — methods not defined.

- [ ] **Step 3: Write minimal implementation**

Add these methods to `impl I64`:

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-pricing onchain::tests`
Expected: PASS (all I64 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-pricing/src/onchain.rs
git commit -m "feat(pricing): L0 I64 sign-magnitude arithmetic (checked, floor div)"
```

---

### Task 3: DeepBook `mul`/`div` (round-down, checked cast-back)

**Files:**
- Modify: `crates/volarb-pricing/src/onchain.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    #[test]
    fn db_mul_div_round_down() {
        // mul: floor(a*b/1e9). 2.0 * 3.0 = 6.0
        assert_eq!(db_mul(2 * SCALE, 3 * SCALE).unwrap(), 6 * SCALE);
        // floor truncation toward zero on the 1e-9 tail
        assert_eq!(db_mul(SCALE + 1, SCALE + 1).unwrap(), SCALE + 2);
        // div: floor(a*1e9/b). 6.0 / 4.0 = 1.5
        assert_eq!(db_div(6 * SCALE, 4 * SCALE).unwrap(), SCALE + SCALE / 2);
        // div floor: 1 / 3 = 0.333333333 (floored, not rounded up)
        assert_eq!(db_div(SCALE, 3 * SCALE).unwrap(), 333_333_333);
        // div by zero -> DivByZero
        assert_eq!(db_div(SCALE, 0), Err(OnchainError::DivByZero));
        // mul that overflows u64 on cast-back -> MagnitudeOverflow (not silent wrap)
        assert_eq!(db_mul(MAX_U64, MAX_U64), Err(OnchainError::MagnitudeOverflow));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-pricing onchain::tests::db_mul_div`
Expected: FAIL — `db_mul`/`db_div` not defined.

- [ ] **Step 3: Write minimal implementation**

Add as free functions in `onchain.rs` (below the `impl I64`):

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-pricing onchain::tests::db_mul_div`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-pricing/src/onchain.rs
git commit -m "feat(pricing): L0 DeepBook mul/div round-down with checked cast-back"
```

---

### Task 4: `exp` and `sqrt`

(Done before `ln`/`normal_cdf` because they have no uncertain Horner pairing and `normal_cdf` regime B depends on `exp`.)

**Files:**
- Modify: `crates/volarb-pricing/src/onchain.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`. `approx_fp` is a small helper asserting two 1e9-FP integers agree within `tol` units (floor-truncation band); define it once in the test module if not already present:

```rust
    /// Assert two 1e9-FP integers agree within `tol` units (floor-truncation tolerance).
    fn approx_fp(got: u64, expected: u64, tol: u64) {
        let d = got.abs_diff(expected);
        assert!(d <= tol, "got {got}, expected {expected} (+/-{tol}), diff {d}");
    }

    #[test]
    fn exp_anchors_and_overflow() {
        // exp(0) == 1.0 exactly
        assert_eq!(exp(&I64::zero()).unwrap(), SCALE);
        // exp(ln2) == 2.0 within a few floor-truncation units
        approx_fp(exp(&I64::from_u64(LN2)).unwrap(), 2 * SCALE, 50);
        // exp(-ln2) == 0.5
        approx_fp(exp(&I64::from_parts(LN2, true)).unwrap(), SCALE / 2, 50);
        // positive-arg overflow guard (> ~23.638) -> ExpOverflow
        assert_eq!(exp(&I64::from_u64(23_638_153_700)), Err(OnchainError::ExpOverflow));
        // exactly at guard boundary is allowed (does not error)
        assert!(exp(&I64::from_u64(23_638_153_699)).is_ok());
    }

    #[test]
    fn sqrt_perfect_and_domain() {
        // sqrt(4.0) with b=1e9 -> 2.0 exactly (perfect square)
        assert_eq!(sqrt(4 * SCALE, SCALE).unwrap(), 2 * SCALE);
        // sqrt(2.0) ~ 1.41421356
        approx_fp(sqrt(2 * SCALE, SCALE).unwrap(), 1_414_213_562, 5);
        // sqrt(0) == 0
        assert_eq!(sqrt(0, SCALE).unwrap(), 0);
        // domain: b == 0 and b > SCALE -> SqrtDomain
        assert_eq!(sqrt(SCALE, 0), Err(OnchainError::SqrtDomain));
        assert_eq!(sqrt(SCALE, SCALE + 1), Err(OnchainError::SqrtDomain));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-pricing onchain::tests::exp_anchors onchain::tests::sqrt_perfect`
Expected: FAIL — `exp`/`sqrt` not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `onchain.rs`:

```rust
/// Taylor `Σ_{n=0..12} r^n/n!` in 1e9-FP, `r` in `[0, ln2)`.
fn exp_series(r: u64) -> u64 {
    let mut term: u64 = SCALE; // n = 0 term = 1.0
    let mut sum: u64 = SCALE;
    for n in 1u64..=12 {
        // term *= r / (n * 1e9)  in u128 then floor back
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
    if x.magnitude == 0 {
        return Ok(SCALE);
    }
    if !x.is_negative && x.magnitude > 23_638_153_699 {
        return Err(OnchainError::ExpOverflow);
    }
    let k = x.magnitude / LN2;
    let r = x.magnitude - k * LN2; // in [0, ln2)
    let base = exp_series(r); // in [1.0, 2.0)*1e9
    if !x.is_negative {
        // base * 2^k, checked (Move would abort on overflow within guard this never fires)
        let scaled = (base as u128) << k;
        u64::try_from(scaled).map_err(|_| OnchainError::ExpOverflow)
    } else {
        // reciprocal 1e18/base, then >> k (early-zero if it shifts out)
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-pricing onchain::tests::exp_anchors onchain::tests::sqrt_perfect`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-pricing/src/onchain.rs
git commit -m "feat(pricing): L0 exp (Taylor+2^k) and sqrt (Newton) fixed-point"
```

---

### Task 5: `ln` (re-disassemble to lock op-order, then port)

**Files:**
- Modify: `crates/volarb-pricing/src/onchain.rs`

- [ ] **Step 1: Re-disassemble to confirm `ln_u128` op-order**

The findings doc gives the series coeffs (`333333333, 200000000, 142857143, 111111111, 90909091, 76923077` = 1/3..1/13) but the exact Horner op-order pairing came from a deleted `/tmp/dis_math.txt`. Regenerate and read the `ln_u128`/`normalize` blocks before coding (recipe from findings §Reproduce):

```bash
PKG=0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138
curl -s https://fullnode.testnet.sui.io:443 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"sui_getObject","params":["'$PKG'",{"showBcs":true}]}' > /tmp/pkg.json
python3 - <<'PY'
import json,base64
d=json.load(open('/tmp/pkg.json')); mm=d['result']['data']['bcs']['moduleMap']
open('/tmp/math.mv','wb').write(base64.b64decode(mm['math']))
PY
sui move disassemble /tmp/math.mv > /tmp/dis_math.txt
# read the ln_u128 + normalize blocks; confirm: normalize halves by {32,16,8,4,2,1};
# atanh: y=(m-1e9)/(m+1e9) scaled; bracket = 1e9 + y2*(c0 + c1*y2 + ... + c5*y2^5);
# result = 2*mul_scaled(y, bracket) + shift*ln2.
```

If the disassembly contradicts the implementation in Step 3, fix Step 3 to match bytecode and note the correction in the commit message. (JSON-RPC still answers on testnet per progress.md 2026-05-30; if it is finally dead, the package is Immutable so any archived bytecode works — this step is read-only.)

- [ ] **Step 2: Write the failing test**

Add inside `mod tests`:

```rust
    #[test]
    fn ln_anchors_and_branches() {
        // ln(1.0) == 0 exactly
        assert_eq!(ln(SCALE).unwrap(), I64::zero());
        // ln(0) -> LnZero
        assert_eq!(ln(0), Err(OnchainError::LnZero));
        // ln(2.0) ~ 0.693147... positive
        let l2 = ln(2 * SCALE).unwrap();
        assert!(!l2.is_negative());
        approx_fp(l2.magnitude(), LN2, 1000);
        // ln(0.5) ~ -0.693147..., negative branch (x < 1e9 -> -ln(1e18/x))
        let lhalf = ln(SCALE / 2).unwrap();
        assert!(lhalf.is_negative());
        approx_fp(lhalf.magnitude(), LN2, 1000);
        // ln(e) ~ 1.0 (e ~ 2.718281828)
        approx_fp(ln(2_718_281_828).unwrap().magnitude(), SCALE, 1000);
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p volarb-pricing onchain::tests::ln_anchors`
Expected: FAIL — `ln`/`normalize`/`ln_u128` not defined.

- [ ] **Step 4: Write minimal implementation**

Add to `onchain.rs`:

```rust
/// Reduce `x > 1e9` to mantissa in `[1e9, 2e9)` by halving; `shift` = number of halvings.
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

/// `ln` of mantissa in `[1e9, 2e9)` plus `shift*ln2`. atanh series, 6 coeffs (1/3..1/13).
fn ln_u128(mantissa: u64, shift: u32) -> Res<I64> {
    // y = (m - 1e9) / (m + 1e9), 1e9-scaled. m >= 1e9 so numerator >= 0.
    let num = I64::from_u64(mantissa - SCALE);
    let den = I64::from_u64(mantissa + SCALE);
    let y = num.div_scaled(&den)?; // non-negative, < 1e9
    let y2 = I64::from_u64(y.square_scaled()?); // y^2, non-negative
    // Horner over y2: P = c0 + c1*y2 + ... + c5*y2^5, coeffs = 1/3 .. 1/13
    const C: [u64; 6] = [333_333_333, 200_000_000, 142_857_143, 111_111_111, 90_909_091, 76_923_077];
    let mut acc = I64::from_u64(C[5]);
    for &c in C[..5].iter().rev() {
        acc = acc.mul_scaled(&y2)?.add(&I64::from_u64(c))?;
    }
    // bracket = 1e9 + y2 * P
    let bracket = acc.mul_scaled(&y2)?.add(&I64::from_u64(SCALE))?;
    // 2 * y * bracket
    let two_y_bracket = y.mul_scaled(&bracket)?.add(&y.mul_scaled(&bracket)?)?;
    // + shift * ln2
    let shift_term = I64::from_u64(
        (shift as u64).checked_mul(LN2).ok_or(OnchainError::MagnitudeOverflow)?,
    );
    two_y_bracket.add(&shift_term)
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
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p volarb-pricing onchain::tests::ln_anchors`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/volarb-pricing/src/onchain.rs
git commit -m "feat(pricing): L0 ln (normalize + atanh series), op-order confirmed vs bytecode

Golden vs true ln within floor-truncation band; chain bit-parity deferred to Part 2."
```

---

### Task 6: `normal_cdf` (re-disassemble regime A/B pairing, then port)

**Files:**
- Modify: `crates/volarb-pricing/src/onchain.rs`

- [ ] **Step 1: Re-disassemble to lock regime-A/B constant→Horner-step pairing**

The findings doc lists the regime-A coeffs (consts 7–15) and regime-B coeffs (consts 16–33) but **not** which const maps to which numerator/denominator Horner step. This MUST be read from bytecode — guessing the pairing produces a plausible-but-wrong CDF. Reuse `/tmp/dis_math.txt` from Task 5 (or regenerate via the same recipe) and read the `normal_cdf_u128` block:

```bash
# /tmp/dis_math.txt already produced in Task 5 Step 1; if absent, rerun that recipe.
grep -n "normal_cdf" /tmp/dis_math.txt   # locate the block, then trace the stack machine:
#  regime A (mag < 0.66291): z = mag^2; P(z)/Q(z) rational; val = mag*P/Q; result = 0.5 +/- val
#  regime B (0.66291 <= mag < 5.65685): R(mag) rational * exp(-mag^2/2); tail; result = tail or 1e9-tail
# Record the exact numerator-coeff list, denominator-coeff list, and Horner order for each
# regime, then transcribe into the const arrays in Step 3.
```

**Fill the `A_NUM`/`A_DEN`/`B_NUM`/`B_DEN` arrays in Step 3 from this trace** — the values below are grouped per the findings doc's `2235252035, 161028231069, 1067689485460, 18154981253344, | 65682338, 47202581905, 976098551738, 10260932208619, 45507789335027` split (4 numerator-side, 5 denominator-side for A; analogous for B) but the *order* and num/den assignment is bytecode-confirmed here.

- [ ] **Step 2: Write the failing test**

Add inside `mod tests`:

```rust
    #[test]
    fn normal_cdf_anchors_and_clamps() {
        // N(0) == 0.5 exactly
        assert_eq!(normal_cdf(&I64::zero()).unwrap(), SCALE / 2);
        // hard clamp |x| > 8.0
        assert_eq!(normal_cdf(&I64::from_u64(8 * SCALE + 1)).unwrap(), SCALE);
        assert_eq!(normal_cdf(&I64::from_parts(8 * SCALE + 1, true)).unwrap(), 0);
        // symmetry: N(x) + N(-x) ~ 1.0
        let xp = normal_cdf(&I64::from_u64(SCALE)).unwrap();
        let xm = normal_cdf(&I64::from_parts(SCALE, true)).unwrap();
        approx_fp(xp + xm, SCALE, 50);
        // known value N(1.0) ~ 0.841344746
        approx_fp(normal_cdf(&I64::from_u64(SCALE)).unwrap(), 841_344_746, 200);
        // N(-1.96) ~ 0.0249979 (regime B, near 2-sigma)
        approx_fp(normal_cdf(&I64::from_parts(1_960_000_000, true)).unwrap(), 24_997_895, 500);
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p volarb-pricing onchain::tests::normal_cdf`
Expected: FAIL — `normal_cdf` not defined.

- [ ] **Step 4: Write minimal implementation**

Add to `onchain.rs`. **The `A_NUM`/`A_DEN`/`B_NUM`/`B_DEN` arrays and their Horner application are transcribed from the Step 1 bytecode trace** — the grouping below follows the findings doc; correct in-place if the trace differs:

```rust
/// Regime A break (0.66291), regime B break (5.656854249), hard clamp 8.0 — all 1e9-FP.
const NCDF_A_BREAK: u64 = 662_910_000;
const NCDF_B_BREAK: u64 = 5_656_854_249;

// Regime A rational coeffs (consts 7-15), num then den (confirm split + order vs bytecode).
const A_NUM: [u64; 4] = [2_235_252_035, 161_028_231_069, 1_067_689_485_460, 18_154_981_253_344];
const A_DEN: [u64; 5] =
    [65_682_338, 47_202_581_905, 976_098_551_738, 10_260_932_208_619, 45_507_789_335_027];
// Regime B rational coeffs (consts 16-33), num then den (confirm split + order vs bytecode).
const B_NUM: [u64; 8] = [
    398_941_512, 8_883_149_794, 93_506_656_132, 597_270_276_395, 2_494_537_585_290,
    6_848_190_450_536, 11_602_651_437_647, 9_842_714_838_384,
];
const B_DEN: [u64; 9] = [
    11, 22_266_688_044, 235_387_901_782, 1_519_377_599_408, 6_485_558_298_267,
    18_615_571_640_885, 34_900_952_721_146, 38_912_003_286_093, 19_685_429_676_860,
];

/// Horner evaluation of `Σ c[i]*z^i` in 1e9-FP via DeepBook round-down `db_mul`.
fn horner(coeffs: &[u64], z: u64) -> Res<u64> {
    let mut acc = 0u64;
    for &c in coeffs.iter().rev() {
        acc = db_mul(acc, z)?.checked_add(c).ok_or(OnchainError::MagnitudeOverflow)?;
    }
    Ok(acc)
}

/// Predict `math::normal_cdf(&I64) -> u64` (probability in 1e9-FP).
fn normal_cdf(x: &I64) -> Res<u64> {
    const HALF: u64 = SCALE / 2;
    let mag = x.magnitude;
    if mag > 8 * SCALE {
        return Ok(if x.is_negative { 0 } else { SCALE });
    }
    if mag < NCDF_A_BREAK {
        // z = mag^2; val = mag * P(z)/Q(z)
        let z = db_mul(mag, mag)?;
        let p = horner(&A_NUM, z)?;
        let q = horner(&A_DEN, z)?;
        let ratio = db_div(p, q)?;
        let val = db_mul(mag, ratio)?;
        return Ok(if x.is_negative { HALF - val } else { HALF + val });
    }
    if mag < NCDF_B_BREAK {
        // tail = R(mag) * exp(-mag^2/2)
        let r = db_div(horner(&B_NUM, mag)?, horner(&B_DEN, mag)?)?;
        let half_sq = I64::from_parts(db_mul(mag, mag)? / 2, true); // -mag^2/2
        let e = exp(&half_sq)?;
        let tail = db_mul(r, e)?;
        return Ok(if x.is_negative { tail } else { SCALE - tail });
    }
    // mag >= B break
    Ok(if x.is_negative { 0 } else { SCALE })
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p volarb-pricing onchain::tests::normal_cdf`
Expected: PASS. If `normal_cdf_anchors_and_clamps` fails on the known values (`N(1.0)`, `N(-1.96)`), the num/den split or Horner order from Step 1 is wrong — re-read the bytecode trace and correct the arrays. Do NOT widen the tolerance to force a pass; the anchor values are real Φ values and a correct port lands within band.

- [ ] **Step 6: Commit**

```bash
git add crates/volarb-pricing/src/onchain.rs
git commit -m "feat(pricing): L0 normal_cdf (regime A/B rational), pairing confirmed vs bytecode

Golden vs true Phi within band + monotone-across-breaks monkey test. Chain bit-parity: Part 2."
```

---

### Task 7: `OnchainOracle` + `compute_nd2` / `compute_price` / `binary_price_pair`

**Files:**
- Modify: `crates/volarb-pricing/src/onchain.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    /// ATM-ish oracle: F = K = 100, small variance. Hand-traced expected via the same path.
    fn sample_oracle() -> OnchainOracle {
        OnchainOracle {
            forward: 100 * SCALE,
            a: 10_000_000,            // 0.01 base variance
            b: 100_000_000,           // 0.1
            sigma: 200_000_000,       // 0.2
            rho: I64::from_parts(300_000_000, true), // -0.3
            m: I64::zero(),
            settlement: None,
        }
    }

    #[test]
    fn compute_price_atm_in_unit_range() {
        let o = sample_oracle();
        let p = o.compute_price(100 * SCALE).unwrap(); // K == F -> k == 0
        // ATM digital under positive variance is below 0.5 (N(d2), d2 = -w/(2*sqrt(w)) < 0)
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
        assert_eq!(o.compute_price(100 * SCALE), Err(OnchainError::ForwardNonPositive));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-pricing onchain::tests::compute_price onchain::tests::binary_pair onchain::tests::forward_zero`
Expected: FAIL — `OnchainOracle` / methods not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `onchain.rs`:

```rust
/// Subset of on-chain `oracle::OracleSVI` needed to price one expiry. All FP fields 1e9-scaled.
/// `w` is RAW total variance — `a,b,sigma,rho,m` already bake in T. No annualization here.
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
    /// `compute_nd2`: N(d2) with `d2 = (ln(F/K) - w/2)/sqrt(w)`, `w` raw total variance.
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
        Ok((up, SCALE - up))
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-pricing onchain::tests`
Expected: PASS (all onchain tests).

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-pricing/src/onchain.rs
git commit -m "feat(pricing): L0 compute_nd2/compute_price/binary_price_pair (raw-w SVI)

Settled tie-break strict > (ties DOWN), documented in module doc. Self-consistency only."
```

---

### Task 8: Monkey tests (test.md mandate — try to break it)

**Files:**
- Modify: `crates/volarb-pricing/src/onchain.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    #[test]
    fn monkey_extreme_strikes_never_panic() {
        let o = sample_oracle();
        // strike == 1 (deep ITM for UP) and MAX_U64 (deep OTM): must return Ok or typed Err, never panic
        let _ = o.compute_price(1);
        let _ = o.compute_price(MAX_U64);
        // forward == 0 surfaces typed error, not panic
        let mut o0 = sample_oracle();
        o0.forward = 0;
        assert_eq!(o0.compute_price(SCALE), Err(OnchainError::ForwardNonPositive));
    }

    #[test]
    fn monkey_degenerate_params_typed_errors() {
        // i64 div by zero
        assert_eq!(I64::from_u64(1).div_scaled(&I64::zero()), Err(OnchainError::DivByZero));
        // i64 magnitude overflow on add
        assert_eq!(
            I64::from_u64(MAX_U64).add(&I64::from_u64(MAX_U64)),
            Err(OnchainError::MagnitudeOverflow)
        );
        // sqrt domain
        assert_eq!(sqrt(SCALE, 0), Err(OnchainError::SqrtDomain));
        assert_eq!(sqrt(SCALE, SCALE + 1), Err(OnchainError::SqrtDomain));
        // ln(0)
        assert_eq!(ln(0), Err(OnchainError::LnZero));
    }

    #[test]
    fn monkey_neg_zero_normalizes_everywhere() {
        assert_eq!(I64::from_parts(0, true), I64::zero());
        assert_eq!(I64::from_u64(0).neg(), I64::zero());
        assert_eq!(I64::from_u64(5).mul_scaled(&I64::zero()).unwrap(), I64::zero());
        // -0 fed as oracle m/rho still prices
        let mut o = sample_oracle();
        o.m = I64::from_parts(0, true);
        o.rho = I64::from_parts(0, true);
        assert!(o.compute_price(100 * SCALE).is_ok());
    }

    #[test]
    fn monkey_w_nonpositive_when_a_zero_and_bracket_zero() {
        // a = 0, b = 0 -> w = 0 -> WNonPositive (never panic)
        let o = OnchainOracle {
            forward: 100 * SCALE,
            a: 0,
            b: 0,
            sigma: 0,
            rho: I64::zero(),
            m: I64::zero(),
            settlement: None,
        };
        // K == F -> k == 0; inner = 0 -> sqrt 0 -> bracket 0 -> w = 0
        assert_eq!(o.compute_price(100 * SCALE), Err(OnchainError::WNonPositive));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-pricing onchain::tests::monkey`
Expected: FAIL initially only if any path actually panics or returns the wrong variant. If they pass immediately, that is acceptable (they assert already-implemented behavior) — but run them to confirm no panic/overflow path was missed. If `monkey_w_nonpositive` panics with arithmetic overflow instead of returning `WNonPositive`, that is a real bug to fix in Task 7's `compute_nd2` (audit the `db_mul`/`checked_add` chain).

- [ ] **Step 3: Fix any panic surfaced**

If a monkey test panics (e.g. an unchecked shift in `exp` with a huge `k`, or a `sqrt_u128` `x*x` overflow), replace the offending op with a checked variant returning the appropriate `OnchainError`. Show the diff in the commit. If none panic, no code change — proceed.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-pricing onchain::tests`
Expected: PASS (entire onchain suite).

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-pricing/src/onchain.rs
git commit -m "test(pricing): L0 monkey tests — extreme strikes, degenerate params, -0, w<=0"
```

---

### Task 9: Verification gate + progress/docs caveat

**Files:**
- Modify: `tasks/progress.md` (mark Part 1 implemented; keep parity caveat)
- (No code change unless the gate surfaces one.)

- [ ] **Step 1: Run the full verification gate**

```bash
cargo test -p volarb-pricing
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

Expected: all green. If `clippy` flags the `<< k` in `exp` (possible `arithmetic_side_effects` or shift lint) or unused-result, fix minimally (e.g. keep the checked `try_from`, allow only with a justifying comment). If `fmt --check` fails, run `cargo fmt` and re-stage.

- [ ] **Step 2: Confirm the loud caveat is in the module doc**

Verify the `//!` header of `onchain.rs` still states "GOLDEN-TESTED FOR SELF-CONSISTENCY ONLY — NOT yet parity-verified against the live chain (Part 2)". (Written in Task 1; confirm not lost in edits.)

Run: `grep -n "NOT yet parity-verified" crates/volarb-pricing/src/onchain.rs`
Expected: one match in the header.

- [ ] **Step 3: Update progress.md**

In `tasks/progress.md`, under TODO #5 Part 1, change the status line from "🟡 spec done，實作未開始" to "✅ 實作完成（offline self-consistency；chain parity 仍待 Part 2）" and add a Recently-Completed entry dated 2026-06-01 noting: file `crates/volarb-pricing/src/onchain.rs`, full `compute_price` path ported, N tests green, **explicit caveat that bit-exact chain parity is NOT proven (Part 2)**, and that `normal_cdf`/`ln` Horner pairing was confirmed by re-disassembly.

- [ ] **Step 4: Commit**

```bash
git add tasks/progress.md
git commit -m "docs(progress): L0 Part 1 ported (offline self-consistency; parity = Part 2)"
```

---

## Self-Review

**1. Spec coverage:**
- Placement (`onchain.rs`, `pub mod`, zero deps) → Task 1. ✓
- Constants (SCALE/MAX_U64/LN2/regime breaks) → Tasks 1, 6. ✓
- `I64` + `OnchainOracle` types → Tasks 1, 2, 7. ✓
- Error enum + checked cast-back (no `as u64`) → Tasks 1, 3 (db_mul/div), and every `try_from`/`checked_*`. ✓
- Functions in dependency order (I64 → mul/div → ln/exp/sqrt/normal_cdf → compute_nd2/price/pair) → Tasks 2–7. ✓
- Tie-break strict `>` in module doc + test → Task 1 doc, Task 7 test. ✓
- Testing: per-fn golden anchors → Tasks 2,4,5,6,7; monkey → Task 8; verification gate → Task 9. ✓
- Out-of-scope marker (self-consistency not parity) → Task 1 doc header + Task 9 grep + progress caveat. ✓
- Part 2 JSON-RPC/gRPC note → recorded in spec; re-disasm steps (Tasks 5,6) note testnet JSON-RPC still works and package is Immutable. ✓

**2. Placeholder scan:** No "TBD"/"handle edge cases"/"similar to". The two re-disassembly steps (Tasks 5,6) are concrete bash + a transcription instruction with the candidate const arrays already filled — the engineer confirms/corrects ordering, not invents. Acceptable: this is the one place bytecode is ground truth and the doc admits the pairing was lost.

**3. Type consistency:** `db_mul`/`db_div` (Task 3) used in Tasks 5,6,7. `I64` method names (`add`/`sub`/`mul_scaled`/`div_scaled`/`square_scaled`/`neg`/`from_u64`/`from_parts`/`zero`/`magnitude`/`is_negative`) consistent across all tasks. `exp(&I64)`, `sqrt(u64,u64)`, `ln(u64)`, `normal_cdf(&I64)` signatures match call sites in Task 7. `OnchainError` variants match the abort-code mapping in the spec. `approx_fp` helper defined once (Task 4) and reused (Tasks 5,6). `Res<T>` alias defined Task 1. ✓

---

## Execution Handoff

**Note:** This port is plain integer Rust, NOT `.move` files — so the project's "no generic reviewer on Move code" override does **not** apply. After implementation, run `/dual-review` per dev-rules (round 1 codex, round 2 project rules) on the diff. Tasks 5 and 6 each carry a real risk (bytecode op-order); review should specifically sanity-check `ln`/`normal_cdf` golden values against an independent Φ/ln source.
