# Cargo Workspace + volarb-core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Scaffold the 10-crate Rust workspace and fully implement `volarb-core` shared types.

**Architecture:** A Cargo workspace where the internal crate dependency graph (spec §3.1 / `module-dependency.mmd`) is wired as `path` deps so the compiler enforces one-way data flow. Only `volarb-core` gets real code this round (layered numerics: pricing-domain `f64` + on-chain `u64` newtypes, all serde-serializable); the other 9 crates are doc-only skeletons with correct deps.

**Tech Stack:** Rust 1.94.1, edition 2024, `serde` (derive), `serde_json` (dev), `thiserror` (declared for later crates).

> **Git note:** this directory is NOT a git repo yet. Either run `git init` once before starting (workspace code is fine to track — keep `.claude/` out per global rules) or treat each "Commit" step as an optional checkpoint and skip it. Build/test steps are the real gates.

> **Source of truth:** design spec `docs/specs/2026-05-30-cargo-workspace-volarb-core-design.md`. §3.2 of that spec flags `Strike`/`Expiry`/`UsdcAmount` on-chain precision as UNVERIFIED → resolved in TODO #6, not here.

---

## File Structure

```
Cargo.toml                          # [workspace] + [workspace.dependencies] (incl. internal path deps)
rust-toolchain.toml                 # pin 1.94.1
crates/
├── volarb-core/
│   ├── Cargo.toml
│   ├── src/lib.rs                   # module decls + re-exports + crate doc
│   ├── src/numeric.rs               # Strike, Expiry, UsdcAmount, VolPoints (+ unit tests)
│   ├── src/svi.rs                   # SVIParams, SVISurface::sigma_at (+ unit test)
│   ├── src/market.rs                # Side, Quote
│   ├── src/position.rs              # Position
│   └── tests/serde_roundtrip.rs     # cross-type JSON round-trip integration test
├── volarb-pricing/   { Cargo.toml, src/lib.rs }   # dep: core
├── volarb-venues/    { Cargo.toml, src/lib.rs }   # dep: core   (VenueAdapter trait doc only)
├── volarb-risk/      { Cargo.toml, src/lib.rs }   # dep: core
├── volarb-router/    { Cargo.toml, src/lib.rs }   # deps: pricing, venues, risk
├── volarb-sui/       { Cargo.toml, src/lib.rs }   # dep: core
├── volarb-indexer/   { Cargo.toml, src/lib.rs }   # dep: core
├── volarb-executor/  { Cargo.toml, src/lib.rs }   # deps: router, sui, venues, indexer
├── volarb-rpc/       { Cargo.toml, src/lib.rs }   # deps: pricing, risk, indexer
└── volarb-bin/       { Cargo.toml, src/main.rs }  # deps: pricing, router, risk, executor
```

---

## Task 1: Workspace root + first crate compiles

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `crates/volarb-core/Cargo.toml`
- Create: `crates/volarb-core/src/lib.rs`

- [ ] **Step 1: Write the workspace root `Cargo.toml`**

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
version = "0.1.0"
license = "MIT"

[workspace.dependencies]
# external
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
# internal (path deps — this is the architecture boundary, spec §3.1)
volarb-core = { path = "crates/volarb-core" }
volarb-pricing = { path = "crates/volarb-pricing" }
volarb-venues = { path = "crates/volarb-venues" }
volarb-risk = { path = "crates/volarb-risk" }
volarb-router = { path = "crates/volarb-router" }
volarb-sui = { path = "crates/volarb-sui" }
volarb-indexer = { path = "crates/volarb-indexer" }
volarb-executor = { path = "crates/volarb-executor" }
volarb-rpc = { path = "crates/volarb-rpc" }
```

- [ ] **Step 2: Write `rust-toolchain.toml`**

```toml
[toolchain]
channel = "1.94.1"
components = ["clippy", "rustfmt"]
```

- [ ] **Step 3: Write `crates/volarb-core/Cargo.toml`**

```toml
[package]
name = "volarb-core"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
serde.workspace = true

