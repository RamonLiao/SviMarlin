# volarb-pricing L1 (SVI Surface) Implementation Plan — Plan A of TODO #5

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the L1 (float-domain) pricing layer: core `SVISurface::sigma_at` evaluation, Black-Scholes binary inversion (HL leg), and the Zeliade quasi-explicit SVI fitter — all pure, fully unit-tested.

**Architecture:** `volarb-core` owns the SVI surface type and its evaluation (DAG-forced: pricing depends on core, so eval can't live in pricing). `volarb-pricing` owns binary inversion + the fitter. The fitter is Zeliade two-layer: a hand-rolled 2D Nelder-Mead outer over `(m, σ)` and a closed-form 3×3 normal-equations inner over `(a, c, d)` (the change of variables `w = a + d·y + c·√(y²+1)` is linear in those three), with feasibility projection onto the no-arb polytope.

**Tech Stack:** Rust (edition 2024), `statrs` (normal CDF), `thiserror`. Optimizer hand-rolled (no `cobyla`/`nalgebra`).

**Scope note:** This is Plan A of TODO #5 (the L1 layer). Plan B (L0 chain-parity port + L3 parity harness) is gated on a `sui-decompile` spike of the chain `math`/`oracle` source and is written separately afterward — see the design §0/§4.1/§5 (`docs/specs/2026-05-30-volarb-pricing-svi-fitter-design.md`). Plan A produces working, testable software on its own (a usable float IV surface + fitter).

---

## File Structure

- `crates/volarb-core/src/svi.rs` — MODIFY: `Smile` struct, `SVISurface` fields (`as_of_ms`, `per_expiry: BTreeMap<u64, Smile>`), `sigma_at(.., now_ms)`, `is_stale`, `MS_PER_YEAR`. Update inline test.
- `crates/volarb-core/src/lib.rs` — MODIFY: export `Smile`.
- `crates/volarb-core/tests/serde_roundtrip.rs` — MODIFY: build surface with new shape.
- `Cargo.toml` (root) — MODIFY: add `statrs` to `[workspace.dependencies]`.
- `crates/volarb-pricing/Cargo.toml` — MODIFY: depend on `statrs`, `thiserror`.
- `crates/volarb-pricing/src/lib.rs` — MODIFY: module decls + re-exports + `PricingError`.
- `crates/volarb-pricing/src/binary.rs` — CREATE: `binary_price`, `implied_vol_from_binary`.
- `crates/volarb-pricing/src/svi_fit.rs` — CREATE: `fit_smile` + inner/outer optimizer.

---

## Task 1: Core — `Smile`/`SVISurface` reshape + `sigma_at` eval + `is_stale`

**Files:**
- Modify: `crates/volarb-core/src/svi.rs`
- Modify: `crates/volarb-core/src/lib.rs:12`
- Modify: `crates/volarb-core/tests/serde_roundtrip.rs:44-55`

- [ ] **Step 1: Replace `svi.rs` body (struct reshape + eval + tests)**

Replace the entire contents of `crates/volarb-core/src/svi.rs` with:

```rust
use crate::numeric::{Expiry, Strike, VolPoints};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Calendar-day annualization (L1 display day-count, design §3.2). The authoritative
/// day-count is the chain's (`oracle::compute_price`); L0/L3 (Plan B) measure any gap.
/// `pub` so the fitter (`volarb-pricing`) uses the SAME constant — if eval and fit disagree on
/// day-count, the fitted smile won't reproduce under eval (silent bug).
pub const MS_PER_YEAR: f64 = 365.0 * 24.0 * 3600.0 * 1000.0;

/// Gatheral raw SVI parameters for a single smile (one expiry). Design spec §3.2.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SVIParams {
    pub a: f64,
    pub b: f64,
    pub rho: f64,
    pub m: f64,
    pub sigma: f64,
}

/// One expiry's smile: SVI params plus the forward they were measured against. Mirrors the
/// chain: each `oracle::OracleSVI` object carries its own `svi` params AND `prices.forward`
/// (design §3.1 / §7). Forward lives WITH params so they cannot desync across snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Smile {
    pub params: SVIParams,
    pub forward: f64,
}

/// Implied-vol surface: an off-chain aggregation of N per-expiry on-chain `OracleSVI` objects.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SVISurface {
    /// Snapshot time for the §242 staleness gate ONLY — NOT used to derive time-to-expiry.
    pub as_of_ms: u64,
    /// expiry `unix_ms` -> smile.
    pub per_expiry: BTreeMap<u64, Smile>,
}

impl SVISurface {
    /// Annualized implied vol at `(strike, expiry)`, valued at `now_ms`. Returns `None` if no
    /// smile exists for that expiry, the strike/forward is non-positive, total variance is
    /// negative (defensive), or the option has expired (`T <= 0`). Result is in vol points
    /// (annualized sigma x 100, e.g. `VolPoints(80.0)` == 80% vol).
    pub fn sigma_at(&self, strike: Strike, expiry: Expiry, now_ms: u64) -> Option<VolPoints> {
        let smile = self.per_expiry.get(&expiry.unix_ms)?;
        let f = smile.forward;
        if f <= 0.0 || strike.0 <= 0.0 {
            return None;
        }
        let k = (strike.0 / f).ln();
        let p = &smile.params;
        let d = k - p.m;
        let w = p.a + p.b * (p.rho * d + (d * d + p.sigma * p.sigma).sqrt());
        if w < 0.0 {
            return None;
        }
        let t = (expiry.unix_ms as f64 - now_ms as f64) / MS_PER_YEAR;
        if t <= 0.0 {
            return None;
        }
        let sigma = (w / t).sqrt();
        if !sigma.is_finite() {
            return None;
        }
        Some(VolPoints(sigma * 100.0))
    }

    /// §242 staleness gate: true if the snapshot is older than `max_age_ms`.
    pub fn is_stale(&self, now_ms: u64, max_age_ms: u64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) > max_age_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smile() -> Smile {
        Smile {
            params: SVIParams { a: 0.04, b: 0.4, rho: -0.3, m: 0.0, sigma: 0.1 },
            forward: 64_000.0,
        }
    }

    // WHY: "no smile for this expiry" is normal control flow (router skips that venue), not a
    // panic. The `?` on the absent key must short-circuit before any arithmetic.
    #[test]
    fn sigma_at_absent_expiry_returns_none() {
        let surface = SVISurface::default();
        assert!(
            surface
                .sigma_at(Strike(50_000.0), Expiry { unix_ms: 1_700_000_000_000 }, 0)
                .is_none()
        );
    }

    // WHY: eval is the money-path formula both legs share; pin it against a hand-computed value.
    // At-the-money (strike == forward) => k = 0, d = -m = 0, so w = a + b*sigma = 0.04 + 0.4*0.1
    // = 0.08. With T = 0.25y, annualized sigma = sqrt(0.08/0.25) = sqrt(0.32) = 0.565685..., so
    // VolPoints = 56.5685... Pin to 1e-9.
    #[test]
    fn sigma_at_atm_matches_hand_computation() {
        let mut surface = SVISurface { as_of_ms: 0, per_expiry: BTreeMap::new() };
        let now = 0u64;
        let expiry = (MS_PER_YEAR * 0.25) as u64; // T = 0.25 years
        surface.per_expiry.insert(expiry, smile());
        let vp = surface
            .sigma_at(Strike(64_000.0), Expiry { unix_ms: expiry }, now)
            .expect("smile present, not expired");
        assert!((vp.0 - 56.568_542_494_923_804).abs() < 1e-9, "got {}", vp.0);
    }

    // WHY: an expired option (T <= 0) must not produce a bogus huge vol or NaN; the router must
    // see None and skip, not act on garbage.
    #[test]
    fn sigma_at_expired_returns_none() {
        let mut surface = SVISurface { as_of_ms: 0, per_expiry: BTreeMap::new() };
        surface.per_expiry.insert(1_000, smile());
        assert!(surface.sigma_at(Strike(64_000.0), Expiry { unix_ms: 1_000 }, 1_000).is_none());
        assert!(surface.sigma_at(Strike(64_000.0), Expiry { unix_ms: 1_000 }, 2_000).is_none());
    }

    // WHY: staleness gate (§242) is what halts trading on a frozen feed; off-by-one here means
    // trading on stale vol. Pin the boundary exactly.
    #[test]
    fn is_stale_boundary() {
        let surface = SVISurface { as_of_ms: 1_000, per_expiry: BTreeMap::new() };
        assert!(!surface.is_stale(61_000, 60_000)); // age == 60_000, not > 60_000
        assert!(surface.is_stale(61_001, 60_000)); // age 60_001 > 60_000
    }
}
```

- [ ] **Step 2: Run the new core tests to verify they fail to compile (signature changed)**

Run: `cargo test -p volarb-core 2>&1 | head -30`
Expected: FAIL — `tests/serde_roundtrip.rs` and `lib.rs` no longer compile (`Smile` not exported, surface shape changed). This is expected; Steps 3–4 fix the call sites.

- [ ] **Step 3: Export `Smile` from `lib.rs`**

In `crates/volarb-core/src/lib.rs`, change line 12 from:

```rust
pub use svi::{SVIParams, SVISurface};
```
to:
```rust
pub use svi::{SVIParams, SVISurface, Smile, MS_PER_YEAR};
```

- [ ] **Step 4: Update the serde round-trip test for the new surface shape**

In `crates/volarb-core/tests/serde_roundtrip.rs`, change the import line 4-6 to add `Smile`:

```rust
use volarb_core::{
    Expiry, Position, Quote, SVIParams, SVISurface, Side, Smile, Strike, UsdcAmount, VolPoints,
};
```

and replace lines 44-55 (the surface construction) with:

```rust
    let mut surface = SVISurface { as_of_ms: 1_700_000_000_000, ..Default::default() };
    surface.per_expiry.insert(
        expiry.unix_ms,
        Smile {
            params: SVIParams { a: 0.04, b: 0.4, rho: -0.3, m: 0.0, sigma: 0.1 },
            forward: 64_000.0,
        },
    );
    roundtrip(&surface);
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p volarb-core`
Expected: PASS — all core tests (numeric + svi eval/stale + serde round-trip) green.

- [ ] **Step 6: Lint + format**

Run: `cargo clippy -p volarb-core -- -D warnings && cargo fmt -p volarb-core -- --check`
Expected: clean, no output diff.

- [ ] **Step 7: Commit**

```bash
git add crates/volarb-core/src/svi.rs crates/volarb-core/src/lib.rs crates/volarb-core/tests/serde_roundtrip.rs
git commit -m "feat(core): SVISurface gains forward-per-smile + as_of; implement sigma_at eval"
```

---

## Task 2: Pricing crate scaffolding — deps + `PricingError` + module decls

**Files:**
- Modify: `Cargo.toml` (root, `[workspace.dependencies]`)
- Modify: `crates/volarb-pricing/Cargo.toml`
- Modify: `crates/volarb-pricing/src/lib.rs`

- [ ] **Step 1: Add `statrs` to workspace dependencies**

In the root `Cargo.toml`, under `[workspace.dependencies]`, after the `async-trait = "0.1"` line add:

```toml
statrs = "0.18"
```

- [ ] **Step 2: Wire pricing crate dependencies**

Replace `crates/volarb-pricing/Cargo.toml` `[dependencies]` section with:

```toml
[dependencies]
volarb-core.workspace = true
statrs.workspace = true
thiserror.workspace = true
```

- [ ] **Step 3: Declare modules + error type in `lib.rs`**

Replace the contents of `crates/volarb-pricing/src/lib.rs` with:

```rust
//! volarb-pricing — L1 float-domain pricing: Black-Scholes binary inversion (HL leg) and the
//! Zeliade quasi-explicit SVI fitter. Zero IO; fully unit-testable. (L0 chain-parity + L3 parity
//! harness land in Plan B.) Design: `docs/specs/2026-05-30-volarb-pricing-svi-fitter-design.md`.

pub mod binary;
pub mod svi_fit;

// NOTE: `pub use svi_fit::fit_smile;` is added in Task 5 once `fit_smile` exists — adding it here
// would break compilation for Tasks 2–4.

use thiserror::Error;

/// Errors from the pure pricing layer. (Pricing has no venue IO, so no `VenueError` here —
/// that is the venue-trait boundary, ADR-003.)
#[derive(Debug, Error, PartialEq)]
pub enum PricingError {
    #[error("need at least 3 observations to fit a smile, got {0}")]
    TooFewPoints(usize),
    #[error("fit did not converge")]
    NonConvergent,
    #[error("degenerate input: {reason}")]
    Degenerate { reason: &'static str },
    #[error("invalid input: {reason}")]
    InvalidInput { reason: &'static str },
}
```

- [ ] **Step 4: Verify it builds**

The `binary`/`svi_fit` module files don't exist yet, so a build would fail with "file not found for module". Create empty stubs:

```bash
printf '//! BS binary digital pricing + implied-vol inversion (HL leg).\n' > crates/volarb-pricing/src/binary.rs
printf '//! Zeliade quasi-explicit SVI fit.\n' > crates/volarb-pricing/src/svi_fit.rs
```
Run: `cargo build -p volarb-pricing`
Expected: PASS — empty modules + `PricingError` compile; `fit_smile` is not re-exported yet (added in Task 5), so nothing dangles.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/volarb-pricing/Cargo.toml crates/volarb-pricing/src/lib.rs crates/volarb-pricing/src/binary.rs crates/volarb-pricing/src/svi_fit.rs
git commit -m "chore(pricing): add statrs/thiserror deps, PricingError, module scaffold"
```

---

## Task 3: `binary.rs` — Black-Scholes digital price

**Files:**
- Modify: `crates/volarb-pricing/src/binary.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/volarb-pricing/src/binary.rs`:

```rust
use statrs::distribution::{ContinuousCDF, Normal};

/// Standard normal CDF.
fn norm_cdf(x: f64) -> f64 {
    // Normal::new(0,1) is infallible for these args; unwrap is fine.
    Normal::new(0.0, 1.0).unwrap().cdf(x)
}

/// Price of a cash-or-nothing binary call (pays 1 if terminal price > strike), under
/// Black-Scholes with r = 0 priced off the forward `F`: `p = N(d2)`,
/// `d2 = (ln(F/K) - 0.5*sigma^2*T) / (sigma*sqrt(T))`. Returns `NaN` on non-positive inputs.
pub fn binary_price(forward: f64, strike: f64, t_years: f64, sigma: f64) -> f64 {
    if t_years <= 0.0 || sigma <= 0.0 || forward <= 0.0 || strike <= 0.0 {
        return f64::NAN;
    }
    let sqrt_t = t_years.sqrt();
    let d2 = ((forward / strike).ln() - 0.5 * sigma * sigma * t_years) / (sigma * sqrt_t);
    norm_cdf(d2)
}

#[cfg(test)]
mod tests {
    use super::*;

    // WHY: at-the-money (F == K) with r = 0, d2 = -0.5*sigma*sqrt(T) < 0, so the binary call is
    // worth strictly less than 0.5 — a sign error in d2 (the classic BS digital bug) would push
    // it above 0.5. Pin the direction and a hand value: sigma=0.8, T=0.25 => d2 = -0.5*0.8*0.5 =
    // -0.2, N(-0.2) = 0.42074...
    #[test]
    fn binary_price_atm_below_half() {
        let p = binary_price(100.0, 100.0, 0.25, 0.8);
        assert!(p < 0.5, "ATM binary call must be < 0.5, got {p}");
        assert!((p - 0.420_740_290_560_897).abs() < 1e-9, "got {p}");
    }

    // WHY: degenerate inputs must not panic or return Inf — callers branch on NaN.
    #[test]
    fn binary_price_degenerate_is_nan() {
        assert!(binary_price(100.0, 100.0, 0.0, 0.8).is_nan());
        assert!(binary_price(100.0, 100.0, 0.25, 0.0).is_nan());
        assert!(binary_price(0.0, 100.0, 0.25, 0.8).is_nan());
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p volarb-pricing binary::tests`
Expected: PASS (both tests). If the hand value differs, the d2 formula has a sign/scale bug — fix `binary_price`, not the test.

- [ ] **Step 3: Lint**

Run: `cargo clippy -p volarb-pricing -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/volarb-pricing/src/binary.rs
git commit -m "feat(pricing): Black-Scholes binary digital price"
```

---

## Task 4: `binary.rs` — implied-vol inversion (monotone branch)

**Files:**
- Modify: `crates/volarb-pricing/src/binary.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/volarb-pricing/src/binary.rs` (above the existing `#[cfg(test)] mod tests`, add the impl; add the tests inside the test module):

Impl (append after `binary_price`):

```rust
/// Bisect for sigma on `[lo, hi]` where `binary_price(.., sigma) - target` is monotone and
/// brackets a root. Returns `None` if not bracketed.
fn bisect_sigma(target: f64, forward: f64, strike: f64, t: f64, mut lo: f64, mut hi: f64) -> Option<f64> {
    let g = |s: f64| binary_price(forward, strike, t, s) - target;
    let (mut glo, ghi) = (g(lo), g(hi));
    if glo.abs() < 1e-12 { return Some(lo); }
    if ghi.abs() < 1e-12 { return Some(hi); }
    if glo * ghi > 0.0 { return None; }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        let gm = g(mid);
        if gm.abs() < 1e-12 || (hi - lo) < 1e-12 {
            return Some(mid);
        }
        if glo * gm < 0.0 {
            hi = mid;
        } else {
            lo = mid;
            glo = gm;
        }
    }
    Some(0.5 * (lo + hi))
}

/// Recover Black-Scholes implied vol from a binary-call price `p` in (0, 1).
///
/// A digital's vega changes sign, so price-vs-sigma is non-monotone for out-of-the-money calls
/// (`F < K`): it rises to a peak `p_max` at `sigma* = sqrt(2*ln(K/F)/T)` then falls, giving 0 or
/// 2 solutions. We return the **lower-vol branch** (`sigma <= sigma*`) and `None` when the target
/// exceeds `p_max` (unreachable on that branch). For `F >= K` price is monotone-decreasing in
/// sigma and the solution is unique. Returns `None` on out-of-range / degenerate inputs.
pub fn implied_vol_from_binary(p: f64, forward: f64, strike: f64, t_years: f64) -> Option<f64> {
    if !(p > 0.0 && p < 1.0) || t_years <= 0.0 || forward <= 0.0 || strike <= 0.0 {
        return None;
    }
    let c = (forward / strike).ln();
    let lo = 1e-6;
    if c >= 0.0 {
        // monotone decreasing in sigma; price -> ~1 at lo, -> ~0 at 50.0
        bisect_sigma(p, forward, strike, t_years, lo, 50.0)
    } else {
        let sigma_star = (2.0 * (-c) / t_years).sqrt();
        let p_max = binary_price(forward, strike, t_years, sigma_star);
        if p > p_max {
            return None;
        }
        bisect_sigma(p, forward, strike, t_years, lo, sigma_star)
    }
}
```

Tests (add inside `mod tests`):

```rust
    // WHY: round-trip is the contract — a vol priced and then recovered must come back. ATM
    // (F == K, monotone branch) pins the happy path.
    #[test]
    fn inversion_roundtrips_atm() {
        let sigma = 0.8;
        let p = binary_price(100.0, 100.0, 0.25, sigma);
        let recovered = implied_vol_from_binary(p, 100.0, 100.0, 0.25).expect("invertible");
        assert!((recovered - sigma).abs() < 1e-4, "got {recovered}");
    }

    // WHY: in-the-money (F > K) is monotone — recovery must be unique and exact.
    #[test]
    fn inversion_roundtrips_itm() {
        let sigma = 0.5;
        let p = binary_price(110.0, 100.0, 0.25, sigma);
        let recovered = implied_vol_from_binary(p, 110.0, 100.0, 0.25).expect("invertible");
        assert!((recovered - sigma).abs() < 1e-4, "got {recovered}");
    }

    // WHY: out-of-the-money digitals have a max price; a target above it has NO real vol. We must
    // return None, not a bogus root — trading on a fabricated IV is the failure we're preventing.
    #[test]
    fn inversion_above_otm_max_returns_none() {
        // F < K (OTM): peak price p_max < 0.5; ask for something above it.
        let sigma_star = (2.0 * (100.0_f64 / 90.0).ln() / 0.25).sqrt();
        let p_max = binary_price(90.0, 100.0, 0.25, sigma_star);
        assert!(implied_vol_from_binary(p_max + 0.05, 90.0, 100.0, 0.25).is_none());
    }

    // WHY: out-of-range / degenerate prices must yield None, never panic.
    #[test]
    fn inversion_boundary_inputs_return_none() {
        assert!(implied_vol_from_binary(0.0, 100.0, 100.0, 0.25).is_none());
        assert!(implied_vol_from_binary(1.0, 100.0, 100.0, 0.25).is_none());
        assert!(implied_vol_from_binary(0.5, 100.0, 100.0, 0.0).is_none());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p volarb-pricing binary::tests`
Expected: PASS (all 6 binary tests).

- [ ] **Step 3: Lint + commit**

```bash
cargo clippy -p volarb-pricing -- -D warnings
git add crates/volarb-pricing/src/binary.rs
git commit -m "feat(pricing): binary implied-vol inversion (lower-vol branch, None outside)"
```

---

## Task 5: `svi_fit.rs` — Zeliade quasi-explicit fitter

**Files:**
- Modify: `crates/volarb-pricing/src/svi_fit.rs`

- [ ] **Step 1: Write the implementation**

Replace `crates/volarb-pricing/src/svi_fit.rs` with:

```rust
//! Zeliade quasi-explicit SVI fit (design §4.3). Outer: 2D Nelder-Mead over (m, sigma). Inner:
//! closed-form 3x3 normal-equations solve over (a, c, d) for the linearized total variance
//! `w = a + d*y + c*sqrt(y^2+1)`, `y = (k-m)/sigma`, then feasibility projection onto the no-arb
//! polytope. Back-transform: `b = c/sigma`, `rho = d/c`.

use crate::PricingError;
use volarb_core::{Expiry, MS_PER_YEAR, SVIParams, Smile, Strike, VolPoints};

/// Fit a Gatheral raw-SVI smile to market `(strike, vol)` observations for one expiry.
///
/// `observations` are `(strike, VolPoints)` where `VolPoints` is annualized sigma x 100. Needs
/// >= 3 points and `now_ms < expiry`. Returns the fitted `Smile` (params + the given forward).
pub fn fit_smile(
    forward: f64,
    expiry: Expiry,
    now_ms: u64,
    observations: &[(Strike, VolPoints)],
) -> Result<Smile, PricingError> {
    if observations.len() < 3 {
        return Err(PricingError::TooFewPoints(observations.len()));
    }
    if forward <= 0.0 {
        return Err(PricingError::InvalidInput { reason: "forward <= 0" });
    }
    let t = (expiry.unix_ms as f64 - now_ms as f64) / MS_PER_YEAR;
    if t <= 0.0 {
        return Err(PricingError::InvalidInput { reason: "expiry <= now" });
    }

    // Market points -> (k, w) total-variance space.
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(observations.len());
    for (strike, vp) in observations {
        if strike.0 <= 0.0 {
            return Err(PricingError::InvalidInput { reason: "strike <= 0" });
        }
        let sigma = vp.0 / 100.0;
        if !sigma.is_finite() || sigma < 0.0 {
            return Err(PricingError::InvalidInput { reason: "non-finite/negative vol" });
        }
        let k = (strike.0 / forward).ln();
        let w = sigma * sigma * t;
        if !k.is_finite() || !w.is_finite() {
            return Err(PricingError::Degenerate { reason: "non-finite k or w" });
        }
        pts.push((k, w));
    }
    let max_w = pts.iter().map(|p| p.1).fold(0.0_f64, f64::max);

    // Initial (m, sigma) guess from the moneyness spread.
    let ks: Vec<f64> = pts.iter().map(|p| p.0).collect();
    let m0 = ks.iter().sum::<f64>() / ks.len() as f64;
    let var_k = ks.iter().map(|k| (k - m0) * (k - m0)).sum::<f64>() / ks.len() as f64;
    let s0 = var_k.sqrt().max(0.05);

    let objective = |params: [f64; 2]| -> f64 {
        let (m, sigma) = (params[0], params[1].abs().max(1e-6));
        let (_, sse) = inner_solve(&pts, m, sigma, max_w);
        sse
    };

    let best = nelder_mead_2d(objective, [m0, s0], 400);
    let (m, sigma) = (best[0], best[1].abs().max(1e-6));
    let (theta, sse) = inner_solve(&pts, m, sigma, max_w);
    if !sse.is_finite() {
        return Err(PricingError::NonConvergent);
    }
    let (a, d, c) = (theta[0], theta[1], theta[2]);
    let b = c / sigma;
    let rho = if c > 1e-12 { (d / c).clamp(-1.0, 1.0) } else { 0.0 };

    Ok(Smile {
        params: SVIParams { a, b, rho, m, sigma },
        forward,
    })
}

/// Inner: given (m, sigma), solve constrained linear LS for (a, d, c) minimizing
/// `sum (a + d*y_i + c*z_i - w_i)^2`, then project onto the no-arb polytope. Returns
/// (theta = [a, d, c], residual SSE at the projected theta).
fn inner_solve(pts: &[(f64, f64)], m: f64, sigma: f64, max_w: f64) -> ([f64; 3], f64) {
    // Design matrix rows phi_i = [1, y_i, z_i]; build normal equations M*theta = r.
    let mut mm = [[0.0_f64; 3]; 3];
    let mut r = [0.0_f64; 3];
    for &(k, w) in pts {
        let y = (k - m) / sigma;
        let z = (y * y + 1.0).sqrt();
        let phi = [1.0, y, z];
        for a in 0..3 {
            r[a] += phi[a] * w;
            for b in 0..3 {
                mm[a][b] += phi[a] * phi[b];
            }
        }
    }
    let mut theta = solve3(mm, r).unwrap_or([max_w.max(0.0), 0.0, 0.0]);

    // Project onto: 0 <= a <= max_w ; 0 <= c <= 4*sigma ; |d| <= min(c, 4*sigma - c).
    theta[2] = theta[2].clamp(0.0, 4.0 * sigma); // c
    let d_bound = theta[2].min(4.0 * sigma - theta[2]).max(0.0);
    theta[1] = theta[1].clamp(-d_bound, d_bound); // d
    theta[0] = theta[0].clamp(0.0, max_w.max(0.0)); // a

    // Residual SSE at projected theta.
    let mut sse = 0.0;
    for &(k, w) in pts {
        let y = (k - m) / sigma;
        let z = (y * y + 1.0).sqrt();
        let model = theta[0] + theta[1] * y + theta[2] * z;
        sse += (model - w) * (model - w);
    }
    (theta, sse)
}

/// Solve a 3x3 linear system via Gaussian elimination with partial pivoting. None if singular.
fn solve3(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        // pivot
        let mut piv = col;
        for r in (col + 1)..3 {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-15 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for r in (col + 1)..3 {
            let f = a[r][col] / a[col][col];
            for c in col..3 {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0.0; 3];
    for i in (0..3).rev() {
        let mut s = b[i];
        for j in (i + 1)..3 {
            s -= a[i][j] * x[j];
        }
        x[i] = s / a[i][i];
    }
    Some(x)
}

/// Minimal 2D Nelder-Mead simplex minimizer. Fixed iteration budget; returns best vertex.
fn nelder_mead_2d<F: Fn([f64; 2]) -> f64>(f: F, start: [f64; 2], iters: usize) -> [f64; 2] {
    let mut simplex = [
        start,
        [start[0] + 0.1, start[1]],
        [start[0], start[1] + 0.1],
    ];
    let mut fv = [f(simplex[0]), f(simplex[1]), f(simplex[2])];
    for _ in 0..iters {
        // order: best=0 .. worst=2
        let mut idx = [0, 1, 2];
        idx.sort_by(|&i, &j| fv[i].partial_cmp(&fv[j]).unwrap_or(std::cmp::Ordering::Equal));
        let (b, g, w) = (idx[0], idx[1], idx[2]);
        let centroid = [
            (simplex[b][0] + simplex[g][0]) / 2.0,
            (simplex[b][1] + simplex[g][1]) / 2.0,
        ];
        let reflect = [
            centroid[0] + (centroid[0] - simplex[w][0]),
            centroid[1] + (centroid[1] - simplex[w][1]),
        ];
        let fr = f(reflect);
        if fr < fv[b] {
            let expand = [
                centroid[0] + 2.0 * (centroid[0] - simplex[w][0]),
                centroid[1] + 2.0 * (centroid[1] - simplex[w][1]),
            ];
            let fe = f(expand);
            if fe < fr {
                simplex[w] = expand;
                fv[w] = fe;
            } else {
                simplex[w] = reflect;
                fv[w] = fr;
            }
        } else if fr < fv[g] {
            simplex[w] = reflect;
            fv[w] = fr;
        } else {
            let contract = [
                centroid[0] + 0.5 * (simplex[w][0] - centroid[0]),
                centroid[1] + 0.5 * (simplex[w][1] - centroid[1]),
            ];
            let fc = f(contract);
            if fc < fv[w] {
                simplex[w] = contract;
                fv[w] = fc;
            } else {
                // shrink toward best
                for &v in &[g, w] {
                    simplex[v] = [
                        (simplex[v][0] + simplex[b][0]) / 2.0,
                        (simplex[v][1] + simplex[b][1]) / 2.0,
                    ];
                    fv[v] = f(simplex[v]);
                }
            }
        }
    }
    let best = (0..3).min_by(|&i, &j| fv[i].partial_cmp(&fv[j]).unwrap()).unwrap();
    simplex[best]
}
```

Then re-add the re-export in `crates/volarb-pricing/src/lib.rs` — replace the NOTE comment added in Task 2 Step 3:

```rust
// NOTE: `pub use svi_fit::fit_smile;` is added in Task 5 once `fit_smile` exists — adding it here
// would break compilation for Tasks 2–4.
```
with:
```rust
pub use svi_fit::fit_smile;
```

- [ ] **Step 2: Write the failing tests**

Append to `crates/volarb-pricing/src/svi_fit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use volarb_core::SVISurface;

    const FWD: f64 = 64_000.0;

    fn truth() -> SVIParams {
        SVIParams { a: 0.04, b: 0.4, rho: -0.3, m: 0.0, sigma: 0.1 }
    }

    // Generate (strike, VolPoints) observations from a known smile at the given strikes.
    fn synth(params: &SVIParams, t: f64, strikes: &[f64], noise: &[f64]) -> Vec<(Strike, VolPoints)> {
        strikes
            .iter()
            .zip(noise.iter().chain(std::iter::repeat(&0.0)))
            .map(|(&strike, &n)| {
                let k = (strike / FWD).ln();
                let dd = k - params.m;
                let w = params.a + params.b * (params.rho * dd + (dd * dd + params.sigma * params.sigma).sqrt());
                let sigma = (w / t).sqrt();
                (Strike(strike), VolPoints(sigma * 100.0 + n))
            })
            .collect()
    }

    fn eval(params: &SVIParams, strike: f64, t: f64) -> f64 {
        let mut s = SVISurface { as_of_ms: 0, per_expiry: BTreeMap::new() };
        let expiry = (MS_PER_YEAR * t) as u64;
        s.per_expiry.insert(expiry, Smile { params: *params, forward: FWD });
        s.sigma_at(Strike(strike), Expiry { unix_ms: expiry }, 0).unwrap().0
    }

    // WHY (gold test): a fitter that cannot invert its own forward model is wrong regardless of
    // how it behaves on real data. We compare the *smile* (sigma_at), not raw params, because raw
    // SVI params are not unique — two param sets can describe the same smile.
    #[test]
    fn fit_recovers_known_smile_dense() {
        let t = 0.25;
        let expiry = Expiry { unix_ms: (MS_PER_YEAR * t) as u64 };
        let strikes = [56_000.0, 58_000.0, 60_000.0, 62_000.0, 64_000.0, 66_000.0, 68_000.0, 70_000.0, 72_000.0];
        let obs = synth(&truth(), t, &strikes, &[]);
        let fitted = fit_smile(FWD, expiry, 0, &obs).expect("fit");
        for &strike in &strikes {
            let want = eval(&truth(), strike, t);
            let got = eval(&fitted.params, strike, t);
            assert!((want - got).abs() < 0.5, "strike {strike}: want {want} got {got}");
        }
    }

    // WHY: ~5 strikes per sub-hour binary expiry is the real HL regime. Zeliade exists precisely
    // so a sparse fit stays well-posed; a 5-point fit must still track the smile.
    #[test]
    fn fit_sparse_five_points() {
        let t = 0.05;
        let expiry = Expiry { unix_ms: (MS_PER_YEAR * t) as u64 };
        let strikes = [60_000.0, 62_000.0, 64_000.0, 66_000.0, 68_000.0];
        let obs = synth(&truth(), t, &strikes, &[]);
        let fitted = fit_smile(FWD, expiry, 0, &obs).expect("fit");
        for &strike in &strikes {
            let want = eval(&truth(), strike, t);
            let got = eval(&fitted.params, strike, t);
            assert!((want - got).abs() < 1.0, "strike {strike}: want {want} got {got}");
        }
    }

    // WHY: the projection step must keep the smile inside the no-arb domain even under noise.
    // We assert |rho| < 1 and b >= 0 (Gatheral no-arb necessary conditions).
    #[test]
    fn fit_noisy_stays_no_arb() {
        let t = 0.25;
        let expiry = Expiry { unix_ms: (MS_PER_YEAR * t) as u64 };
        let strikes = [58_000.0, 60_000.0, 62_000.0, 64_000.0, 66_000.0, 68_000.0, 70_000.0];
        let noise = [0.8, -0.6, 0.5, -0.7, 0.4, -0.5, 0.6];
        let obs = synth(&truth(), t, &strikes, &noise);
        let fitted = fit_smile(FWD, expiry, 0, &obs).expect("fit");
        assert!(fitted.params.b >= 0.0, "b = {}", fitted.params.b);
        assert!(fitted.params.rho.abs() < 1.0, "rho = {}", fitted.params.rho);
    }

    // WHY: too-few / degenerate inputs must be typed errors, not panics or garbage smiles.
    #[test]
    fn fit_rejects_bad_input() {
        let expiry = Expiry { unix_ms: 1_000_000 };
        assert_eq!(
            fit_smile(FWD, expiry, 0, &[(Strike(60_000.0), VolPoints(50.0))]).unwrap_err(),
            PricingError::TooFewPoints(1)
        );
        let obs = synth(&truth(), 0.25, &[60_000.0, 64_000.0, 68_000.0], &[]);
        // expiry <= now
        assert!(matches!(
            fit_smile(FWD, Expiry { unix_ms: 0 }, 1_000, &obs).unwrap_err(),
            PricingError::InvalidInput { .. }
        ));
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p volarb-pricing svi_fit::tests`
Expected: PASS (4 tests). If `fit_recovers_known_smile_dense` exceeds tolerance, the inner solve or back-transform is wrong — debug `inner_solve`/`solve3` before loosening tolerances (loosening to hide a real fit bug violates Rule 9). The projection inner is approximate; if `fit_noisy_stays_no_arb` or the sparse test fails the band, escalate the inner to an exact active-set solve (design §4.3 note) rather than widening the band.

- [ ] **Step 4: Lint + commit**

```bash
cargo clippy -p volarb-pricing -- -D warnings
git add crates/volarb-pricing/src/svi_fit.rs crates/volarb-pricing/src/lib.rs
git commit -m "feat(pricing): Zeliade quasi-explicit SVI fitter (hand-rolled NM + normal-eq inner)"
```

---

## Task 6: Perf gate + monkey tests

**Files:**
- Modify: `crates/volarb-pricing/src/svi_fit.rs` (perf + monkey tests)
- Modify: `crates/volarb-pricing/src/binary.rs` (monkey tests)

- [ ] **Step 1: Add the perf test**

Append inside `svi_fit.rs` `mod tests`:

```rust
    // WHY: <10ms for a 50-point smile is a hard spec target (design §1.4 / spec §3.2 line 209).
    // Run in release (`cargo test --release`); debug builds are not representative.
    #[test]
    fn fit_50_points_under_10ms() {
        let t = 0.25;
        let expiry = Expiry { unix_ms: (MS_PER_YEAR * t) as u64 };
        let strikes: Vec<f64> = (0..50).map(|i| 50_000.0 + i as f64 * 600.0).collect();
        let obs = synth(&truth(), t, &strikes, &[]);
        let start = std::time::Instant::now();
        let _ = fit_smile(FWD, expiry, 0, &obs).expect("fit");
        let elapsed = start.elapsed();
        assert!(elapsed.as_millis() < 10, "50-pt fit took {elapsed:?} (target <10ms, run --release)");
    }
```

- [ ] **Step 2: Add monkey tests (try to break it)**

Append inside `svi_fit.rs` `mod tests`:

```rust
    // WHY (monkey): adversarial inputs must surface as None/Err, never panic. NaN/inf vol,
    // all-identical strikes (zero moneyness spread), and zero forward are the realistic ways a
    // bad upstream feed corrupts a fit.
    #[test]
    fn monkey_fit_never_panics() {
        let expiry = Expiry { unix_ms: 1_000_000 };
        // NaN vol
        let nan = vec![
            (Strike(60_000.0), VolPoints(f64::NAN)),
            (Strike(62_000.0), VolPoints(50.0)),
            (Strike(64_000.0), VolPoints(50.0)),
        ];
        assert!(fit_smile(FWD, expiry, 0, &nan).is_err());
        // all-same strike (degenerate moneyness) — must not panic; Err or a (possibly poor) Ok.
        let same = vec![
            (Strike(64_000.0), VolPoints(50.0)),
            (Strike(64_000.0), VolPoints(51.0)),
            (Strike(64_000.0), VolPoints(49.0)),
        ];
        let _ = fit_smile(FWD, expiry, 0, &same); // just must not panic
        // zero forward
        let ok = vec![
            (Strike(60_000.0), VolPoints(50.0)),
            (Strike(64_000.0), VolPoints(50.0)),
            (Strike(68_000.0), VolPoints(50.0)),
        ];
        assert!(fit_smile(0.0, expiry, 0, &ok).is_err());
    }
```

Append inside `binary.rs` `mod tests`:

```rust
    // WHY (monkey): inversion is fed live venue prices; NaN/extreme values must yield None.
    #[test]
    fn monkey_inversion_never_panics() {
        assert!(implied_vol_from_binary(f64::NAN, 100.0, 100.0, 0.25).is_none());
        assert!(implied_vol_from_binary(0.5, f64::NAN, 100.0, 0.25).is_none());
        assert!(implied_vol_from_binary(0.5, 100.0, 100.0, -1.0).is_none());
        assert!(binary_price(f64::NAN, 100.0, 0.25, 0.8).is_nan());
    }
```

- [ ] **Step 3: Run the full pricing suite in release (perf gate is release-only)**

Run: `cargo test -p volarb-pricing --release`
Expected: PASS — all binary + svi_fit + perf + monkey tests green, including `fit_50_points_under_10ms`.

- [ ] **Step 4: Full-workspace verification**

Run: `cargo build --workspace && cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check`
Expected: all green (the DAG/skeleton crates still compile; core + pricing tests pass).

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-pricing/src/svi_fit.rs crates/volarb-pricing/src/binary.rs
git commit -m "test(pricing): 50-point <10ms perf gate + monkey tests"
```

---

## Done criteria for Plan A

- [ ] `cargo test --workspace` green; `cargo test -p volarb-pricing --release` green (perf gate).
- [ ] `cargo clippy --workspace -- -D warnings` clean; `cargo fmt --check` clean.
- [ ] `SVISurface::sigma_at` has no `todo!()`; forward-per-smile + `as_of_ms` shipped.
- [ ] Zeliade fitter recovers a known smile (dense + sparse) and stays no-arb under noise.
- [ ] Binary inversion round-trips and returns `None` on the non-invertible branch.
- [ ] Monkey inputs never panic.

**Next:** Plan B (L0 chain-parity port + L3 parity harness). Gated on a `sui-decompile` spike of the chain `math`/`oracle`/`i64` source (design §4.1/§8). Written after the spike so its fixed-point port code is placeholder-free. Plan A's float `binary_price` is the reference that L3 will measure the chain basis against.
```
