# System Architecture Spec — Vol-Arb Bot

> **Project**: Vol-Arb Bot — Predict ↔ Multi-Venue Prediction-Market Vol Aggregator
> **Track**: Sui Overflow 2026 · Track 2 (DeepBook & Prediction Markets)
> **Spec date**: 2026-05-28
> **Scope**: MVP (hackathon) → v1 (Bot SaaS) → v2 (TEE-attested vault)
> **Companion docs**: [`../../BUSINESS_SPEC.md`](../../BUSINESS_SPEC.md) · [`../decisions/ADR-001-nav-oracle.md`](../decisions/ADR-001-nav-oracle.md)

---

## 0. How to read this spec

This is the **engineering source of truth** for the Vol-Arb Bot system. It describes WHAT components exist, HOW they interact, and WHERE the boundaries lie. The business rationale lives in [`BUSINESS_SPEC.md`](../../BUSINESS_SPEC.md); decision rationale for individual sub-systems lives in `docs/decisions/ADR-XXX.md`.

**Reader paths**:
- **Judges / first-time readers** → §1 Overview → §2 Diagrams → §10 Demo Path → §13 Threat Model. ~10 min.
- **Implementers** → §3 Off-Chain Engine → §4 VenueAdapter → §5 Cross-Chain Execution → §6 Sui Move Modules → §11 Module Map.
- **Auditors** → §6 Move Modules → §13 Threat Model → §14 Test Strategy → ADR-001.

---

## 1. System Overview

Vol-Arb Bot is a three-layer system that arbitrages implied-volatility (IV) spreads between **DeepBook Predict's on-chain SVI surface** and **N external prediction-market venues**.

```
                          ┌──────────────────────────────────────────────┐
                          │  Layer 3 — User Surface                       │
                          │  · Next.js dashboard (TS)                     │
                          │  · CLI / Telegram alerts (v1)                 │
                          │  · LP deposit/withdraw UI (v2 vault)          │
                          └────────────────┬─────────────────────────────┘
                                           │ gRPC + WebSocket
                          ┌────────────────▼─────────────────────────────┐
                          │  Layer 2 — Off-Chain Engine (Rust)            │
                          │  ┌──────────────┐  ┌──────────────────────┐  │
                          │  │ Pricing      │  │ VenueAdapter trait   │  │
                          │  │ Engine       │  │ ├─ Hyperliquid HIP-4 │  │
                          │  │ (SVI fit,    │  │ ├─ Limitless         │  │
                          │  │ BS binary)   │  │ ├─ Polymarket  (v1)  │  │
                          │  └──────┬───────┘  │ ├─ Thales      (v1)  │  │
                          │         │          │ ├─ Opinion     (v1)  │  │
                          │  ┌──────▼───────┐  │ └─ Binance EC  (v1)  │  │
                          │  │ Spread       │  └──────────┬───────────┘  │
                          │  │ Router       │             │              │
                          │  └──────┬───────┘  ┌──────────▼───────────┐  │
                          │         │          │ Cross-Chain Executor │  │
                          │  ┌──────▼───────┐  │ (state machine)      │  │
                          │  │ Risk Engine  │  └──────────┬───────────┘  │
                          │  │ (Kelly,      │             │              │
                          │  │ watchdogs)   │             │              │
                          │  └──────────────┘             │              │
                          │  ┌────────────────────────────▼───────────┐  │
                          │  │ Sui PTB Builder (sui-sdk Rust; sidecar node fallback) │
                          │  └─────────────────┬──────────────────────┘  │
                          │  ┌─────────────────▼──────────────────────┐  │
                          │  │ Postgres Indexer (Sui events + venues) │  │
                          │  └────────────────────────────────────────┘  │
                          └────────────────┬─────────────────────────────┘
                                           │ gRPC / JSON-RPC / WebSocket
       ┌───────────────────────────────────┼───────────────────────────────────┐
       │                                   │                                   │
┌──────▼────────────────┐    ┌─────────────▼──────────────┐    ┌──────────────▼─────────────┐
│ Layer 1A — Sui Testnet │    │ Layer 1B — Hyperliquid     │    │ Layer 1C — Base / Limitless │
│                        │    │  HIP-4 Testnet (Chain 998) │    │  (Base Sepolia)             │
│ · predict::mint        │    │ · /info, /exchange         │    │ · Limitless REST/WS         │
│ · predict::redeem      │    │ · EIP-712 signing          │    │ · Base JSON-RPC             │
│ · oracle::OracleSVI*   │    │                            │    │                             │
│ · DeepBook BalanceMgr  │    │                            │    │                             │
│ · Pyth BTC/ETH feed    │    │                            │    │                             │
│ · v2: vol_arb_vault    │    │                            │    │                             │
│ · v2: nautilus_verifier│    │                            │    │                             │
└────────────────────────┘    └────────────────────────────┘    └─────────────────────────────┘
```

**Key invariant**: the Sui leg is always real-on-chain testnet. The external leg is always real on its native chain's testnet (HL testnet Chain ID 998, Base sepolia). **No simulator anywhere on the MVP critical path** — this is what makes the demo land.

---

## 2. Architecture Diagrams

### 2.1 Module dependency graph

See [`../architecture/module-dependency.mmd`](../architecture/module-dependency.mmd) (renders on GitHub).

### 2.2 Arb-cycle data flow

```
OracleSVIUpdated event ─────────► Pricing Engine ──► fit SVI σ(K,T)
                                       │
                  ┌────────────────────┼─────────────────────┐
                  │                    │                     │
Hyperliquid /info WS ──► HL Adapter ──►│  ┌──────────────┐  │
Limitless WS         ──► LM Adapter ──►├─►│ Spread Router│──┤
[v1 venues...]       ──► [adapters] ──►│  └──────┬───────┘  │
                                       │         │           │
                                       │         ▼           │
                                       │   pick venue with   │
                                       │   max(|Δσ|−fees)    │
                                       │         │           │
                                       │  ┌──────▼───────┐   │
                                       │  │ Risk Engine  │───┤── halt if stale/divergent
                                       │  │ (Kelly size, │   │
                                       │  │  watchdogs)  │   │
                                       │  └──────┬───────┘   │
                                       │         │ approved  │
                                       │         ▼           │
                                       │  ┌──────────────┐   │
                                       │  │  Executor    │   │
                                       │  │ (2-leg SM)   │   │
                                       │  └──┬───────┬───┘   │
                                       │     │       │       │
                            Sui PTB ◄──┘     │       └──► VenueAdapter.place()
                            (predict::mint)                    │
                                                               │
                                       ▼                       ▼
                              ┌──────────────────────────────────────┐
                              │     Postgres Indexer (audit trail)   │
                              └──────────────────────────────────────┘
```