[dev-dependencies]
serde_json.workspace = true
```

- [ ] **Step 4: Write a minimal `crates/volarb-core/src/lib.rs`** (modules added in later tasks)

```rust
//! volarb-core — shared types for the vol-arb engine.
//!
//! Numeric strategy (design spec §3.2): pricing domain uses `f64`; on-chain amounts use
//! `u64` newtypes. On-chain precision conversions are deferred to TODO #6 (UNVERIFIED there).
```

- [ ] **Step 5: Verify the workspace compiles**

Run: `cargo build`
Expected: PASS — compiles `volarb-core` (one member; `crates/*` glob tolerates the other dirs not existing yet). If cargo complains about missing path-dep members, that's expected only once Task 6 references them — at this point no crate depends on them, so build is green.

- [ ] **Step 6: Commit** (optional checkpoint — see Git note)

```bash
git add Cargo.toml rust-toolchain.toml crates/volarb-core/Cargo.toml crates/volarb-core/src/lib.rs
git commit -m "chore: cargo workspace skeleton + volarb-core crate"
```

---

## Task 2: `volarb-core` numeric primitives (TDD)

**Files:**
- Create: `crates/volarb-core/src/numeric.rs`
- Modify: `crates/volarb-core/src/lib.rs`

- [ ] **Step 1: Write `crates/volarb-core/src/numeric.rs` with failing tests first**

```rust
use serde::{Deserialize, Serialize};

/// BTC price level (pricing domain, `f64`). On-chain strike conversion deferred to TODO #6
/// (`StrikeTicks(u64)`, design spec §3.2) — do NOT convert here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Strike(pub f64);

/// Expiry as Clock-based wall-clock unix milliseconds (ADR-007), NOT epoch time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Expiry {
    pub unix_ms: u64,
}

/// USDC amount in 6-decimal fixed point (on-chain `u64`). testnet QuoteAsset decimals
/// UNVERIFIED — see design spec §3.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsdcAmount(pub u64);

impl UsdcAmount {
    pub const DECIMALS: u32 = 6;
    const SCALE: f64 = 1_000_000.0; // 10^DECIMALS

    /// Fixed point → human USDC value.
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / Self::SCALE
    }

    /// Human USDC value → fixed point. Rounds to nearest tick (`f64::round`, half away from
    /// zero); non-positive inputs clamp to 0 because on-chain amounts are unsigned.
    pub fn from_f64(v: f64) -> Self {
        if v <= 0.0 {
            return UsdcAmount(0);
        }
        UsdcAmount((v * Self::SCALE).round() as u64)
    }
}

/// Implied vol expressed in vol points (router edge unit). `VolPoints(1.0)` == 1 vol point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VolPoints(pub f64);

#[cfg(test)]
mod tests {
    use super::*;

    // WHY: the Sui leg and the external venue leg are sized from the same USDC figure; if
    // ticks don't round-trip exactly, the two legs drift apart and the hedge is mis-sized.
    #[test]
    fn usdc_amount_roundtrip_at_tick_resolution() {
        for raw in [0u64, 1, 1_000_000, 123_456_789] {
            // values below 2^53 are exactly representable in f64, so the round-trip is exact.
            let a = UsdcAmount(raw);
            assert_eq!(UsdcAmount::from_f64(a.to_f64()), a, "raw={raw}");
        }
    }

    // WHY: a 6dp rounding/truncation bug mis-sizes positions on the money path. Pin the
    // rounding DIRECTION with values away from the exact .5 boundary (avoids f64 repr flake).
    #[test]
    fn usdc_amount_rounding_and_clamp() {
        assert_eq!(UsdcAmount::from_f64(0.0000016).0, 2); // 1.6 ticks -> 2
        assert_eq!(UsdcAmount::from_f64(0.0000013).0, 1); // 1.3 ticks -> 1
        assert_eq!(UsdcAmount::from_f64(-5.0), UsdcAmount(0)); // unsigned clamp
        assert_eq!(UsdcAmount(1).to_f64(), 0.000_001);
    }
}
```

- [ ] **Step 2: Register the module — modify `crates/volarb-core/src/lib.rs`**

Add below the crate doc:

```rust
pub mod numeric;

pub use numeric::{Expiry, Strike, UsdcAmount, VolPoints};
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p volarb-core numeric`
Expected: PASS — `usdc_amount_roundtrip_at_tick_resolution`, `usdc_amount_rounding_and_clamp`.

- [ ] **Step 4: Commit** (optional)

```bash
git add crates/volarb-core/src/numeric.rs crates/volarb-core/src/lib.rs
git commit -m "feat(core): numeric primitives Strike/Expiry/UsdcAmount/VolPoints"
```

---

## Task 3: `volarb-core` SVI types (TDD)

**Files:**
- Create: `crates/volarb-core/src/svi.rs`
- Modify: `crates/volarb-core/src/lib.rs`

- [ ] **Step 1: Write `crates/volarb-core/src/svi.rs` with the boundary test first**

```rust
use crate::numeric::{Expiry, Strike, VolPoints};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Gatheral raw SVI parameters for a single smile (one expiry). Design spec §3.2.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SVIParams {
    pub a: f64,
    pub b: f64,
    pub rho: f64,
    pub m: f64,
    pub sigma: f64,
}

/// Implied-vol surface: one SVI smile per expiry, keyed by expiry `unix_ms`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SVISurface {
    pub per_expiry: BTreeMap<u64, SVIParams>,
}

impl SVISurface {
    /// Implied vol at `(strike, expiry)`. Returns `None` if no smile exists for that expiry.
    /// SVI evaluation math lands in TODO #5 (`volarb-pricing`) — the eval path is `todo!()`.
    pub fn sigma_at(&self, strike: Strike, expiry: Expiry) -> Option<VolPoints> {
        let _params = self.per_expiry.get(&expiry.unix_ms)?;
        let _ = strike;
        todo!("SVI evaluation — TODO #5 (volarb-pricing)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // WHY: "no smile for this expiry" is normal control flow (router skips that venue), not a
    // panic. The `?` on the absent key must short-circuit BEFORE the unimplemented eval path.
    #[test]
    fn sigma_at_absent_expiry_returns_none_without_panicking() {
        let surface = SVISurface::default();
        let r = surface.sigma_at(Strike(50_000.0), Expiry { unix_ms: 1_700_000_000_000 });
        assert!(r.is_none());
    }
}
```

- [ ] **Step 2: Register the module — modify `crates/volarb-core/src/lib.rs`**

```rust
pub mod svi;

pub use svi::{SVIParams, SVISurface};
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p volarb-core svi`
Expected: PASS — `sigma_at_absent_expiry_returns_none_without_panicking` (the `todo!()` does not fire because the key is absent).

- [ ] **Step 4: Commit** (optional)

```bash
git add crates/volarb-core/src/svi.rs crates/volarb-core/src/lib.rs
git commit -m "feat(core): SVI surface types + sigma_at lookup boundary"
```

---

## Task 4: `volarb-core` market + position types

**Files:**
- Create: `crates/volarb-core/src/market.rs`
- Create: `crates/volarb-core/src/position.rs`
- Modify: `crates/volarb-core/src/lib.rs`

- [ ] **Step 1: Write `crates/volarb-core/src/market.rs`**

```rust
use crate::numeric::{Expiry, Strike};
use serde::{Deserialize, Serialize};

/// Binary outcome leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Up,
    Down,
}

/// A venue quote at a `(strike, expiry)` market point. `bid`/`ask` are binary prices in [0,1].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub bid: f64,
    pub ask: f64,
    pub strike: Strike,
    pub expiry: Expiry,
    pub ts_ms: u64,
}
```

- [ ] **Step 2: Write `crates/volarb-core/src/position.rs`**

```rust
use crate::market::Side;
use crate::numeric::{Expiry, Strike, UsdcAmount, VolPoints};
use serde::{Deserialize, Serialize};

/// An open position on one leg.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub side: Side,
    pub size: UsdcAmount,
    pub entry_iv: VolPoints,
    pub strike: Strike,
    pub expiry: Expiry,
}
```

- [ ] **Step 3: Register modules — modify `crates/volarb-core/src/lib.rs`**

```rust
pub mod market;
pub mod position;

pub use market::{Quote, Side};
pub use position::Position;
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p volarb-core`
Expected: PASS.

- [ ] **Step 5: Commit** (optional)

```bash
git add crates/volarb-core/src/market.rs crates/volarb-core/src/position.rs crates/volarb-core/src/lib.rs
git commit -m "feat(core): market (Side/Quote) + Position types"
```

---

## Task 5: cross-type serde round-trip (integration test)

**Files:**
- Create: `crates/volarb-core/tests/serde_roundtrip.rs`

- [ ] **Step 1: Write the integration test**

```rust
//! WHY: executor crash-resume (spec §3.5) and the indexer persist these types as JSON.
//! If any public type fails to round-trip, resumed state or indexed rows corrupt silently.

use volarb_core::{Expiry, Position, Quote, SVIParams, SVISurface, Side, Strike, UsdcAmount, VolPoints};

fn roundtrip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("serialize");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&back, value, "round-trip mismatch for {json}");
}

#[test]
fn all_public_types_json_roundtrip() {
    let strike = Strike(64_250.5);
    let expiry = Expiry { unix_ms: 1_700_000_000_123 };
    let amt = UsdcAmount(123_456_789);

    roundtrip(&strike);
    roundtrip(&expiry);
    roundtrip(&amt);
    roundtrip(&VolPoints(72.5));
    roundtrip(&Quote { bid: 0.51, ask: 0.53, strike, expiry, ts_ms: 1_700_000_000_000 });
    roundtrip(&Position { side: Side::Up, size: amt, entry_iv: VolPoints(72.5), strike, expiry });

    let mut surface = SVISurface::default();
    surface
        .per_expiry
        .insert(expiry.unix_ms, SVIParams { a: 0.04, b: 0.4, rho: -0.3, m: 0.0, sigma: 0.1 });
    roundtrip(&surface);
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p volarb-core --test serde_roundtrip`
Expected: PASS — `all_public_types_json_roundtrip`.

- [ ] **Step 3: Commit** (optional)

```bash
git add crates/volarb-core/tests/serde_roundtrip.rs
git commit -m "test(core): cross-type JSON round-trip (crash-resume/indexer invariant)"
```

---

## Task 6: scaffold the 9 skeleton crates + dependency DAG

Each crate = `Cargo.toml` (deps per the DAG) + a doc-only `lib.rs` (or `main.rs` for bin). No logic. The DAG wiring is the deliverable: it makes illegal imports a compile error.

**Files (create all):**
- `crates/volarb-pricing/{Cargo.toml, src/lib.rs}`
- `crates/volarb-venues/{Cargo.toml, src/lib.rs}`
- `crates/volarb-risk/{Cargo.toml, src/lib.rs}`
- `crates/volarb-router/{Cargo.toml, src/lib.rs}`
- `crates/volarb-sui/{Cargo.toml, src/lib.rs}`
- `crates/volarb-indexer/{Cargo.toml, src/lib.rs}`
- `crates/volarb-executor/{Cargo.toml, src/lib.rs}`
- `crates/volarb-rpc/{Cargo.toml, src/lib.rs}`
- `crates/volarb-bin/{Cargo.toml, src/main.rs}`

- [ ] **Step 1: `volarb-pricing`** — dep: core

`crates/volarb-pricing/Cargo.toml`:
```toml
[package]
name = "volarb-pricing"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
```
`crates/volarb-pricing/src/lib.rs`:
```rust
//! volarb-pricing — SVI fitting + Black-Scholes binary inversion (spec §3.2).
//! Zero IO; fully unit-testable. TODO #5: Gatheral fitter (nlopt COBYLA) + `SVISurface::sigma_at`.
```

- [ ] **Step 2: `volarb-venues`** — dep: core (carries the VenueAdapter trait doc)

`crates/volarb-venues/Cargo.toml`:
```toml
[package]
name = "volarb-venues"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
```
`crates/volarb-venues/src/lib.rs`:
```rust
//! volarb-venues — VenueAdapter trait + per-venue adapter modules.
//!
//! VenueAdapter is the architectural keystone (spec §4 / ADR-003): every external prediction
//! market implements it, so Router/Executor stay venue-agnostic by construction.
//!
//! TODO(#6): define the VenueAdapter trait + satellite types (MarketRef, OrderId, PlaceOrder,
//! OrderReceipt, ExtPosition, SettleReceipt, FeeModel, HealthStatus, VenueId, ChainKind,
//! VenueError) with REAL fields — pull the live venue API/ABI first (lessons.md), don't guess.
//!
//! MVP modules: hyperliquid, limitless. v1: polymarket, thales, opinion, binance_ec.
// pub mod hyperliquid;  // TODO(#6)
// pub mod limitless;    // TODO(#6)
```

- [ ] **Step 3: `volarb-risk`** — dep: core

`crates/volarb-risk/Cargo.toml`:
```toml
[package]
name = "volarb-risk"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
```
`crates/volarb-risk/src/lib.rs`:
```rust
//! volarb-risk — fractional-Kelly sizer + pre-trade gates + watchdogs/kill-switches (spec §3.4).
//! TODO: Kelly sizing, stale-SVI / Pyth-divergence / concentration / daily-loss gates.
```

- [ ] **Step 4: `volarb-router`** — deps: pricing, venues, risk

`crates/volarb-router/Cargo.toml`:
```toml
[package]
name = "volarb-router"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
volarb-pricing.workspace = true
volarb-venues.workspace = true
volarb-risk.workspace = true
```
`crates/volarb-router/src/lib.rs`:
```rust
//! volarb-router — spread detection + venue selection (spec §3.3).
//! Consumes pricing SVI surface + venue quotes + risk gates → emits a TradeIntent.
//! TODO #5+: `route(predict_iv, venues) -> Option<TradeIntent>` with MIN_EDGE_VOL_POINTS gate.
```

- [ ] **Step 5: `volarb-sui`** — dep: core

`crates/volarb-sui/Cargo.toml`:
```toml
[package]
name = "volarb-sui"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
```
`crates/volarb-sui/src/lib.rs`:
```rust
//! volarb-sui — Sui PTB builders + `predict::*` call wrappers (spec §5).
//! Owns the Strike/Expiry/UsdcAmount -> on-chain conversions (TODO #6, spec §3.2 — UNVERIFIED).
//! TODO: deposit + mint PTB, owned PredictManager version refetch on retry.
```

- [ ] **Step 6: `volarb-indexer`** — dep: core

`crates/volarb-indexer/Cargo.toml`:
```toml
[package]
name = "volarb-indexer"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
```
`crates/volarb-indexer/src/lib.rs`:
```rust
//! volarb-indexer — Postgres writer + replay engine (spec §3.5, ADR-005).
//! TODO: persisted CycleState transitions (u64 ms timestamps, NOT Instant), replay.
```

- [ ] **Step 7: `volarb-executor`** — deps: router, sui, venues, indexer

`crates/volarb-executor/Cargo.toml`:
```toml
[package]
name = "volarb-executor"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
volarb-router.workspace = true
volarb-sui.workspace = true
volarb-venues.workspace = true
volarb-indexer.workspace = true
```
`crates/volarb-executor/src/lib.rs`:
```rust
//! volarb-executor — cross-chain 2-leg state machine (spec §2.3 / §3.5, ADR-004).
//! TODO: CycleState (Idle..Settled/Aborted), 5s soft / 30s hard unwind, persist-before-side-effect.
```

- [ ] **Step 8: `volarb-rpc`** — deps: pricing, risk, indexer

`crates/volarb-rpc/Cargo.toml`:
```toml
[package]
name = "volarb-rpc"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
volarb-pricing.workspace = true
volarb-risk.workspace = true
volarb-indexer.workspace = true
```
`crates/volarb-rpc/src/lib.rs`:
```rust
//! volarb-rpc — gRPC server exposing read API for the dashboard (spec §3.1).
//! TODO: tonic service over pricing/risk/indexer state.
```

- [ ] **Step 9: `volarb-bin`** — deps: pricing, router, risk, executor (binary crate)

`crates/volarb-bin/Cargo.toml`:
```toml
[package]
name = "volarb-bin"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
volarb-core.workspace = true
volarb-pricing.workspace = true
volarb-router.workspace = true
volarb-risk.workspace = true
volarb-executor.workspace = true
```
`crates/volarb-bin/src/main.rs`:
```rust
//! volarb-bin — main binary: config loader + supervisor (spec §3.1, §3.5).
fn main() {
    // TODO: load config, wire pricing/router/risk/executor, supervise.
    println!("volarb-bin: not yet implemented");
}
```

- [ ] **Step 10: Build the whole workspace**

Run: `cargo build --workspace`
Expected: PASS — all 10 crates compile.

- [ ] **Step 11: Commit** (optional)

```bash
git add crates/volarb-pricing crates/volarb-venues crates/volarb-risk crates/volarb-router \
        crates/volarb-sui crates/volarb-indexer crates/volarb-executor crates/volarb-rpc crates/volarb-bin
git commit -m "chore: scaffold 9 skeleton crates wired to the dependency DAG"
```

---

## Task 7: full verification gate

- [ ] **Step 1: Workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 2: All tests**

Run: `cargo test --workspace`
Expected: PASS — 4 `volarb-core` tests (2 numeric, 1 svi, 1 serde integration); other crates have none.

- [ ] **Step 3: Clippy (deny warnings)**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS — clean. (Unused path-dep crates do NOT warn; `unused_crate_dependencies` is allow-by-default.)

- [ ] **Step 4: DAG enforcement smoke check (manual, then revert)**

Temporarily add to `crates/volarb-venues/src/lib.rs`:
```rust
use volarb_executor as _; // illegal per DAG
```
Run: `cargo build -p volarb-venues`
Expected: FAIL — `error[E0432]: unresolved import` / `use of undeclared crate volarb_executor`. This proves the compiler enforces the architecture boundary (venues cannot reach executor). **Revert the line** after observing the error.

- [ ] **Step 5: Format check**

Run: `cargo fmt --check`
Expected: PASS (or run `cargo fmt` then re-check).

- [ ] **Step 6: Final commit** (optional)

```bash
git add -A
git commit -m "chore: workspace verification gate green (build/test/clippy/fmt)"
```

---

## Self-Review

- **Spec coverage:** §2 workspace structure → Tasks 1, 6. §2.1 DAG → Task 6 deps + Task 7 Step 4 enforcement check. §3 core types → Tasks 2–4 (numeric/svi/market/position). §3.1 scope decisions (no trait, `todo!()` sigma_at) → Task 3 + Task 6 Step 2 doc. §3.2 unverified precision → annotated in `numeric.rs`/`volarb-sui` doc, not converted. §4 tests → Tasks 2/3/5. ✓ all sections mapped.
- **Placeholder scan:** no "TBD/implement later" in plan steps; every code step shows complete code. `todo!()` in `sigma_at` is intentional spec'd behavior, tested for the non-panicking path. ✓
- **Type consistency:** `Strike`/`Expiry{unix_ms}`/`UsdcAmount`/`VolPoints`/`SVIParams`/`SVISurface{per_expiry}`/`Side`/`Quote`/`Position` field names + `to_f64`/`from_f64`/`sigma_at`/`DECIMALS` signatures identical across Tasks 2–5 and the serde test. ✓
