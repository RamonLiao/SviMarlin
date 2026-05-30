# Design — `volarb-pricing`: Layered Pricing Engine (TODO #5)

> Date: 2026-05-30
> Scope: A GTM-grade off-chain pricing engine for the arb bot — on-chain price parity (L0), SVI surface + Zeliade fit (L1), and a chain-parity harness (L3). Fills core `SVISurface::sigma_at`.
> Parent spec: `docs/specs/2026-05-28-vol-arb-bot-spec.md` §3.2 / §4.
> On-chain ground truth: Predict pkg `0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138` (testnet, Immutable), modules `oracle` / `math` / `i64` / `pricing_config` — ABI pulled & verified 2026-05-30 (see §7).
> Status: Approved (brainstorming), pending implementation plan.

## 0. Why layered (the GTM decision)

The naive design compares the two venues' **implied vols**. But the chain prices its own binaries with `oracle::compute_price` using its **own fixed-point `math` module** (`normal_cdf`/`sqrt`/`ln`/`exp`). Comparing a textbook-BS σ against a chain-derived σ folds **model basis** (fixed-point truncation, day-count, formula details) into the "edge" — for an arb bot that basis can BE the entire edge.

GTM-grade answer: don't assume the basis is small — **measure it and pin it with a test**. That is the difference between a hackathon pricer and a shippable product: anyone can compute σ; a product can *prove* its off-chain marks match the on-chain contract to N ticks. So the engine is layered, and the trade signal rides on chain-parity prices, not model vols.

```
L0  Parity pricer     faithful off-chain port of oracle::compute_price + math fixed-point
                      → Predict-leg fair price, tick-parity with chain. SOURCE OF TRUTH.
L1  Surface/analytics sigma_at eval (Predict IV) + Zeliade fit (HL smile), float domain
                      → cross-strike interpolation + 3D IV-surface viz. Product differentiator.
L3  Parity harness    golden vectors from chain compute_price → assert L0 within N ticks
                      → quantifies basis, guards against chain formula drift, GTM trust story.
---
L2  Signal (router, TODO #6/#7, NOT this TODO): price-space edge = executable_HL − executable_Predict,
    executable = fair ± spread (pricing_config). Uses L1 to interpolate, L0 to price.
```

**This TODO delivers L0 + L1 + L3.** No downgrade: eval + Zeliade fitter + binary inversion all ship inside L1; L0 + L3 wrap them with chain parity.

## 1. Success Criteria (loop until all true)

1. `cargo build -p volarb-pricing -p volarb-core` green; `clippy -- -D warnings` clean; `fmt --check`.
2. **L1 eval**: `SVISurface::sigma_at` returns annualized vol (no `todo!()`); existing core tests updated for new signature and pass.
3. **L1 fit**: self-inversion test (points from a known smile → fit → recovers it within tol); sparse 5-point + noisy cases converge and satisfy no-arb.
4. **L1 perf**: 50-point smile fit < 10 ms (release).
5. **L0 parity (the GTM gate)**: off-chain `predict_binary_price` matches on-chain `oracle::compute_price` within a documented tick tolerance across a strike grid (golden vectors pulled from chain). **The measured basis is recorded in the spec/test, not assumed.**
6. **L1 inversion**: binary price→σ→price round-trips; non-monotone/boundary → `None`.
7. **Monkey**: degenerate inputs (NaN/inf/neg σ, F=0, T=0, 1 point, all-same-strike) never panic — return `None`/`Err`.

