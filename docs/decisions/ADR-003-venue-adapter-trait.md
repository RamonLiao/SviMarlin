# ADR-003 — VenueAdapter Trait Design

> Status: **Accepted** · 2026-05-28
> Scope: `crates/volarb-venues` — the abstraction over all external prediction-market venues
> Stakeholders: backend engineers (add new venues), Router (consumer), Executor (consumer)
> Related: ADR-002 (Rust core), system spec §4

---

## 1. Context

Vol-Arb Bot's competitive moat is **multi-venue aggregation** (BUSINESS_SPEC §1, system spec §4.1). The bot ships with 2 venues at MVP (Hyperliquid HIP-4 + Limitless) and adds 4 more in v1 (Polymarket, Thales Speed, Opinion, Binance Event Contracts). Each venue has a wildly different API:

| Venue | Network model | Auth | Quote source | Settlement |
|---|---|---|---|---|
| Hyperliquid HIP-4 | L1 native API (REST/WS) | EIP-712 sig | `l2Book` snapshot | Validator vote |
| Limitless | Base EVM JSON-RPC + REST | EIP-712 sig | Subgraph + REST | On-chain |
| Polymarket | Polygon CLOB | EIP-712 sig + proxy wallet | CLOB REST/WS | UMA dispute |
| Thales Speed | Arb/Base contracts | EIP-712 sig | REST + contract reads | On-chain Chainlink |
| Opinion | BNB contract reads | EOA sig | REST/WS | Bonding curve auto-settle |
| Binance EC | CEX REST | HMAC API key | REST/WS | CEX settle |

If we let each venue's quirks bleed into Router and Executor, we get a combinatorial explosion: `N_venues × N_strategies` paths to test. The trait abstraction must be **tight enough** that Router/Executor see venues as interchangeable, **loose enough** that real venue quirks fit.

This ADR locks the trait signature so contributors adding v1 venues don't accidentally re-shape the abstraction.

---

## 2. Decision

**Single `VenueAdapter` trait, 6 methods + 2 metadata accessors. No subtrait splits in MVP.**

```rust
#[async_trait]
pub trait VenueAdapter: Send + Sync {
    // — identity & metadata —
    fn id(&self) -> VenueId;
    fn chain(&self) -> ChainKind;
    fn fees(&self) -> FeeModel;

    // — read path —
    async fn quote(&self, market: MarketRef) -> Result<Quote, VenueError>;
    async fn quote_stream(&self) -> BoxStream<'static, QuoteEvent>;
    async fn position(&self, market: MarketRef) -> Result<Option<ExtPosition>, VenueError>;
    async fn health(&self) -> HealthStatus;

    // — write path —
    async fn place(&self, order: PlaceOrder) -> Result<OrderReceipt, VenueError>;
    async fn cancel(&self, order_id: OrderId) -> Result<(), VenueError>;
    async fn settle(&self, market: MarketRef) -> Result<SettleReceipt, VenueError>;
}
```

Common error type: `VenueError` with variants `RateLimited`, `Unauthorized`, `MarketClosed`, `InsufficientLiquidity`, `Network(Box<dyn Error>)`, `VenueSpecific(String)`. **No `Result<_, anyhow::Error>`** — Router needs to distinguish retryable from terminal errors.

---

## 3. Rationale

### 3.1 Why 6 methods (not 12)?

The trait expresses **what Router and Executor need to do**, not the union of every venue's full API. Methods chosen by working backwards from the cycle state machine (system spec §2.3):

- `Idle → Preparing`: Router calls `quote_stream` continuously, `fees` + `health` to gate
- `Preparing → Sending`: Executor calls `place`
- `Sending → Live`: success path: `place` returns receipt; failure path: `cancel` + `position` to verify
- `Live → Settling`: `position` to check status, `settle` at expiry
- `Settling → Settled`: `position` again to verify final balance

Every method maps to a state transition. Removing any one method breaks the SM. Adding more methods means we're leaking venue-specific concepts.

### 3.2 Why no read/write trait split?

Considered: `trait ReadAdapter` + `trait WriteAdapter: ReadAdapter`. Rejected because:
- Every MVP and v1 venue implements both (no read-only venues in roadmap)
- Splitting forces 2 generic bounds everywhere in Router/Executor (`A: ReadAdapter + WriteAdapter`) without benefit
- Future read-only venues (e.g., Manifold for mana-based sentiment signal) can be added as a separate `SignalSource` trait — they're a different concept, not a subtrait

### 3.3 Why `quote_stream` as a separate method instead of internal pub-sub?

The trait returns a `BoxStream<'static, QuoteEvent>`, leaving subscription lifecycle in the adapter. Reasons:
- Adapter knows its own WS reconnect strategy; Router shouldn't (separation of concerns)
- `BoxStream` is `Send`, so Router can push events into `tokio::sync::broadcast` for multi-consumer (e.g., Risk Engine also wants quotes for staleness checks)
- Backpressure is the adapter's job — if Router is slow, adapter drops or merges quotes per its own policy

