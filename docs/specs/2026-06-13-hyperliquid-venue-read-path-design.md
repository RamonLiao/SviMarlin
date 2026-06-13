# Design — Hyperliquid HIP-4 Venue Adapter (read path) — TODO #6 Part 1

> Date: 2026-06-13
> Status: design (pre-implementation)
> Scope: VenueAdapter trait + satellite types + Hyperliquid adapter **read path only**.
> Out of scope (next round): `place` / `cancel` / `settle` (EIP-712/msgpack signing, needs funded testnet account).

## Context

TODO #6 = first real external venue. ADR-003 already locks the `VenueAdapter` trait
(6 methods + 3 metadata accessors). This round lands the trait + satellite types in
`volarb-venues`, and implements the **Hyperliquid** adapter's read path against live testnet.

### Verified facts (live testnet, 2026-06-13 — see lessons.md)

- Taiwan IP → `https://api.hyperliquid-testnet.xyz/info` HTTP 200 / ~0.27s. HL blocks US IPs; TW OK.
- **HIP-4 Outcome Markets = builder-deployed perp DEXs** (`POST /info {"type":"perpDexs"}`, 230 on testnet).
- Prediction-market builder = **HyperOdd** (dex name `ho` / `hodd`):
  - `ho:BTCHOURLY`, `ho:ETHHOURLY`, `ho:BTCDAILY`, `ho:ETHDAILY` (szDecimals 4, maxLev 50).
- **Prices are [0,1] probabilities**: `ho:BTCHOURLY` `oraclePx≈0.24`, `markPx≈0.2`. Same unit as
  Predict on-chain `oracle::compute_price → N(d2)` ([0,1e9]) → arb is **price-space**, not vol-space.
- ⚠️ **testnet hourly L2 book is empty** (bids/asks `[]`); only DAILY has small OI. Liquidity is a
  real blocker for "real two-sided fill" demo — surfaced loud, handled as `InsufficientLiquidity`.

### Read-path API surface (all `POST /info`, no auth, no signing)

| Need | Request body | Returns |
|------|-------------|---------|
| dex list | `{"type":"perpDexs"}` | builder dex array |
| market meta | `{"type":"meta","dex":"ho"}` | `universe[]` (name, szDecimals, maxLeverage) |
| meta + ctx | `{"type":"metaAndAssetCtxs","dex":"ho"}` | `[meta, ctxs[]]` (markPx, oraclePx, midPx, funding, openInterest) |
| order book | `{"type":"l2Book","coin":"ho:BTCHOURLY"}` | `{coin, time, levels:[bids[],asks[]]}` (px, sz strings) |
| position | `{"type":"clearinghouseState","dex":"ho","user":"0x.."}` | per-user state (read-only, no sig) |

WS: `wss://api.hyperliquid-testnet.xyz/ws`, subscribe `{"method":"subscribe","subscription":{"type":"l2Book","coin":"ho:BTCHOURLY"}}`.

## Module structure (`crates/volarb-venues/`)

```
src/lib.rs          VenueAdapter trait + satellite types (ADR-003-locked) + re-exports
src/error.rs        VenueError (RateLimited/Unauthorized/MarketClosed/InsufficientLiquidity/Network/VenueSpecific)
src/hyperliquid/
  mod.rs            HyperliquidAdapter + builder (base_url, dex name, optional user addr)
  info.rs           /info REST client (meta / metaAndAssetCtxs / l2Book / clearinghouseState) → domain
  ws.rs             wss l2Book subscription → quote_stream BoxStream
  market.rs         MarketRef ↔ HL "dex:COIN" + asset-index mapping; BTCHOURLY → (strike, expiry) derivation
```

`place`/`cancel`/`settle` live in `mod.rs` returning a loud unimplemented error this round.
No `signing.rs` yet (no signing deps added).

## VenueAdapter trait + satellite types (ADR-003)

```rust
#[async_trait]
pub trait VenueAdapter: Send + Sync {
    fn id(&self) -> VenueId;
    fn chain(&self) -> ChainKind;
    fn fees(&self) -> FeeModel;

    async fn quote(&self, market: MarketRef) -> Result<Quote, VenueError>;
    async fn quote_stream(&self) -> BoxStream<'static, QuoteEvent>;
    async fn position(&self, market: MarketRef) -> Result<Option<ExtPosition>, VenueError>;
    async fn health(&self) -> HealthStatus;

    async fn place(&self, order: PlaceOrder) -> Result<OrderReceipt, VenueError>;
    async fn cancel(&self, order_id: OrderId) -> Result<(), VenueError>;
    async fn settle(&self, market: MarketRef) -> Result<SettleReceipt, VenueError>;
}
```