### 2.3 Cross-chain execution state machine

```
                  ┌───────────┐
                  │  Idle     │
                  └─────┬─────┘
                        │ spread > threshold + risk approved
                        ▼
                  ┌───────────┐
            ┌─────│ Preparing │  build PTB + pre-sign external order
            │     └─────┬─────┘
            │           │ both ready
            │           ▼
            │     ┌────────────────┐
            │     │ Sending        │  fire both legs in parallel (atomic Sui
            │     │ (5s soft /     │  PTB + signed HL/LM order). 0–5s: if exactly
            │     │  30s hard)     │  one leg filled, hold in single-leg watchdog.
            │     └─────┬──────────┘  At 5s still single-leg → Unwinding. See ADR-004.
            │           │
            │   ┌───────┼───────┐
            │   │       │       │
            │   ▼       ▼       ▼
            │ Both    Sui    External
            │ filled  only   only
            │   │       │       │
            │   │   ┌───▼───────▼────┐
            │   │   │   Unwinding    │  redeem unfilled side OR market-close
            │   │   │ (by 30s hard)  │  filled side; 30s measured from Sending entry
            │   │   └────────┬───────┘
            │   │            │
            │   ▼            ▼
            │ ┌───────┐ ┌──────────┐
            │ │ Live  │ │ Aborted  │  reason logged to indexer
            │ └───┬───┘ └────┬─────┘
            │     │          │
            │     │ hold to expiry / target P&L
            │     ▼          │
            │ ┌─────────┐    │
            │ │ Settling│    │
            │ └────┬────┘    │
            │      │         │
            │      ▼         ▼
            │   ┌───────────────┐
            └──►│  Settled      │ realized P&L → indexer
                └───────────────┘
```

The state machine is **persistent** — every transition writes to Postgres before firing the next side-effect. A crash mid-cycle resumes from the last committed state, never double-sends an order.

---

## 3. Off-Chain Engine (Rust)

### 3.1 Crate layout

```
crates/
├── volarb-core/         # types: Strike, Expiry, SVISurface, Quote, Position
├── volarb-pricing/      # SVI fitting, Black-Scholes binary inversion
├── volarb-venues/       # VenueAdapter trait + per-venue impls
│   ├── hyperliquid/
│   ├── limitless/
│   ├── polymarket/      # v1
│   ├── thales/          # v1
│   ├── opinion/         # v1
│   └── binance-ec/      # v1
├── volarb-router/       # spread detection + venue selection
├── volarb-risk/         # Kelly sizer, watchdogs, kill-switches
├── volarb-executor/     # cross-chain 2-leg state machine
├── volarb-sui/          # Sui PTB builders, predict::* call wrappers
├── volarb-indexer/      # Postgres writer, replay engine
├── volarb-rpc/          # gRPC server for dashboard, exposes read API
└── volarb-bin/          # main binary, config loader, supervisor
```

Why split this many crates? **Compile-time enforcement of the dependency graph.** `volarb-venues` cannot import from `volarb-executor` (one-way data flow); `volarb-pricing` has zero IO dependencies and is fully unit-testable. The crate boundary is the architecture boundary.

### 3.2 Pricing Engine

**Responsibility**: convert raw market data into a comparable `σ(K, T)` representation.

| Input | Source | Conversion |
|---|---|---|
| `OracleSVIUpdated` event | Sui indexer / `predict-server` | Direct SVI params → σ(K,T) for any (strike, expiry) |
| HL HIP-4 quote (binary bid/ask) | `api.hyperliquid-testnet.xyz/info` WS | Black-Scholes binary inversion: σ = inv_bs_binary(p, K, T, r, F) |
| Limitless / Polymarket quote | per-venue REST/WS | Same inversion, with venue-specific fee adjustment |

SVI fitting uses **Gatheral's raw parametrization** with constraint `b ≥ 0, |ρ| < 1, a + b·σ·√(1-ρ²) ≥ 0` (no-arb). Fitter is `nlopt` with COBYLA solver, runs in <10ms for 50-point smile.

### 3.3 Spread Router

**Algorithm** (pseudocode, runs every Pricing Engine tick):

```rust
fn route(predict_iv: SVISurface, venues: &[VenueQuote]) -> Option<TradeIntent> {
    let mut best: Option<(VenueId, f64, TradeIntent)> = None;
    for v in venues {
        // Compare both sides at the SAME (strike, expiry) — the venue's market point.
        let sigma_ext = v.implied_vol(v.strike, v.expiry)?;
        let delta_sigma = sigma_ext - predict_iv.sigma_at(v.strike, v.expiry);
        let edge = delta_sigma.abs() - v.fees_in_vol_points() - v.expected_slippage_in_vol_points();
        if edge > MIN_EDGE_VOL_POINTS {
            // as_ref(): TradeIntent is not Copy, so don't move `best` out of the Option.
            if best.as_ref().map_or(true, |(_, b, _)| edge > *b) {
                best = Some((v.id, edge, TradeIntent::from(v, predict_iv, delta_sigma)));
            }
        }
    }
    best.map(|(_, _, t)| t)
}
```

`MIN_EDGE_VOL_POINTS` default = 8 (from BUSINESS_SPEC). Per-venue fee model lives in each adapter.

### 3.4 Risk Engine

Pre-trade gates (all must pass):