### 3.4 Why `fees() -> FeeModel` and not `fees(order: PlaceOrder) -> f64`?

`FeeModel` is a value type with `maker_bps`, `taker_bps`, `min_fee_usd`, `volume_tier_function`. Router computes expected fee per quote using the same model; no per-call adapter dispatch. **One method call, infinite quote evaluations.** Saves ~50µs per Router tick at 1k quotes/s.

### 3.5 Why `async fn health()` instead of pub-sub health events?

Router calls `health()` lazily (only when about to route to this venue). Pub-sub for health adds infrastructure cost for a 10s-resolution signal. If a venue ships an asynchronous degradation event, the adapter folds it into `health()`'s next return — caller is none the wiser.

### 3.6 Why no `Adapter::new(config) -> Self`?

Constructors are not part of the trait. Each adapter has its own builder with venue-specific config (HL needs API key + EIP-712 signer; Limitless needs Base RPC URL + private key; Binance EC needs HMAC creds + symbol mapping). Forcing a uniform constructor would either (a) take a giant union config type or (b) hide config validation. Both bad. Construction is venue-specific; the trait is for runtime use.

---

## 4. Alternatives Considered

### 4.1 Plugin-style dynamic dispatch via `dyn VenueAdapter`

Considered making venues hot-loadable via shared library. Rejected: hackathon timeline doesn't justify the complexity, and `dyn` dispatch costs ~5-10ns per call which adds up at WS rates. Use `enum VenueId` for routing in Router, statically-dispatched generic adapters in Executor.

### 4.2 Macro-generated adapters from OpenAPI specs

Tempting for v1 venues. Rejected because:
- Polymarket and Binance EC don't ship OpenAPI specs
- Hyperliquid's API has non-REST semantics (asset encoding `#N`, EIP-712 quirks) that auto-gen can't capture
- Maintenance of macro itself becomes a side-quest

### 4.3 Convert venues into a unified internal "exchange" model first

Some HFT shops normalize all venues to FIX. Rejected: FIX is overkill for binary prediction markets, and the normalization layer becomes another integration target. Direct trait is simpler.

### 4.4 Trait with associated types for venue-specific order types

```rust
trait VenueAdapter {
    type Order;
    type Receipt;
    async fn place(&self, order: Self::Order) -> Result<Self::Receipt>;
}
```

Rejected: Router needs to construct orders generically. Using a common `PlaceOrder` struct with optional venue-specific extension field (`extra: serde_json::Value`) is more practical. We pay 1 serde-roundtrip; we gain a uniform Router.

---

## 5. Consequences

### Positive
- Adding a new venue = 1 struct + 9 trait impls + conformance test macro invocation. Estimated 4-5 eng-days for an experienced developer per venue.
- Router and Executor have zero venue-specific code paths.
- Type safety: rate-limit retries, market-closed handling, etc. are exhaustive matches on `VenueError`.

### Negative
- Venues with truly exotic features (Opinion's bonding curve "buy quote depends on size") need an escape hatch via `extra: serde_json::Value` on `PlaceOrder`. Not pretty but bounded.
- Conformance test macro runs against real testnet, so adding a venue requires a working testnet account for that venue. Acceptable.

### Neutral
- `async-trait` macro adds 1 allocation per method call. Negligible at our call rates.

---

## 6. Conformance Test Macro

Every adapter must pass:

```rust
conformance_test!(AdapterName, {
    quote_returns_within_500ms,                 // p99
    quote_stream_reconnects_after_disconnect,
    place_then_cancel_idempotent,               // cancel twice → second is no-op, not error
    position_query_after_fill_matches_receipt,
    settle_after_expiry_within_60s,
    health_responds_under_concurrent_quotes,    // 100 concurrent quote() calls don't starve health()
    rate_limit_surfaces_as_VenueError_RateLimited,  // not as Network or generic
});
```

Failing any one blocks merge. Run nightly against each testnet.

---

## 7. Open Follow-ups

1. **PlaceOrder schema review** at v1 venue #3 onboarding. If 3+ venues need `extra: serde_json::Value`, redesign with explicit variants.
2. **Read-only `SignalSource` trait** for Manifold and Polymarket-public-data — separate concept; design when first signal source lands.
3. **Backtest mode** for adapters: each adapter exposes `with_replay(history: VenueHistory) -> Self` that swaps live calls for recorded. Implementing this for v1.
4. **Conformance test cost cap**: testnet calls have rate limits; budget conformance suite to run in <2 min per adapter to keep CI green.

---

## 8. Version History

| Date | Author | Change |
|---|---|---|
| 2026-05-28 | team + Claude/Opus 4.7 | Initial. Trait locked at 6 methods + 2 metadata. |
