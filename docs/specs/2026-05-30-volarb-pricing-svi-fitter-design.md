# Design — `volarb-pricing` SVI Fitter + core `sigma_at` (TODO #5)

> Date: 2026-05-30
> Scope: Implement `volarb-pricing` (Black-Scholes binary inversion + Zeliade quasi-explicit SVI fit) and fill the deferred `SVISurface::sigma_at` evaluation in `volarb-core`.
> Parent spec: `docs/specs/2026-05-28-vol-arb-bot-spec.md` §3.2 / §4 (line 209: Gatheral raw SVI, no-arb constraints, <10ms / 50-point smile).
> Status: Approved (brainstorming), pending implementation plan.

## 1. Goal & Success Criteria

The off-chain engine must express both legs of the arb in one comparable unit (annualized implied vol). Predict's oracle hands us **raw SVI params directly** → we only need to *evaluate* σ(K,T). External venues (HL HIP-4) hand us **binary prices** → we must *invert* Black-Scholes to recover discrete vol points, then *fit* a smooth no-arb smile so the two surfaces can be compared at any strike.

**Success criteria (loop until all true):**

1. `cargo build -p volarb-pricing` and `cargo build -p volarb-core` green.
2. `SVISurface::sigma_at` returns evaluated annualized vol (no `todo!()`); existing core tests updated for the new signature and pass.
3. Fitter **self-inversion** test: points generated from a known SVI smile fit back to that smile within tolerance.
4. Fitter meets perf target: 50-point smile fit < 10 ms in release mode.
5. Binary inversion round-trips (price → σ → price) and returns `None` on the non-invertible branch.
6. `cargo clippy -p volarb-pricing -p volarb-core -- -D warnings` clean; `cargo fmt --check`.
7. Monkey tests (degenerate inputs) do not panic — they return `None` / `Err`.