| Gate | Trigger | Action |
|---|---|---|
| Stale SVI | `now - last_svi_update_ts > 60s` | Reject trade, halt new entries |
| Pyth divergence | `|pyth_btc − predict_implied_btc| / pyth_btc > 1%` | Reject, halt |
| Venue health | adapter `health()` returns degraded for >30s | Skip that venue |
| Kelly cap | `position_size > kelly_frac × capital` | Reject |
| Per-venue concentration | `exposure_to_venue > 50% × total` | Reject venue, try next |
| Daily loss limit | `realized_pnl_today < −2% × capital` | Halt for the day |
| Global kill-switch | manual or watchdog-triggered | Halt all entries, allow only redemptions |

Sizing: **fractional Kelly** with default `f = 0.25`. Formula:
```
size = f × (edge / variance) × capital
     bounded by: max_per_market, max_per_venue, max_total_notional
```

### 3.5 Cross-Chain Executor

Implements the §2.3 state machine. **Persistence first**: every state transition is written to Postgres in the same transaction that fires the side-effect. State enum:

```rust
enum CycleState {
    Idle,
    Preparing { intent: TradeIntent, ptb_draft: Bytes, ext_order_draft: ExtOrder },
    // Timestamps are wall-clock unix ms (u64), NOT std::time::Instant: Instant is
    // monotonic + process-local and cannot be serialized/restored, so a crash mid-cycle
    // would lose the deadline on resume. Persisted state must round-trip through Postgres.
    Sending  { sui_tx_digest: TxDigest, ext_order_id: OrderId, sent_at_ms: u64 },
    Live     { sui_position: PositionRef, ext_position: ExtPositionRef },
    Unwinding { reason: UnwindReason, deadline_ms: u64 },
    Settling { redeem_tx: TxDigest, ext_settle_id: SettleId },
    Settled  { realized_pnl_usd: f64 },
    Aborted  { reason: AbortReason },
}
```

**Failure modes & responses** (full timing semantics in [ADR-004](../decisions/ADR-004-unwind-window.md)):
- **0–5s soft window** — normal latency variance. Both legs confirmed → `Live`. Exactly one confirmed → enter `single-leg watchdog` sub-state, do **not** unwind yet.
- **at 5s, still single-leg → transition to `Unwinding`** (this is the one canonical trigger; §2.3 diagram + ADR-004 §2 agree). The Unwinding logic then runs until the 30s hard deadline: if Sui filled and external still pending → cancel external + redeem Sui; if external filled and Sui still pending → retry `predict::mint` once, fail → market-close external. External-leg-first close precedence (ADR-004 §2).
- **t > 30s** (measured from `Sending` entry) — still in `Unwinding` without resolution → transition to `Aborted`, page operator (PagerDuty + Telegram).
- **Both legs land but pricing moved >2σ during the window** → log slippage, hold position; risk engine may flatten on next tick.
- **`predict::mint` revert mid-PTB** → atomic, no partial state on Sui side; treat as "Sui not filled".

Owned-object caveat: `PredictManager` is a per-user **owned** object (see §6.1). On retry, the Executor MUST refetch the latest object version before rebuilding the PTB, otherwise the second attempt fails with `EObjectVersionMismatch`.

---

## 4. VenueAdapter Trait

This is the architectural keystone. Every external prediction market implements:

```rust
#[async_trait]
pub trait VenueAdapter: Send + Sync {
    fn id(&self) -> VenueId;
    fn chain(&self) -> ChainKind;  // Sui, Ethereum, Hyperliquid, BNB, CEX

    // Error type is VenueError (NOT anyhow::Error) so Router can distinguish
    // retryable vs terminal failures — see ADR-003 §2.
    async fn quote(&self, market: MarketRef) -> Result<Quote, VenueError>;
    async fn quote_stream(&self) -> BoxStream<'static, QuoteEvent>;

    async fn place(&self, order: PlaceOrder) -> Result<OrderReceipt, VenueError>;
    async fn cancel(&self, order_id: OrderId) -> Result<(), VenueError>;
    async fn position(&self, market: MarketRef) -> Result<Option<ExtPosition>, VenueError>;
    async fn settle(&self, market: MarketRef) -> Result<SettleReceipt, VenueError>;

    fn fees(&self) -> FeeModel;
    async fn health(&self) -> HealthStatus;
}
```

**Why these 6 action methods (not 12)?** (full trait = 6 action methods + `fees`/`health` + `id`/`chain` identity accessors; canonical definition in [ADR-003](../decisions/ADR-003-venue-adapter-trait.md))

- `quote` + `quote_stream` — needed by Pricing Engine
- `place` + `cancel` + `position` + `settle` — needed by Executor
- `fees` + `health` — needed by Router and Risk Engine

No read/write trait split because every adapter currently needs all of these. If a future venue becomes read-only, we split via subtrait, not via re-architecture.

### 4.1 Adapter delivery schedule

| Venue | Phase | Why this order |
|---|---|---|
| Hyperliquid HIP-4 | MVP | Best expiry match + lowest latency + working testnet |
| Limitless (Base) | MVP | Hot-standby; proves multi-venue thesis with N=2 |
| Polymarket | v1 | Largest TVL; v1 because Polygon mainnet (real $) + geofence |
| Thales Speed | v1 | 5-min binaries; complements HL granularity |
| Opinion (BNB) | v1 | Volume #3; bonding-curve mechanic interesting for stress testing |
| Binance Event Contracts | v1 | CEX leg for off-chain arb; KYC adds ops cost |

### 4.2 Adapter conformance tests

Every adapter ships with a `conformance_test!` macro that runs against the real testnet (mocked at unit-test level):

```rust
conformance_test!(HyperliquidAdapter, {
    test_quote_returns_within_500ms,
    test_place_then_cancel_roundtrip,
    test_position_query_after_fill,
    test_settle_after_expiry,
    test_health_responds_under_load,
});
```

Adding a new venue = writing one adapter struct + implementing the trait. Router and Executor are venue-agnostic by construction.

---

## 5. Sui PTB Construction

### 5.1 MVP arb-cycle PTB