Satellite types — **only the fields proven by live API this round**; write-side types
(`PlaceOrder`, `OrderReceipt`, `SettleReceipt`, `OrderId`) defined minimally enough to compile
the trait but their HL wire mapping is deferred to the signing round (don't guess fields — pull
the `/exchange` action schema when we implement `place`, per lessons.md "real ABI first").

- `VenueId` — enum { Hyperliquid, Limitless, ... }.
- `ChainKind` — enum { Evm { chain_id: u64 }, Sui, ... }. HL = `Evm { chain_id: 998 }` (HL testnet).
- `FeeModel` — taker/maker bps + flat; HL builder-dex fees from `perpDexs.deployerFeeScale`.
- `MarketRef` — `{ venue_market: String /* "ho:BTCHOURLY" */, strike: Strike, expiry: Expiry }`.
  HL outcome perps have no native strike/expiry field → derived from market name + ctx in `market.rs`.
- `QuoteEvent` — `{ market: MarketRef, quote: Quote }` (reuses `core::Quote`, [0,1] prices).
- `ExtPosition` — `{ market, side, size: f64, entry_px: f64, unrealized_pnl: f64 }`.
- `HealthStatus` — enum { Healthy, Degraded { reason }, Down }.

## Read-method semantics

- `quote` → `l2Book` → best bid/ask as `core::Quote`. Empty book → `Err(InsufficientLiquidity)` (loud).
- `quote_stream` → WS `l2Book` sub → `QuoteEvent`. Reconnect/backoff in `ws.rs`; on terminal close the
  stream ends (caller re-subscribes). Parse failures dropped with a warn, not silently.
- `position` → requires `user` addr (set at builder); `None` if no position. Read-only, no signing.
- `health` → `meta` reachable + latency under threshold → `Healthy`; slow → `Degraded`; error → `Down`.
- `place` / `cancel` / `settle` → `Err(VenueError::VenueSpecific("unimplemented: HL signing round (TODO #6 part 2)"))`.

## Wire-format → domain mapping (the real design work)

- HL prices/sizes are **JSON strings** → parse to `f64` via `core::numeric` round+clamp helpers; reject
  non-finite (NaN/Inf) loud (echoes Plan A monkey bug: `is_finite()` guard, not `<= 0.0`).
- `dex:COIN` naming: `ho:BTCHOURLY`. Asset index for write-side (`#102170`-style from `allMids`) is
  **only needed for `place`** → captured in the signing round, not now.
- `BTCHOURLY` / `BTCDAILY` → `(strike, expiry)`: HourlyDaily cadence maps to expiry bucket; strike for
  these "will BTC be up?" outcome perps is the reference spot at window open (from ctx). Derivation rule
  documented in `market.rs`; if a market name doesn't parse → `Err(MarketClosed)` rather than guess.

## Dependencies (workspace.dependencies, new)

`reqwest` (json, rustls-tls), `tokio-tungstenite` (rustls), `futures` (BoxStream/Stream), `async-trait`.
**No** `alloy`/`k256`/`sha3` this round (signing deferred). `volarb-core` already in deps.

## Testing

- **Unit (offline CI)**: wire-format parse tests using **real JSON captured live 2026-06-13** as fixtures
  (perpDexs / meta / metaAndAssetCtxs / l2Book). Verifies [0,1] price parse, string→f64, empty-book →
  `InsufficientLiquidity`, market-name → (strike,expiry) derivation.
- **Integration (`#[ignore]`, needs network)**: hit live testnet `ho` dex, assert quote shape / health OK.
- **Monkey**: empty book, malformed/NaN price string, unknown dex name, oversized response, WS mid-stream
  disconnect — none panic; all map to typed `VenueError`.

## Out of scope / handoff to signing round (TODO #6 part 2)

Needs from user: HL testnet private key + USDC (faucet/bridge). Then: `/exchange` action schema pull,
msgpack action-hash + EIP-712 signing, `place`/`cancel` verified via "valid-sig-but-no-funds → specific
error" probe (signature validation precedes margin check), `settle`, asset-index mapping, empty-book
demo strategy (self-maker vs pick a market with OI).