**Non-goals:** L2 router/edge logic, real HL quote ingestion, gRPC data path (all TODO #6/#7). No IO/async in pricing — golden vectors are captured to a fixtures file, the parity test reads the fixture (the RPC pull is a dev-time tool, not a crate dependency).

## 2. Crate layout & boundaries

`eval` lives in **`volarb-core`** (DAG-forced: `sigma_at` is a method on core's `SVISurface`; pricing depends on core, so eval can't live in pricing without a cycle). L0/L1-fit/inversion + parity live in **`volarb-pricing`**.

```
volarb-core (no new deps)            volarb-pricing → depends on volarb-core
  svi.rs                               lib.rs       : re-exports + PricingError (thiserror)
    Smile { params, forward }          fixedpoint.rs: faithful port of i64::I64 + math
    SVISurface { as_of_ms,                            (normal_cdf/sqrt/ln/exp), scale from source
                 per_expiry }          predict.rs   : L0 — port of oracle::compute_price
    sigma_at(.., now_ms)               binary.rs    : L1 — BS digital + inv_bs_binary (HL leg)
    is_stale(now_ms, max_age_ms)       svi_fit.rs   : L1 — Zeliade two-layer fit
                                       tests/parity.rs + fixtures/ : L3 — golden-vector parity
```
New deps (all pure-Rust, zero C build — matters for later TEE/container cross-compile): `cobyla`, `nalgebra`, `statrs`, `thiserror` (workspace).

## 3. Core changes (`volarb-core`) — L1 eval

### 3.1 Data model (storage struct change — high risk; mirrors chain)

```rust
pub struct Smile { pub params: SVIParams, pub forward: f64 }   // F lives WITH params
pub struct SVISurface {
    pub as_of_ms: u64,                       // staleness gate ONLY (§242) — NOT used for T
    pub per_expiry: BTreeMap<u64, Smile>,    // expiry_ms → Smile
}
```
**This is not just best practice — it mirrors the chain.** Each on-chain `oracle::OracleSVI` object = ONE expiry, carrying its own `prices.forward`, one `svi: SVIParams`, and a `timestamp` (§7). The engine subscribes to N such objects (one per expiry) and aggregates them into one `SVISurface`; each `Smile` ← one `OracleSVI` (`svi` params + `prices.forward`), and `as_of_ms` ← the oldest constituent `OracleSVI.timestamp`. Splitting forward out of the smile would let snapshot-N params pair with snapshot-(N+1) forward — the classic vol-engine desync.

### 3.2 Evaluation (float domain — analytics/viz, NOT the trade signal)

```rust
impl SVISurface {
    pub fn sigma_at(&self, strike: Strike, expiry: Expiry, now_ms: u64) -> Option<VolPoints>;
    pub fn is_stale(&self, now_ms: u64, max_age_ms: u64) -> bool;     // §242
}
// k=ln(strike/forward); w=a+b(ρ(k−m)+√((k−m)²+σ²)); T=(expiry−now)/MS_PER_YEAR; σ=√(w/T)
// None if: no smile / T≤0 / w<0. Returns VolPoints(σ·100).
```
- `MS_PER_YEAR = 365*24*3600*1000` — **the day-count used by L1 eval for display**. The *authoritative* day-count is whatever `oracle::compute_price` uses on-chain; L0 mirrors that, and L3 measures any gap. So this constant is a display convention, no longer a blocking UNVERIFIED — L3 quantifies its effect.
- `VolPoints` = annualized σ × 100 (80.0 = 80% vol). eval & fit agree.

### 3.3 Test fallout
Signature change touches two existing core tests (call sites): `sigma_at_absent_expiry_*` (+`now_ms`), `serde_roundtrip` (+`as_of_ms`, `Smile{params,forward}`).

## 4. `volarb-pricing`

### 4.1 L0 — `fixedpoint.rs` + `predict.rs` (chain parity)

**The thing that makes this GTM-grade.** Faithful off-chain port of the chain's pricing so the Predict leg is priced locally (no per-tick RPC latency) but provably tick-consistent with the contract.

- `fixedpoint.rs`: port `i64::I64` (sign-magnitude: `from_parts`/`magnitude`/`is_negative`/`*_scaled`) and `math` (`normal_cdf`, `sqrt(value,scale)`, `ln`, `exp`) at the **same fixed-point scale as on-chain**.
- `predict.rs`: `predict_binary_price(svi: &SVIParams, forward: u64, strike: u64, t: ...) -> u64`, a port of `oracle::compute_price` / `binary_price_pair`.
- **Source of truth = the Move source, not a guess.** Pull `math` / `oracle` / `i64` source via `sui-decompile` (per lessons.md: don't assume on-chain formulas/constants — read them). The SCALE constant and the exact `compute_price` formula come from that source. This is the bulk of the new work and the reason #5 grew.
- L0 operates in the chain's integer fixed-point (U64/I64), NOT f64 — that's how parity is achievable.

### 4.2 L1 — `binary.rs` (HL leg inversion)

Cash-or-nothing binary call, r≈0, off forward: `d2=(ln(F/K)−σ²T/2)/(σ√T)`, `p=N(d2)`.
```rust
pub fn binary_price(forward, strike, t_years, sigma) -> f64;          // float BS (HL leg / surface)
pub fn implied_vol_from_binary(p, forward, strike, t_years) -> Option<f64>;  // Brent root-find
```
`binary.rs` is **raw σ (f64)**, unit-agnostic; →`VolPoints` conversion at the call boundary (#6). ⚠️ Digital vega changes sign → `p(σ)` non-monotone (0/2 solutions); solve on the monotone branch only, boundary (`p→0/1`, deep ITM/OTM, `T≤0`) → `None`. (This is the HL→surface path; it is **not** on the trade-signal critical path, so its fragility is contained.)

### 4.3 L1 — `svi_fit.rs` (Zeliade quasi-explicit)

```rust
pub fn fit_smile(forward, expiry, now_ms, observations: &[(Strike, VolPoints)]) -> Result<Smile, PricingError>;
```
1. Observations → `(kᵢ,wᵢ)`: `kᵢ=ln(strike/F)`, `wᵢ=(σᵢ/100)²·T`.
2. **Outer** (2D non-linear): minimize residual over `(m,σ)`, `σ>0`, via `cobyla`.
3. **Inner** (linear, per outer step): substitution `y=(k−m)/σ`, `c=bσ`, `d=ρbσ` ⇒ `w=a+d·y+c·√(y²+1)` is **linear** in `(a,c,d)`. Constrained linear LS via `nalgebra` normal equations + active-set on the polytope `0≤c≤4σ, |d|≤c, |d|≤4σ−c, 0≤a≤max(wᵢ)` — these guarantee butterfly no-arb by construction.
4. Back-transform `b=c/σ, ρ=d/c (0 if c=0), (a,m,σ)` → `SVIParams` → `Smile{params,forward}`.
5. `<3` points / degenerate / non-convergent → `Err(PricingError)`.

> Zeliade over direct 5-param COBYLA: ~5–15 strikes per sub-hour binary expiry makes a direct 5-param fit ill-posed (flat/multi-modal landscape). Collapsing 3 params to a closed-form inner solve leaves 2 genuinely non-linear params → robust under sparse/noisy quotes. Production vol-desk standard (Zeliade Systems, "Quasi-Explicit Calibration of Gatheral's SVI model").

### 4.4 `PricingError` (thiserror)
Pure crate (no `VenueError` — that's the venue-trait boundary, ADR-003). Variants: `TooFewPoints`, `NonConvergent`, `Degenerate{reason}`, `InvalidInput{reason}`, `ParityScaleUnknown`.

## 5. L3 — Parity harness (`tests/parity.rs` + `fixtures/`)

The GTM gate. A dev-time tool pulls golden vectors from chain (`sui_getNormalizedMoveModule` for ABI + a devInspect/read of `oracle::compute_price` across a strike grid for a live `OracleSVI`) and writes them to `fixtures/compute_price_golden.json`. The committed test reads the fixture (no network in `cargo test`) and asserts:

```
for (strike, chain_price) in golden:
    assert |predict_binary_price(svi, forward, strike, t) − chain_price| ≤ TICK_TOL
```
- `TICK_TOL` is **recorded as the measured basis**, not assumed. If L0 is a faithful integer port, expect 0–1 tick.
- Also a property test: L0 monotonic/bounded where the chain is.
- Drift guard: re-running the puller later catches a chain formula change.

## 6. Testing (Rule 9 — encode WHY; test.md — monkey)

- **L1 eval**: hand-computed σ for known params; `T≤0→None`. *Why:* shared money-path formula.
- **L1 fit self-inversion (gold)**: known smile → fit → recovered within tol. *Why:* a fitter that can't invert its own forward model is wrong regardless of real-data look.
- **L1 fit sparse(5)/noisy**: converges, residual bounded, no-arb holds. *Why:* the real HL regime — Zeliade's reason to exist.
- **L1 binary**: round-trip; non-monotone/boundary→None.
- **L0 parity (gold)**: §5. *Why:* the trade signal trusts L0; basis must be measured, not hoped.
- **L1 perf**: 50-pt fit <10ms release. *Why:* hard target (§3.2 line 209).
- **Monkey**: all-same-strike, 1 pt, NaN/inf/neg σ, F=0, T=0 → None/Err, never panic.

## 7. On-chain ABI (verified 2026-05-30, pkg `0xf5ea…5138`)

```
oracle::OracleSVI { id, authorized_caps, underlying_asset:String, expiry:U64, active:Bool,
  prices: PriceData{spot:U64, forward:U64}, svi: SVIParams{a:U64,b:U64,rho:i64::I64,m:i64::I64,sigma:U64},
  timestamp:U64, settlement_price:Option<U64> }                         // one object = one expiry
oracle::compute_price(&OracleSVI, strike:U64) -> U64                     // fair binary price from SVI
oracle::binary_price_pair(&OracleSVI, strike:U64, &Clock) -> (U64,U64)   // uses Clock for T
oracle::{forward_price,spot_price,svi_a,svi_b,svi_m,svi_rho,svi_sigma,timestamp,expiry} -> accessors
oracle::OracleSVIUpdated { oracle_id, a:U64, b:U64, rho:I64, m:I64, sigma:U64, timestamp:U64 }  // event
math::{ normal_cdf(&I64)->U64, sqrt(U64,U64)->U64, ln(U64)->I64, exp(&I64)->U64, mul_div_round_* }
i64::{ from_parts, from_u64, magnitude, is_negative, is_zero, neg, add, sub, mul_scaled, div_scaled, square_scaled }
pricing_config::{ base_spread, min_spread, quote_spread_from_fair_price, min_ask_price, max_ask_price, utilization_multiplier }
```

## 8. Risk flags (carried into implementation)

| Risk | Disposition |
|------|-------------|
| L0 fixed-point fidelity (SCALE, exact `compute_price` formula) | **Read from decompiled `math`/`oracle` source** (sui-decompile), do NOT assume. L3 parity test is the acceptance gate. Hardest+largest part of this TODO. |
| `i64::I64` decode (sign-magnitude, scaled) | Port `from_parts`/`magnitude`/`is_negative`/`*_scaled` faithfully; unit-test against on-chain accessor outputs. |
| Digital price non-monotone in σ (L1) | Brent on monotone branch; `None` outside; contained off the trade-signal path. |
| Zeliade inner active-set | Hardest L1 math; plan isolates it as its own TDD step. |
| `MS_PER_YEAR` (L1 display day-count) | Display convention; authoritative day-count is L0/chain; L3 measures the gap. Downgraded from blocking. |
| Executable price includes spread (`pricing_config`) | L2/router scope (#6/#7): edge must net out Predict spread + HL fee. Flagged, not built here. |
| Core storage struct change | Confined to `Smile`/`SVISurface`; downstream skeletons don't read these yet → blast radius = 2 core tests. |
| L3 fixture freshness | Golden vectors are a point-in-time snapshot; re-pull on chain upgrade. Puller script committed under `tools/`. |