```typescript
const tx = new Transaction();

// 1. Caller's PredictManager (per-user OWNED object — see §6.1).
//    Executor must use the latest object version on every retry.
const manager = tx.object(PREDICT_MANAGER_ID);

// 2. Source of collateral: either split from caller's USDC coin (default)
//    or borrow against DeepBook margin (leveraged path).
let collateral;
if (intent.use_margin) {
  // margin::borrow returns a Coin<USDC> (an ordinary value with `store`, NOT a
  // hot-potato — but we still consume it in this same PTB rather than leave it
  // dangling). ⚠️ UNVERIFIED: this DeepBook margin signature is assumed, not yet
  // confirmed against the on-chain ABI. Per lessons.md, pull the real ABI via
  // `sui_getNormalizedMoveModulesByPackage` before implementing — the real
  // margin::borrow likely needs a margin-manager/pool object, not just (BalanceManager, u64).
  collateral = tx.moveCall({
    target: `${DEEPBOOK_PKG}::margin::borrow`,
    arguments: [
      tx.object(BALANCE_MGR),
      tx.pure.u64(intent.borrow_amount_e6),
    ],
    typeArguments: [USDC_TYPE],
  });
} else {
  // split from caller's primary USDC coin. splitCoins returns an ARRAY of coins;
  // destructure the first so `collateral` is a single Coin in both branches.
  [collateral] = tx.splitCoins(tx.object(USDC_COIN_ID), [tx.pure.u64(intent.notional_e6)]);
}

// 3. Fund the manager. predict::mint does NOT take a Coin — Predict uses an
//    internal-custody model, so collateral must be deposited into the manager
//    BEFORE minting. (ABI: predict_manager::deposit<QuoteAsset>(manager, coin, ctx))
tx.moveCall({
  target: `${PREDICT_PKG}::predict_manager::deposit`,
  arguments: [manager, collateral],   // collateral Coin consumed here
  typeArguments: [USDC_TYPE],
});

// 4. Build the MarketKey (oracle id + strike + expiry + direction).
//    market_key::up()/down() are convenience ctors; new(id, strike, expiry, is_down)
//    is the explicit form. ORACLE_SVI_ID is the OracleSVI object's ID.
const marketKey = tx.moveCall({
  target: `${PREDICT_PKG}::market_key::${intent.side_is_up ? "up" : "down"}`,
  arguments: [
    tx.pure.id(ORACLE_SVI_ID),
    tx.pure.u64(intent.strike_e8),
    tx.pure.u64(intent.expiry_ms),
  ],
});

// 5. Mint the position. Returns NOTHING — position is recorded inside the
//    PredictManager's internal Table keyed by MarketKey. No outcome token.
//    (ABI: predict::mint<QuoteAsset>(predict, manager, oracle, key, quantity, clock, ctx))
tx.moveCall({
  target: `${PREDICT_PKG}::predict::mint`,
  arguments: [
    tx.object(PREDICT_ID),            // shared Predict object (NOT a Pool)
    manager,
    tx.object(ORACLE_SVI),
    marketKey,
    tx.pure.u64(intent.quantity),     // # of outcome units; cost quoted via get_trade_amounts
    tx.object("0x6"),                 // Clock shared object (id 0x6) — see ADR-007
  ],
  typeArguments: [USDC_TYPE],
});

// 6. Emit indexer enrichment event. Reads Clock for ms-precision timestamp.
tx.moveCall({
  target: `${VOLARB_PKG}::events::emit_arb_intent`,
  arguments: [
    tx.pure.string(intent.cycle_id),
    tx.pure.string(intent.venue_id),
    tx.object("0x6"),                 // Clock — see ADR-007
  ],
});

await suiClient.signAndExecuteTransaction({ transaction: tx, signer: keypair });
```

**Atomicity**: all moves succeed or all revert. If `margin::borrow` fails, `predict_manager::deposit` and `predict::mint` never fire. The Sui leg is therefore `deposit + mint` as one atomic unit, even though they are two `moveCall`s.
**Object refs**: `PredictManager` is owned → version-managed; rebuild PTB with fresh ref on retry. `Predict` (the shared market object, id `PREDICT_ID`), `OracleSVI`, `Clock(0x6)`, `BALANCE_MGR` are shared. `ORACLE_SVI_ID` (used to build the `MarketKey`) is the `OracleSVI` object's ID — read once at startup and cache.
**Verified against on-chain ABI**: package `0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138` (testnet, branch `predict-testnet-4-16`) via `sui_getNormalizedMoveModulesByPackage` on 2026-05-29. `predict::mint` returns nothing; `predict::deposit_outcome` does not exist.
**NOT yet verified**: the `${DEEPBOOK_PKG}::margin::borrow` signature in the margin branch above is assumed. Pull the real DeepBook margin ABI on-chain before implementing the leveraged path (see `lessons.md`, 2026-05-29).
**Sizing note**: `predict::mint`'s `quantity` is in outcome units; the *cost* is quoted separately via `predict::get_trade_amounts`. The PTB must derive `quantity` from the deposited collateral (or vice-versa) so the pre-deposited amount covers the mint — do NOT deposit `notional_e6` and mint an unrelated `quantity`.

### 5.2 v2 vault PTBs

Separate ADR-001 covers the vault Move surface. PTB skeletons:

- `deposit_to_vault`: `coin::split(usdsui) → vault::deposit → returns Coin<VOL_ARB_SHARE>`
- `withdraw_from_vault`: `vault::withdraw(shares) → returns Coin<USDSUI>`
- `post_nav_attestation`: `nautilus_verifier::verify(att) → vault::accept_nav`

---

## 6. Sui Move Modules

### 6.1 MVP — zero new Move modules required

The bot uses **existing** DeepBook Predict modules only:

> ABI verified on-chain 2026-05-29 against package `0xf5ea…5138` (testnet, `predict-testnet-4-16`). Exact signatures in §5.1.

