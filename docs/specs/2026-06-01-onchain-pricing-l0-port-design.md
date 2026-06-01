# On-chain Pricing L0 Port — Design (Plan B, Part 1)

> Date: 2026-06-01
> Scope: **Part 1 only** — offline fixed-point port of the on-chain pricing path. The L3
> parity harness (frozen fixtures captured via `sui_devInspect`, tick-exact comparison to
> chain) is **deferred to Part 2 / a separate chat** and is explicitly out of scope here.
> Source of truth for formulas/constants: `docs/specs/2026-05-31-onchain-pricing-decompile-findings.md`.

## Goal

Port the on-chain `oracle::compute_price` path to Rust as pure integer fixed-point math, so the
bot can compute the **executable** binary price (price-space) the same way the chain does. This is
the L0 layer of the GTM pricing engine. The eventual *difference* between L0 (executable, integer
truncation) and L1 (our float fair value, `volarb-core::svi`) is the model basis = the arb edge
gate input — but **measuring that basis is Part 2**, not this part.

## Non-Goals (Part 1)

- No frozen fixtures, no `sui_devInspect`, no network calls, no L3 parity harness.
- No bit-exact-to-chain assertion. Part 1 verifies **self-consistency + hand-computed golden
  values**, NOT agreement with the live chain. (Stated loudly so "tests pass" is not misread as
  "parity proven".)
- No basis measurement, no router/risk wiring, no `OracleSVIUpdated` event ingestion.

## Placement

- Single new file: `crates/volarb-pricing/src/onchain.rs`; add `pub mod onchain;` to
  `crates/volarb-pricing/src/lib.rs`.
- **Zero new dependencies.** Pure `u64`/`u128` integer arithmetic. No `statrs`, no float.
- Rationale (decided in brainstorming): L0 is executable-pricing logic, peer to `binary.rs` /
  `svi_fit.rs`; the future basis computation (L0 vs L1) lives in pricing. Not in `volarb-core`
  (which holds domain newtypes + annualized SVI eval only).

## Constants

- `SCALE: u64 = 1_000_000_000` (1e9, == DeepBook `FLOAT_SCALING`).
- `MAX_U64: u64 = u64::MAX = 18_446_744_073_709_551_615` (matches DeepBook `constants::max_u64`).
- `LN2: u64 = 693_147_180`.
- `normal_cdf` regime breaks: `662_910_000` (A), `5_656_854_249` (B), hard clamp `8 * SCALE`.
- All polynomial constants (ln series, normal_cdf regime A/B) copied **verbatim** from the findings
  doc constants table, in the exact Horner op order (floor-truncation parity is op-order-sensitive).

## Types

```rust
/// Sign-magnitude integer mirroring the chain's `i64::I64`. Constructors normalize -0 -> +0.
pub struct I64 { magnitude: u64, is_negative: bool }

/// The subset of on-chain `oracle::OracleSVI` needed to price one expiry. All FP fields are 1e9-scaled.
pub struct OnchainOracle {
    pub forward: u64,         // prices.forward
    pub a: u64,               // svi.a
    pub b: u64,               // svi.b
    pub sigma: u64,           // svi.sigma
    pub rho: I64,             // svi.rho
    pub m: I64,               // svi.m
    pub settlement: Option<u64>, // settlement_price
}
```

## Error handling

On-chain `abort(n)` codes are **expected domain errors** for an arb bot (bad oracle state, degenerate
params), not "should never happen" bugs. Port them to a `Result<_, OnchainError>` so the bot can skip
a market gracefully instead of crashing. Fail loud, but recoverable.

```rust
pub enum OnchainError {
    MagnitudeOverflow,   // i64 add/mul/div overflow         (chain abort 0)
    LnZero,              // ln(x) with x == 0                 (chain abort 0 in ln)
    DivByZero,           // i64 div_scaled b.mag == 0         (chain abort 1)
    ExpOverflow,         // exp positive arg > 23.638...      (chain abort 1 in exp)
    SqrtDomain,          // sqrt b == 0 or b > 1e9            (chain abort 2)
    ForwardNonPositive,  // compute_nd2 F <= 0                (chain abort 3)
    BracketNegative,     // compute_nd2 bracket < 0           (chain abort 4)
    WNonPositive,        // compute_nd2 w <= 0                (chain abort 5)
}
```

