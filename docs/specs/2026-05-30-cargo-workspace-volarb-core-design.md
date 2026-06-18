# Design — Cargo Workspace + `volarb-core` (TODO #4)

> Date: 2026-05-30
> Scope: Scaffold the Rust workspace (10 crates) and fully implement `volarb-core` types.
> Parent spec: `docs/specs/2026-05-28-vol-arb-bot-spec.md` §3.1 / §4.
> Status: Approved (brainstorming), pending implementation plan.

## 1. Goal & Success Criteria

Build the Rust off-chain engine skeleton so that subsequent crate work (TODO #5 pricing, #6 venues) has a compiling foundation and a shared type vocabulary.

**Success criteria (loop until all true):**

1. `cargo build` (workspace) is green — all 10 crates compile.
2. `cargo test -p volarb-core` passes (type round-trip + conversion tests).
3. The dependency DAG from spec §3.1 / `module-dependency.mmd` is enforced at compile time: e.g. `volarb-venues` cannot `use volarb_executor::*` (no such path dep exists).
4. `cargo clippy --workspace -- -D warnings` is clean.
5. `volarb-core` exposes `Strike`, `Expiry`, `SVISurface`/`SVIParams`, `Quote`, `Position` plus supporting newtypes, all `Serialize`/`Deserialize`.

Non-goals: SVI evaluation math (#5), VenueAdapter impls (#6), any IO/async in core.

## 2. Workspace Structure

```
01-vol-arb-bot/
├── Cargo.toml            # [workspace] + [workspace.dependencies]
├── rust-toolchain.toml   # pin channel = "1.94.1"
├── crates/
│   ├── volarb-core/      # ONLY crate with real implementation in this TODO
│   ├── volarb-pricing/
│   ├── volarb-venues/
│   ├── volarb-router/
│   ├── volarb-risk/
│   ├── volarb-executor/
│   ├── volarb-sui/
│   ├── volarb-indexer/
│   ├── volarb-rpc/
│   └── volarb-bin/
└── move/                 # existing, untouched
```

- **Edition 2024** (stable since 1.85; toolchain is 1.94.1).
- **Centralized versions** in `[workspace.dependencies]`. Declared now (even if unused by core): `serde`, `thiserror`. Others (`tokio`, `async-trait`, `nlopt`, `tonic`, `sqlx`) declared as workspace deps but only pulled in by the owning crate when implemented.
- `volarb-venues` is a **single crate with per-venue modules** (`hyperliquid`, `limitless`, `polymarket` v1, …), NOT nested sub-crates. VenueAdapter trait lives at its `lib.rs` root.

### 2.1 Dependency DAG (internal path deps)

From `docs/architecture/module-dependency.mmd` (MVP nodes only):

```
core      → (no internal deps)
pricing   → core
venues    → core
risk      → core
router    → pricing, venues, risk
sui       → core
indexer   → core
executor  → router, sui, venues, indexer
rpc       → pricing, risk, indexer
bin       → pricing, router, risk, executor
```

Wiring these as `path` deps in each `Cargo.toml` is the entire point: the crate boundary *is* the architecture boundary, enforced by the compiler (spec §3.1). A skeleton crate = correct `Cargo.toml` deps + `lib.rs` with `//!` module doc and `// TODO(#N)` markers, no logic.

## 3. `volarb-core` Types

Numeric strategy (decided in brainstorming): **layered** — pricing domain uses `f64` (native for SVI / Black-Scholes); on-chain amounts use `u64` newtypes (fixed-point). Conversions are centralized in `volarb-core`. Every public type derives `Serialize`/`Deserialize` because executor state (§3.5) and the indexer must round-trip through Postgres.

```rust
// --- numeric primitives ---
pub struct Strike(pub f64);                 // BTC price level
pub struct Expiry { pub unix_ms: u64 }      // Clock-based wall-clock ms (ADR-007), NOT epoch
pub struct UsdcAmount(pub u64);             // 6-decimal fixed point
pub struct VolPoints(pub f64);              // implied vol expressed in vol points (router edge unit)

impl UsdcAmount {
    pub const DECIMALS: u32 = 6;
    pub fn to_f64(self) -> f64;             // self.0 as f64 / 1e6
    pub fn from_f64(v: f64) -> Self;        // rounding convention documented + tested
}

// --- SVI surface (Gatheral raw parametrization, spec §3.2) ---
pub struct SVIParams { pub a: f64, pub b: f64, pub rho: f64, pub m: f64, pub sigma: f64 }
pub struct SVISurface { pub per_expiry: std::collections::BTreeMap<u64 /*expiry unix_ms*/, SVIParams> }
impl SVISurface {
    /// Evaluate total/implied vol at (strike, expiry). Body is `todo!()` — SVI eval lands in TODO #5.
    pub fn sigma_at(&self, strike: Strike, expiry: Expiry) -> Option<VolPoints>;
}

// --- market data ---
pub enum Side { Up, Down }                  // binary outcome leg
pub struct Quote {
    pub bid: f64, pub ask: f64,
    pub strike: Strike, pub expiry: Expiry,
    pub ts_ms: u64,
}

// --- position ---
pub struct Position {
    pub side: Side,
    pub size: UsdcAmount,
    pub entry_iv: VolPoints,
    pub strike: Strike,
    pub expiry: Expiry,
}
```

### 3.1 Scope decisions (pinned in brainstorming)

1. **VenueAdapter trait is NOT written in this TODO.** `volarb-venues/lib.rs` carries only the trait's doc-comment + `// TODO(#6)`. The trait's satellite types (`MarketRef`, `OrderId`, `PlaceOrder`, `OrderReceipt`, `ExtPosition`, `SettleReceipt`, `FeeModel`, `HealthStatus`, `VenueId`, `ChainKind`, `VenueError`) are defined in TODO #6 alongside real venue fields. Rationale: keep #4 a clean 1-day task and avoid inventing venue type fields from a mental model (cf. lessons.md — pull the real ABI / API before assuming shapes).

2. **`core` is pure data + one `todo!()` method.** `SVISurface::sigma_at` exists as a callable signature so pricing/router have something to target, but its body is `todo!()` (SVI eval = TODO #5).

### 3.2 On-chain precision boundaries (UNVERIFIED — sui-architect review 2026-05-30)

`Strike`/`Expiry`/`UsdcAmount` all carry an **implicit assumption about how they map to on-chain `predict::*` field types**, which is not yet verified against the real `market_key` ABI. Per lessons.md ("pull the real ABI before assuming shapes"), these are flagged loud (Rule 12) and resolved in TODO #6, NOT guessed now. `core` keeps its current `f64`/`u64` shapes; only the conversion layer is deferred.

| core type | on-chain target (lessons.md) | unverified assumption | resolved in |
|---|---|---|---|
| `Strike(f64)` | `market_key::up(oracle_id, strike, expiry)` — `strike` likely `u64` fixed-point | f64→u64 tick conversion + decimals/precision unknown | TODO #6 |
| `Expiry { unix_ms }` | `MarketKey.expiry` field | unit/type (ms vs s vs index) unconfirmed | TODO #6 |
| `UsdcAmount` `DECIMALS = 6` | `predict::mint<QuoteAsset>` collateral coin | testnet QuoteAsset may not be 6dp USDC | TODO #6 |

**Placeholder:** reserve a `StrikeTicks(u64)` newtype name for the on-chain strike representation; its real decimals are filled in TODO #6 after pulling the `market_key` ABI via `sui_getNormalizedMoveModulesByPackage`. Do not implement the `Strike ↔ StrikeTicks` conversion in TODO #4.

## 4. Tests (`volarb-core`)

Per CLAUDE.md Rule 9 (tests encode WHY):

- `usdc_amount_roundtrip`: `from_f64(to_f64(x)) == x` for representative amounts; documents+asserts the rounding convention (why: cross-chain leg sizing must not silently drift — money path).
- `usdc_amount_precision_boundary`: sub-cent / max-u64 edges (monkey test — why: 6dp truncation bug would mis-size positions).
- `serde_roundtrip`: each public type serializes→deserializes identically (why: executor crash-resume + indexer persistence depend on round-trip, §3.5).
- `svi_surface_lookup_present_absent`: `sigma_at` on an absent expiry returns `None` without panicking (the `todo!()` only fires on the eval path, not the lookup miss — documents the boundary).

## 5. Out of Scope / Follow-ups

- SVI eval math + nlopt COBYLA fitter → TODO #5.
- VenueAdapter trait + satellite types + adapter impls → TODO #6.
- **On-chain precision conversions (§3.2)** → TODO #6, after pulling `market_key` ABI:
  - `Strike(f64) ↔ StrikeTicks(u64)` — confirm strike fixed-point decimals.
  - `Expiry.unix_ms` ↔ `MarketKey.expiry` — confirm unit/type.
  - `UsdcAmount::DECIMALS` — confirm testnet `QuoteAsset` actually 6dp.
- Error enums per crate (`thiserror`) → defined when each crate gets logic.
- gRPC proto / `tonic` setup → TODO when `volarb-rpc` is implemented.