| Module / Object | Ownership | Used by |
|---|---|---|
| `predict::mint`, `predict::redeem`, `predict::redeem_permissionless`, `predict::supply` | functions | Executor PTBs. All `<QuoteAsset>` generic. `mint`/`redeem` return void (internal custody); `supply` returns `Coin<plp::PLP>` |
| `predict::get_trade_amounts` | function (read) | Pre-trade cost quote `(u64, u64)` for sizing |
| `predict::Predict` | **shared** | the market object; passed as `&mut` to `mint`/`redeem` |
| `predict_manager::PredictManager` | **owned** (per-user) | one per signer; version-managed on retry. Created via `predict::create_manager` |
| `predict_manager::deposit`, `::withdraw`, `::position`, `::balance` | functions | fund manager before mint; read position size |
| `market_key::new` / `::up` / `::down` | functions | build the `MarketKey` (oracle id, strike, expiry, direction) passed to `mint` |
| `oracle::OracleSVI` | **shared** | passed as `&` (read) to `mint`/`redeem`; also source of live SVI params (`oracle::svi_*`) |
| `deepbook::balance_manager::BalanceManager` | **owned** (per-user) | collateral pool (margin path) |
| `deepbook::margin::*` functions | functions | optional leverage |
| `pyth::PriceInfoObject` | **shared** | Risk Engine cross-check |
| `sui::clock::Clock` (id `0x6`) | **shared** | ms-precision timestamp source |

**One helper module**, `volarb::events`, for indexer enrichment (full source):

```move
module volarb::events;

use std::string::String;
use sui::clock::Clock;
use sui::event;

public struct ArbIntent has copy, drop {
    cycle_id: String,
    venue_id: String,
    timestamp_ms: u64,
}

public fun emit_arb_intent(
    cycle_id: String,
    venue_id: String,
    clock: &Clock,
) {
    event::emit(ArbIntent {
        cycle_id,
        venue_id,
        timestamp_ms: clock.timestamp_ms(),
    });
}
```

This is the *only* custom Move on the MVP critical path. Uses `Clock` (not `TxContext::epoch_timestamp_ms()`) because the latter only returns epoch start — see [ADR-007](../decisions/ADR-007-clock-vs-epoch-timestamp.md).

### 6.2 v2 — Vault package (4 modules)

| Module | Responsibility | Key structs / functions |
|---|---|---|
| `vol_arb_vault` | Deposit, withdraw, share mint/burn, fee accrual | `Vault<phantom BaseCoin>` (shared), `VOL_ARB_SHARE` (OTW, fungible Coin), `deposit`, `withdraw`, `accrue_fees` |
| `nautilus_verifier` | Wraps `mysten::nautilus::attestation::verify`; adds project-specific `PcrWhitelist` + replay nonce | `verify_attestation`, `PcrWhitelist`, `Nonce` |
| `vault_admin` | UpgradeCap + parameter updates via Sui native multisig (see [ADR-006](../decisions/ADR-006-native-multisig.md)); emergency pause flag | `AdminCap`, `PauseFlag`, `update_config`, `pause`, `unpause` |
| `vault_events` | Indexer hooks | event structs for `Deposit`, `Withdraw`, `NavPosted`, `NavLive`, `DisputeRaised`, `DisputeResolved` |

**Type parameters**: `Vault<phantom BaseCoin>` where `BaseCoin = USDC` (mainnet) or `USDSUI` (alt path). `phantom` because the base coin balance lives in a `Balance<BaseCoin>` field, not as a generic value parameter.

**Share token (OTW)**: module name `vol_arb_share`, OTW struct `VOL_ARB_SHARE has drop {}` — module name and OTW struct name must match (`is_one_time_witness` checked at runtime). `coin::create_currency` is called once in `init`, treasury is custodied by the `Vault` shared object.

**`nautilus_verifier` MUST NOT re-implement AWS Nitro root certificate chain verification.** It wraps the official `mysten::nautilus` library (added as explicit dep in `Move.toml`). Project-specific additions are limited to: (a) PCR whitelist intersection check, (b) replay-protection nonce, (c) attestation freshness deadline.

**Vault is shared, not owned**: multiple LPs need concurrent deposit/withdraw, and the off-chain NAV oracle posts attestations as anyone-write (gated by attestation signature). Owned would force a single hot account.

Full design rationale → [`ADR-001-nav-oracle.md`](../decisions/ADR-001-nav-oracle.md). Move 2024 OTW pattern + native multisig choice → [`ADR-006`](../decisions/ADR-006-native-multisig.md).

### 6.3 Move 2024 / SUI v1.72.2 compliance

- All new modules use Move 2024 edition syntax (`public struct`, method syntax, positional fields where ergonomic).
- `TxContext` not required as last parameter (post-v1.72.2 flexibility).
- Display V2 used for `Coin<VOL_ARB_SHARE>` metadata (Display Registry `0xd`).
- DeepBook listed as explicit dependency in `Move.toml` (required since v1.47).

---

## 7. Data Layer

### 7.1 Read sources

| Source | Protocol | Used by | Why |
|---|---|---|---|
| Mysten `predict-server.testnet.mystenlabs.com` | gRPC + WS | Pricing Engine (SVI events), Indexer backfill | Official, zero-ops |
| Sui fullnode gRPC | gRPC (v1.72.2 primary) | Indexer, PTB builder | gRPC is GA; JSON-RPC deprecated April 2026 |
| Sui fullnode GraphQL | GraphQL (beta) | Dashboard ad-hoc queries | Beta, frontend ergonomics |
| Hyperliquid `/info` + `/exchange` | REST + WS | HL adapter | Native |
| Limitless REST/WS | REST + WS | LM adapter | Native |
| Pyth on Sui | Sui object reads | Risk Engine sanity | Authoritative BTC/ETH mark |
| Walrus (v2) | Walrus SDK | NAV attestation snapshot storage | Decentralized audit trail |

### 7.2 Custom Postgres indexer

Why we still need our own indexer despite Mysten's feed:
- **Backtest replay** requires multi-week point-in-time snapshots of `OracleSVIUpdated`; Mysten retention not guaranteed
- **Per-venue historical quotes** must be co-located with SVI history to enable spread backtests
- **Audit trail** for arb cycles needs cross-source join (Sui tx + HL fill + Limitless fill) in one place
- **Performance attribution** for v2 vault requires per-cycle realized P&L queryable by venue, strike, time bucket

Indexer schema (high-level):