Only ops the chain gives an abort code return `Result`: the `i64` ops, `ln` (abort on `x==0`),
`exp`, `sqrt`, and `compute_nd2`. DeepBook `mul`/`div` and `normal_cdf` have no on-chain abort in the oracle path and
return plain `u64` (the u128 intermediate keeps them in range; a `debug_assert!` guards the cast-back
so an out-of-range operand surfaces in tests rather than silently wrapping).

## Functions (ported in dependency order)

1. **`I64`**: `zero`, `from_u64`, `from_parts` (-0→+0), `neg`, `add`, `sub`,
   `mul_scaled`, `div_scaled`, `square_scaled`. u128 intermediates; integer floor division.
2. **DeepBook `mul`/`div`** (round-DOWN floor, u128 intermediate then cast to u64). Only these two
   DeepBook math fns are used by the oracle path.
3. **Predict math**: `ln(u64) -> Result<I64>`, `exp(&I64) -> Result<u64>`,
   `sqrt(a: u64, b: u64) -> Result<u64>`, `normal_cdf(&I64) -> u64`. Constants + op order verbatim.
   - `ln`: `x==1e9 → 0`; `x<1e9 → -ln(1e18/x)`; else normalize + atanh series.
   - `exp`: `mag==0 → 1e9`; positive arg overflow guard `<= 23_638_153_699`; range-reduce by `2^k`,
     Taylor 12 terms.
   - `sqrt`: in the oracle path always called with `b == 1e9` (inv == 1) → `floor(sqrt(a*1e9))`;
     bit-length initial guess + 7 Newton iterations + final `if x*x > a { x -= 1 }`.
   - `normal_cdf`: clamp `|x|>8e9`; regime A polynomial; regime B rational × `exp(-x²/2)`; symmetric
     fold for negative.
4. **`compute_nd2(&OnchainOracle, k: u64) -> Result<u64>`**: the formula in findings §`compute_nd2`.
   `compute_price(&OnchainOracle, strike: u64) -> Result<u64>` (settled → strict `>` ? 1e9 : 0).
   `binary_price_pair(&OnchainOracle, strike) -> Result<(u64, u64)>` = `(up, SCALE - up)`.

## Testing (offline only)

Per-function hand-pinned golden values:

- `I64`: `-0` normalizes to `+0`; `mul_scaled`/`div_scaled` sign rule + floor; `square_scaled` ≥ 0.
- `mul`/`div`: round-DOWN truncation (e.g. values whose exact product has a fractional 1e9 part).
- `ln(1e9) == 0`; `ln(x<1e9)` negative; a known `ln` value to a documented tolerance band (exact
  integer expected, recomputed by hand from the series — pinned, not approximate).
- `exp(0) == 1e9`; `exp(ln2) ≈ 2e9` (pinned exact integer); positive-overflow arg → `Err(ExpOverflow)`.
- `sqrt` of perfect squares exact; `sqrt(b==0)`/`b>1e9` → `Err(SqrtDomain)`.
- `normal_cdf(0) == 500_000_000`; `normal_cdf(±8e9)` clamps to `1e9`/`0`; regime A/B break points
  (`662_910_000`, `5_656_854_249`) — assert no value jump across the boundary (continuity within
  truncation).
- `compute_nd2` ATM: `K == F` → `k == 0` → hand-computed expected `N(d2)`.
- `compute_price` settled: `s > K → 1e9`, `s == K → 0` (strict), `s < K → 0`.

**Monkey tests** (test.md mandate — try to break it):

- Extreme strikes (1, `MAX_U64`), `forward == 0` → `Err(ForwardNonPositive)`.
- `w` driven non-positive / bracket negative → correct `Err`, never panic.
- `i64` magnitude overflow / div-by-zero → `Err`, never panic.
- `-0` inputs everywhere normalize.
- `normal_cdf` swept across both regime breaks → monotone non-decreasing in `x` (within floor
  truncation), no discontinuous jump.

Verification gate (must pass before commit): `cargo test -p volarb-pricing`,
`cargo clippy --workspace -- -D warnings`, `cargo fmt --check`.

## Out-of-scope marker for downstream

`onchain.rs` module doc comment will state explicitly: **golden-tested for self-consistency, NOT yet
parity-verified against the live chain (Part 2)**. The progress file gets the same caveat so no one
mistakes "L0 ported" for "L0 == chain proven".