**Non-goals:** real HL quote ingestion (TODO #6), router wiring, any IO/async (pricing stays pure), verifying the on-chain annualization convention against Predict's oracle (flagged UNVERIFIED, deferred to #6).

## 2. Architecture & Crate Boundaries

`eval` lives in **`volarb-core`**, not `volarb-pricing`. This is forced by the dependency DAG: `sigma_at` is a method on core's own `SVISurface`, and `volarb-pricing` depends on `volarb-core`. If eval lived in pricing, core would need to call pricing → circular dep. (This corrects the stray "eval lands in volarb-pricing" note in the §3.2 design — the *fitter* and *inversion* land in pricing; *eval* is intrinsic to the core type.)

```
volarb-core   (no new deps)        volarb-pricing  → depends on volarb-core
  svi.rs                             lib.rs   : re-exports + PricingError (thiserror)
    Smile { params, forward }        binary.rs: BS digital price + inv_bs_binary
    SVISurface { as_of_ms,           svi_fit.rs: Zeliade two-layer fit
                 per_expiry }                    cobyla + nalgebra + statrs (all pure-Rust)
    sigma_at(.., now_ms)
    is_stale(now_ms, max_age_ms)
```

## 3. Core changes (`volarb-core`)

### 3.1 Data model (storage struct change — high risk)

```rust
// svi.rs
pub struct Smile { pub params: SVIParams, pub forward: f64 }   // F is snapshot data, lives WITH params
pub struct SVISurface {
    pub as_of_ms: u64,                       // §242 staleness gate ONLY — NOT used for T
    pub per_expiry: BTreeMap<u64, Smile>,    // expiry_ms → Smile
}
```

Rationale (from brainstorming, production-DeFi model):
- **Forward travels with the smile.** SVI params are meaningless without the forward they were measured against (`k = ln(K/F)`). Splitting F into a free call-site argument invites cross-snapshot desync (params from snapshot N + forward from N+1). This is the classic vol-engine footgun; production surfaces are immutable snapshots of `{params, forward}` per expiry.
- **`as_of_ms` is for staleness, not T.** Spec §242 (`now − last_svi_update > 60s → halt`) needs the snapshot time. But time-to-expiry must be computed from a *live* clock at eval, never frozen, because the surface is re-evaluated every router tick and sub-hour T decays second-by-second.

### 3.2 Evaluation

```rust
impl SVISurface {
    /// Annualized implied vol at (strike, expiry), valued at now_ms.
    /// None if: no smile for that expiry, T ≤ 0 (expired), or w < 0 (defensive).
    pub fn sigma_at(&self, strike: Strike, expiry: Expiry, now_ms: u64) -> Option<VolPoints>;
    /// §242 staleness gate.
    pub fn is_stale(&self, now_ms: u64, max_age_ms: u64) -> bool;
}
```

Math:
```
k = ln(strike / forward)
w = a + b·(ρ·(k−m) + √((k−m)² + σ²))          // Gatheral raw total variance
T = (expiry.unix_ms − now_ms) / MS_PER_YEAR    // T ≤ 0 → None
σ_BS = √(w / T)                                 // w < 0 → None
return VolPoints(σ_BS · 100.0)
```

Constants / unit conventions (locked):
- `MS_PER_YEAR = 365 * 24 * 3600 * 1000` (calendar-day annualization). **UNVERIFIED** — must match Predict oracle's annualization; flagged like §3.2 on-chain precision, deferred to TODO #6. Using 365 as the working assumption.
- `VolPoints` carries annualized σ × 100 (e.g. `VolPoints(80.0)` = 80% vol). eval and fit agree on this unit.

### 3.3 Test fallout

The signature change touches two existing tests (call sites only):
- `sigma_at_absent_expiry_returns_none_without_panicking` → add `now_ms` arg.
- `serde_roundtrip` → construct `SVISurface` with `as_of_ms` + `Smile { params, forward }`.

## 4. `volarb-pricing`

### 4.1 `binary.rs` — Black-Scholes digital + inversion

Cash-or-nothing binary call, r ≈ 0 (crypto, sub-hour), priced off forward F:
```
d2 = (ln(F/K) − σ²T/2) / (σ√T)
p  = N(d2)                                  // N = standard normal CDF (statrs)
```
```rust
pub fn binary_price(forward: f64, strike: f64, t_years: f64, sigma: f64) -> f64;
pub fn implied_vol_from_binary(p: f64, forward: f64, strike: f64, t_years: f64) -> Option<f64>;
```
- Inversion: Brent 1D root-find on σ. `binary.rs` works in **raw σ (`f64`)**, unit-agnostic; conversion to `VolPoints` (σ×100) happens at the call boundary that feeds `fit_smile` (wired in TODO #6).
- ⚠️ A digital's vega changes sign → `p(σ)` is **non-monotonic** (0 or 2 solutions). Solve only on the monotone branch; boundary cases (`p → 0/1`, deep ITM/OTM, `T ≤ 0`) return `None`. Document the chosen branch.

### 4.2 `svi_fit.rs` — Zeliade quasi-explicit fit

```rust
pub fn fit_smile(
    forward: f64,
    expiry: Expiry,
    now_ms: u64,
    observations: &[(Strike, VolPoints)],   // market (strike, σ) points
) -> Result<Smile, PricingError>;
```
Algorithm:
1. Convert observations to `(kᵢ, wᵢ)`: `kᵢ = ln(strike/F)`, `wᵢ = (σᵢ/100)²·T`.
2. **Outer** (2D, non-linear): minimize fit residual over `(m, σ)` with `σ > 0`, using `cobyla` (pure-Rust derivative-free).
3. **Inner** (3-var, linear, per outer step): change of variables `y=(k−m)/σ`, `c=bσ`, `d=ρbσ` makes total variance **linear**: `w = a + d·y + c·√(y²+1)`. Solve constrained linear least-squares for `(a, c, d)` via `nalgebra` normal equations + active-set over the polytope:
   `0 ≤ c ≤ 4σ`, `|d| ≤ c`, `|d| ≤ 4σ − c`, `0 ≤ a ≤ max(wᵢ)`. These constraints guarantee butterfly no-arb by construction.
4. Back-transform: `b = c/σ`, `ρ = d/c` (ρ=0 if c=0), `(a, m, σ)` as-is → `SVIParams`. Wrap in `Smile { params, forward }`.
5. `< 3` observations / degenerate / non-convergent → `Err(PricingError)`.

Dependencies (all pure-Rust, zero C build deps — matters for later TEE/container cross-compile): `cobyla = "1"`, `nalgebra` (workspace-pinned), `statrs`, `thiserror` (workspace).

> Why Zeliade over direct 5-param COBYLA: with only ~5–15 strikes per sub-hour binary expiry, a direct 5-param fit is ill-posed (known flat/multi-modal landscape). Zeliade collapses 3 of the 5 params to a closed-form inner linear solve, leaving only 2 genuinely non-linear params — robust under sparse/noisy quotes, which is exactly the HL case. This is the production vol-desk standard (Zeliade Systems, "Quasi-Explicit Calibration of Gatheral's SVI model").

### 4.3 `PricingError` (thiserror)

Pricing is pure (no `VenueError`, which is the venue-trait boundary per ADR-003). Variants: `TooFewPoints`, `NonConvergent`, `Degenerate { reason }`, `InvalidInput { reason }`.

## 5. Testing (Rule 9 — encode WHY; test.md — monkey)

- **eval**: hand-computed σ for known params at a few k; assert `T ≤ 0 → None`. *Why:* eval is the money-path formula both legs share.
- **fitter self-inversion (gold test)**: generate `(strike, σ)` points from a known `Smile` → `fit_smile` → recovered params reproduce the smile within tol. *Why:* a fitter that can't invert its own forward model is wrong regardless of how it looks on real data.
- **fitter sparse**: 5-point case still converges to a sane smile. *Why:* this is the real HL regime.
- **fitter robustness**: points + noise → fit residual bounded; assert fitted params satisfy no-arb. *Why:* sparse + noisy is the failure mode Zeliade exists to handle.
- **binary round-trip**: `price → invert → price` within tol; non-monotone/no-solution → `None`; boundary `p→0/1`.
- **perf**: 50-point fit < 10 ms (release). *Why:* hard spec target (§3.2 line 209).
- **monkey**: all-same-strike, 1 point, NaN/inf σ, negative σ, `F = 0`, `T = 0` → `None`/`Err`, never panic.

## 6. Risk flags (carried into implementation)

| Risk | Disposition |
|------|-------------|
| `MS_PER_YEAR` annualization convention | UNVERIFIED vs Predict oracle; working assumption 365-day; re-verify in TODO #6. |
| Digital price non-monotone in σ | Known math trap; Brent on monotone branch only; `None` outside; covered by tests. |
| Zeliade inner active-set | Hardest math in this TODO; plan splits it into its own TDD step before wiring the outer loop. |
| Core storage struct change | High-risk (storage shape). Confined to `Smile`/`SVISurface`; downstream skeletons don't yet read these fields, so blast radius = the 2 core tests. |