```
oracle_svi_updates  (block, ts, strike_grid, sigma_grid, raw_params_jsonb)
venue_quotes        (venue, ts, market_ref, bid, ask, size_bid, size_ask)
arb_cycles          (cycle_id, opened_at, closed_at, state, venue_id, leg_sui_digest,
                     leg_ext_digest, realized_pnl_usd, fees_paid_usd, status_reason)
risk_events         (ts, gate, decision, payload_jsonb)
nav_attestations    (ts, attestation_blob, pcr, signer, accepted, dispute_id_nullable)  -- v2
```

Adaptive Concurrency (Sui v1.72.2) is used for the live processor; `ConcurrencyConfig` replaces deprecated `Processor::FANOUT`.

### 7.3 Backtest replay

Single command, deterministic:

```
volarb backtest --from 2026-04-01 --to 2026-04-14 \
                --venues hyperliquid,limitless \
                --kelly 0.25 --min-edge 8 --capital 100000
```

Reads from `oracle_svi_updates` + `venue_quotes`, simulates Router + Risk + Executor, writes `backtests/<run-id>/` with:
- equity curve CSV
- per-cycle trade log
- per-venue Sharpe / max DD
- HTML report (auto-opens in browser)

---

## 8. Dashboard (Next.js + TS)

### 8.1 Pages

| Route | Purpose | Data |
|---|---|---|
| `/` | Public IV surface viewer + 30d backtest equity curve | Read-only, no wallet |
| `/connect` | Sui wallet connect (Slush / Suiet), HL key paste (v1) | dApp Kit |
| `/strategy` | Kelly slider, threshold tuning, venue allowlist toggle | gRPC to engine |
| `/live` | Open positions, MtM, next expiry countdown, feeder LEDs, venue health | Engine WS push |
| `/backtest` | Run + view replays, downloadable CSV | Indexer + replay runner |
| `/vault` (v2) | Deposit / withdraw, NAV history, attestation list with PCR | Vault Move + indexer |
| `/dispute` (v2) | Raise dispute against pending attestation | Vault Move |

### 8.2 Stack

- Next.js 16 App Router (Server Components for static IV surface, Client Components for live tape)
- `@mysten/dapp-kit-react` for wallet integration
- `@mysten/sui` (Transaction class, not legacy TransactionBlock)
- TanStack Query for engine gRPC calls
- shadcn/ui + Tailwind for components
- Plotly for IV surface 3D plot, Recharts for time series
- Deployed on Vercel; engine gRPC via Vercel Functions or self-hosted

### 8.3 Performance budget

| Metric | Target |
|---|---|
| First Contentful Paint | < 1.2s |
| Live tape WS message latency | < 150ms p50 |
| IV surface plot interactive frame time | < 16ms |

---

## 9. Configuration & Secrets

### 9.1 Config layout

```
config/
├── default.toml      # public defaults, in git
├── testnet.toml      # overrides for testnet
├── mainnet.toml      # overrides for mainnet (v1+)
└── local.toml        # operator-local, gitignored
```

Schema-validated by `serde` at startup; missing required field = crash, never silent default.

### 9.2 Secrets management

| Secret | MVP | v1 | v2 |
|---|---|---|---|
| Sui signer keypair | env var | KMS-managed (AWS / GCP) | Inside Nautilus enclave for vault ops |
| Hyperliquid API key + secret | env var | KMS | Enclave |
| Limitless wallet key (Base) | env var | KMS | Enclave |
| Database password | env var | Vercel Marketplace secret | Same |

**Never log secrets**: lint rule (`forbid::secrets-in-logs`) plus structured logger redaction on field names matching `*key*|*secret*|*password*`.

---

## 10. Demo Path (Sui Overflow 2026)

Tied to BUSINESS_SPEC §12 7-min script. Engineering checkpoints:

| Demo beat | Tech that must work | Verification |
|---|---|---|
| 0:00 — IV surface 3D plot | Pricing Engine streaming, Plotly render | `cargo test pricing::svi_render` |
| 0:45 — Spread panel green | Router emits TradeIntent, HL adapter quote arriving | Indexer row in `arb_cycles` with state=Preparing |
| 2:00 — Click execute, Sui PTB lands | PTB builder, predict::mint, custom event | Sui explorer link with digest |
| 2:30 — HL leg fills on testnet | HL adapter place(), order receipt | `/info/userState` shows new position |
| 4:00 — Backtest equity curve | Replay engine, indexer 2-week snapshot | `backtests/demo-run/` exists, HTML opens |
| 5:30 — Trigger fake feeder lag | Watchdog fires, kill-switch triggers | Risk event written, dashboard LEDs red, positions auto-flatten |
| 6:30 — Vault deposit + dispute demo (stretch, if time) | Nautilus attestation, dispute Move call | Vault state transitions visible on chain |

**Fall-back demo**: if a venue testnet is degraded on demo day, pre-recorded screen capture of a successful arb cycle is shown alongside live components that *are* working. Better to admit one venue is down than to fake it.

---

## 11. Module → Owner / Effort Map (planning)

| Module | LOC est. | Effort (eng-days) | Owner | Priority |
|---|---|---|---|---|
| volarb-core | 800 | 2 | shared | P0 (MVP) |
| volarb-pricing | 1,200 | 4 | quant | P0 |
| volarb-venues/hyperliquid | 1,500 | 5 | backend | P0 |
| volarb-venues/limitless | 1,200 | 4 | backend | P0 |
| volarb-router | 600 | 2 | quant | P0 |
| volarb-risk | 900 | 3 | quant | P0 |
| volarb-executor | 1,800 | 6 | backend | P0 |
| volarb-sui | 1,000 | 3 | backend | P0 |
| volarb-indexer | 1,400 | 5 | backend | P0 |
| volarb-rpc | 700 | 2 | backend | P0 |
| dashboard (Next.js) | 3,000 | 7 | frontend | P0 |
| volarb::events Move module | 50 | 0.5 | backend | P0 |
| v1 venue adapters (4) | 4,000 | 16 | backend | P1 |
| Telegram alerts | 500 | 2 | backend | P1 |
| v2 vol_arb_vault Move | 1,200 | 6 | move dev | P2 |
| v2 nautilus_verifier Move | 600 | 3 | move dev | P2 |
| v2 vault_admin Move | 800 | 4 | move dev | P2 |
| v2 nav_oracle (Rust, in enclave) | 1,500 | 8 | backend + ops | P2 |
| v2 vault UI pages | 1,500 | 5 | frontend | P2 |

**P0 total**: ~14,150 LOC, ~43.5 eng-days. Two engineers × 5 weeks = 50 eng-days. Fits with buffer.

---

## 12. Deployment Plan

### 12.1 Environments

| Env | Sui | HL | Limitless | Hosting |
|---|---|---|---|---|
| Local dev | testnet | HL testnet 998 | Base sepolia | Docker compose |
| CI | testnet | HL testnet 998 | Base sepolia | GitHub Actions ephemeral |
| Staging | testnet | HL testnet 998 | Base sepolia | Fly.io single region (sin) |
| Demo / Hackathon | testnet | HL testnet 998 | Base sepolia | Fly.io sin + Vercel sin |
| Mainnet v1 | mainnet | HL mainnet | Base mainnet | Multi-region, KMS, Sentry |

### 12.2 Staged rollout (Sui v1.72.2 best practice)

1. **devnet** smoke test of any new Move module (vault path only)
2. **testnet** for ≥2 weeks live with monitored capital
3. **mainnet shadow** — bot computes intents but doesn't execute (records would-be P&L for 1 week)
4. **mainnet capped** — execute with `max_total_notional = $10k` for 2 weeks
5. **mainnet full** — caps lifted, multisig holds emergency pause

### 12.3 Move package UpgradeCaps

All multisigs use **Sui native multisig** (`sui::multisig`) — UpgradeCap is `transfer`'d to the multisig address, no custom Move multisig logic. Rationale in [ADR-006](../decisions/ADR-006-native-multisig.md).

- `volarb::events` UpgradeCap → 2/3 team multisig address
- v2 vault modules (`vol_arb_vault`, `nautilus_verifier`, `vault_admin`, `vault_events`) UpgradeCap → 4/5 governance multisig address (same signer set as ADR-001 §4 dispute arbitration, weighted)
- All mainnet upgrades go through a **24h timelock** enforced in `vault_admin::execute_after_timelock`; emergency override = 5/5 multisig signs an explicit `BypassTimelock` payload

---

## 13. Threat Model

Per-component attack surface and mitigations. (See also `docs/security/threat-model.md` for full STRIDE.)

| Asset | Attack | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| Operator Sui signer key | Key exfil from host | Medium | Critical | KMS (v1+); enclave-bound (v2) |
| HL API secret | Same | Medium | High | Same |
| Engine RPC port | Unauthed access leaks strategy | Medium | Medium | mTLS between engine and dashboard; dashboard auth |
| Indexer DB | SQL injection via dashboard input | Low | High | Parameterized queries only; no raw input concat |
| Predict oracle | SVI feed manipulation upstream | Low | Critical | Pyth cross-check; auto-halt on divergence |
| HL testnet | Testnet rollback / wipe | Low | Medium | Tolerated; demo bot resumes; v1 mainnet not affected |
| Cross-chain leg desync | One leg fills, other doesn't | High | High | 30s unwind window in executor; conformance tests |
| v2 NAV attestation | TEE compromise / tainted input | Low | Critical | 12h challenge window; PCR pinning; multisig override (ADR-001) |
| v2 vault Move | Reentrancy via outcome token | Low | Critical | `sui-red-team` skill audit before mainnet; Sui's borrow checker prevents most cases |
| Dashboard | XSS via venue name | Low | Medium | React escapes by default; CSP header; venue names are enum, not free text |
| Telegram alert | Phishing via spoofed bot | Medium | Low | Signed alert payload; users verify bot ID once |

### 13.1 Move-specific red-team vectors (run before any mainnet)

Mandatory `sui-red-team` skill runs on every Move module before audit:
1. Access control bypass (can a non-admin call `accrue_fees`?)
2. Integer overflow in NAV math (share mint with `u64::MAX` deposit)
3. Object manipulation (can `Vault<T>` be wrapped/transferred to a malicious shared object?)
4. Economic exploit (can deposit + immediate withdraw extract value via NAV stale window?)
5. DoS via gas exhaustion (large attestation blob, large dispute payload)

---

## 14. Testing Strategy

### 14.1 Layered pyramid

| Layer | Tooling | Coverage target |
|---|---|---|
| Unit tests (Rust) | `cargo test` | ≥80% on `volarb-pricing`, `volarb-risk`, `volarb-router` |
| Adapter conformance | `conformance_test!` macro against testnet | All 6 trait methods × every adapter |
| Move tests | `sui move test` | ≥90% on all v2 modules; gas benchmarks recorded |
| Integration (cross-chain) | Custom harness, real testnet | 1 cycle/venue/scenario, runs nightly |
| E2E (dashboard → engine → chain) | Playwright | Critical user flows: connect, configure, execute, withdraw |
| Backtest replay | Replay engine | 2-week window, deterministic — same seed = same equity curve |
| Monkey / fuzz | `cargo-fuzz` on parsers, `proptest` on Risk Engine gates | No panics, no missed kills |
| Load test | `vegeta` against gRPC + dashboard | 100 concurrent users / 1k req/s |

### 14.2 Move test priorities

`sui-tester` skill is invoked for every v2 module. Required tests:
- Happy path: deposit, NAV update, withdraw
- NAV attestation: valid PCR vs invalid PCR vs replay attack
- Challenge: raise valid dispute, raise invalid dispute, expire window
- Multisig: 3/5 sign success, 2/5 sign fail
- Emergency pause: pauseable state machine all paths

### 14.3 Monkey testing (per project conventions)

After unit + integration green, dedicated **monkey-test sprint**:
- Random kill of any single venue / RPC / process for 30 min, observe recovery
- Inject malformed quotes (negative prices, NaN, huge sizes) — pricing engine must reject, not panic
- Clock skew injection ±5 min — watchdogs must catch
- Sui testnet wipe simulation — bot must recover state from indexer without double-spending

---

## 15. Observability

- **Metrics**: Prometheus, scraped by Grafana. Key dashboards: arb-cycle funnel (Idle → Settled), per-venue health, risk gate decisions, NAV history (v2)
- **Logs**: structured JSON via `tracing` crate, shipped to Loki. Redaction lint on secrets
- **Traces**: OpenTelemetry; one trace per arb cycle spans Pricing → Router → Risk → Executor → Settlement
- **Alerts**:
  - PagerDuty for: kill-switch fired, NAV attestation overdue >15min, indexer lag >60s
  - Telegram for: arb-cycle Settled (P&L > $10), Aborted, deposit >$5k (v2)

---

## 16. Open Engineering Questions

(Mirror of BUSINESS_SPEC §14, filtered to engineering-only):

1. **Rust ↔ TS PTB construction** — Rust `sui-sdk` crate vs FFI-call into `@mysten/sui`? Lean toward native Rust sdk; if SVI/Predict bindings lag, FFI as fallback. **→ candidate ADR-002**
2. **VenueAdapter trait scope** — do we need separate `ReadAdapter` / `WriteAdapter` traits for future read-only feeds? Defer. **→ candidate ADR-003**
3. **Cross-chain leg unwind window** — is 30s the right number? Backtest will inform; tune in staging. **→ candidate ADR-004**
4. **Indexer hosting** — self-host Postgres on Fly.io vs Vercel Marketplace Neon? Neon for v1 simplicity; revisit at v2 scale.
5. **NAV oracle hot-update** — can we hot-swap enclave code without on-chain redeploy? Investigate Nautilus governance proposal flow.
6. **Backtest determinism** — exact replay requires recording venue WS sequence; current design records quote snapshots but not raw frames. May need WS frame archive for adversarial replay tests.

---

## 17. Appendix

### 17.1 Glossary

- **SVI** — Stochastic Volatility Inspired parametrization (Gatheral)
- **PTB** — Programmable Transaction Block (Sui's atomic multi-call)
- **HIP-4** — Hyperliquid Improvement Proposal 4, Outcome Markets
- **NAV** — Net Asset Value, per-share (see ADR-001)
- **PCR** — Platform Configuration Register, hash of enclave code in TEE attestation
- **Kelly fraction** — fractional bet-sizing rule, default `f = 0.25`

### 17.2 Version table (matches BUSINESS_SPEC §9)

| Component | Version pinned |
|---|---|
| Sui protocol | 124 (testnet v1.72.2 / mainnet v1.71.1) |
| `@mysten/sui` | `^1.x` (Transaction class) — exact pin in `package.json` |
| `@mysten/dapp-kit-react` | exact pin in `package.json` |
| `@mysten/walrus` | exact pin in `package.json` (v2 NAV snapshots) |
| Move edition | 2024 |
| DeepBook v3 | testnet package id `0x...` (TBD on first deploy); pinned by commit hash in `Move.toml` |
| Nautilus framework | rev `${TBD}` from `MystenLabs/nautilus` (set at v2 work start) |
| Predict modules | testnet package id `0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138` (branch `predict-testnet-4-16`, Immutable); read SVI from `predict-server.testnet.mystenlabs.com`. ⚠️ testnet/experimental — re-verify before mainnet |
| Rust | stable, MSRV 1.78 |
| Cargo workspace deps | `sui-sdk`, `tonic`, `tokio`, `sqlx` — exact `=x.y.z` pins |
| Node | 20 LTS |
| Next.js | 16 (App Router, Cache Components) |
| Postgres | 16 |

**Pinning policy**: all production dependencies pinned to exact versions (`=1.2.3` in Cargo.toml, exact strings in package.json). Renovate bot opens PRs for security patches only; minor/major upgrades require explicit human review + an ADR if behavior-changing.

### 17.3 Document history

| Date | Author | Change |
|---|---|---|
| 2026-05-28 | team + Claude/Opus 4.7 | Initial draft. MVP architecture + v2 vault outlined. ADR-001 referenced as NAV-oracle source of truth. |
| 2026-05-29 | team + Claude/Opus 4.7 | Applied `sui-architect` validation review: fixed §1 SDK label, §2.3 5s/30s stages, §3.5 single-leg watchdog semantics, §5.1 PTB correctness (margin borrow flow, Clock, owned PredictManager), §6.1 events module (full imports + Clock), §6.2 `Vault<phantom BaseCoin>` + OTW pattern + Nautilus lib wrap, §12.3 Sui native multisig, §17.2 version pinning policy. New ADR-006 (native multisig) + ADR-007 (Clock vs epoch timestamp). |
| 2026-05-30 | team + Claude/Opus 4.8 | **`/dual-review` pass (codex hung → general-purpose subagent fallback + project-rules round).** Fixed: §3.3 router (`best.as_ref()` borrow + crossed strike/expiry); §3.5 `CycleState` `Instant`→`u64` ms (Instant non-serializable, breaks crash-resume); §4 trait `Result<_, VenueError>` (was anyhow-style, violated ADR-003); §5.1 `splitCoins` array destructure + marked DeepBook `margin::borrow` ABI UNVERIFIED (lessons.md) + "Coin hot-potato" wording + sizing note; §2.3/§3.5 unwind reconciled to ADR-004 (5s→Unwinding canonical); `VOLARB_SHARE`→`VOL_ARB_SHARE` ×6 (OTW name match); §11 LOC 14,950→14,150; ADR-001 §5.2 share math 488.6→488.42 + dispute anti-griefing follow-up. |
| 2026-05-29 | team + Claude/Opus 4.8 | **2nd-round validation + on-chain ABI verification.** Fetched real Predict ABI via `sui_getNormalizedMoveModulesByPackage` (pkg `0xf5ea…5138`). Rewrote §5.1 PTB: `predict::mint` takes no Coin (internal-custody model) → collateral pre-deposited via `predict_manager::deposit`; market params are a `MarketKey` struct (`market_key::up/down/new`), not loose args; `mint` returns void; **deleted non-existent `predict::deposit_outcome` step**; `predict::Pool` → `predict::Predict`. Corrected §6.1 module inventory. Filled §17.2 Predict package id. |
