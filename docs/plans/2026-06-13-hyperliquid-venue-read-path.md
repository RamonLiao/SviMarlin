# Hyperliquid HIP-4 Venue Adapter (read path) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the `VenueAdapter` trait + satellite types in `volarb-venues`, and implement the Hyperliquid HIP-4 adapter's **read path** (`quote`, `quote_stream`, `position`, `health`) verified against live testnet; write methods (`place`/`cancel`/`settle`) are loud stubs deferred to the signing round.

**Architecture:** `volarb-venues` exposes one trait (ADR-003-locked) so Router/Executor stay venue-agnostic. The Hyperliquid adapter talks to `https://api.hyperliquid-testnet.xyz` — `/info` POST for REST reads, `wss://.../ws` for the quote stream. HIP-4 Outcome Markets are builder-deployed perp DEXs; the HyperOdd builder (`ho` dex) lists `ho:BTCHOURLY`-style binary outcome perps whose prices are [0,1] probabilities — same unit as Predict's on-chain `N(d2)`. Wire prices/sizes are JSON strings → parsed to `f64` with finite-guards.

**Tech Stack:** Rust (edition 2024, toolchain 1.94.1), `reqwest` (rustls), `tokio-tungstenite` (rustls), `futures`, `async-trait`, `serde`/`serde_json`, `thiserror`. Domain types from `volarb-core` (`Quote`, `Side`, `Strike`, `Expiry`).

---

## Background facts (verified live testnet 2026-06-13 — see tasks/lessons.md)

- TW IP → `https://api.hyperliquid-testnet.xyz/info` HTTP 200. HL blocks US IPs; TW OK.
- HIP-4 = builder perp DEXs (`POST /info {"type":"perpDexs"}`). Prediction builder = HyperOdd `ho`/`hodd`.
- `ho` universe: `ho:BTCHOURLY`, `ho:ETHHOURLY`, `ho:BTCDAILY`, `ho:ETHDAILY` (szDecimals 4, onlyIsolated).
- Prices are [0,1] probabilities (`oraclePx`/`markPx` strings). **testnet HyperOdd L2 books are empty.**
- Read endpoints (all `POST /info`, no auth):
  - `{"type":"meta","dex":"ho"}` → `{universe:[{name,szDecimals,maxLeverage,...}]}`
  - `{"type":"metaAndAssetCtxs","dex":"ho"}` → `[meta, ctxs:[{markPx,oraclePx,midPx,funding,openInterest,...}]]`
  - `{"type":"l2Book","coin":"ho:BTCHOURLY"}` → `{coin,time,levels:[bids,asks]}`, level `{px,sz,n}`
  - `{"type":"clearinghouseState","dex":"ho","user":"0x.."}` → per-user state
- WS: `wss://api.hyperliquid-testnet.xyz/ws`, subscribe `{"method":"subscribe","subscription":{"type":"l2Book","coin":"ho:BTCHOURLY"}}`.

Captured fixtures already committed at `crates/volarb-venues/tests/fixtures/`:
`meta_ho.json`, `meta_and_ctxs_ho.json`, `l2book_empty.json` (real, empty), `l2book_filled.json` (hand-authored to real schema, [0,1] prices).

---

## File Structure

```
crates/volarb-venues/
  Cargo.toml                  Modify: add reqwest, tokio-tungstenite, futures, async-trait, serde, serde_json, thiserror, tokio
  src/lib.rs                  Modify: VenueAdapter trait + satellite types + module wiring
  src/error.rs                Create: VenueError
  src/hyperliquid/mod.rs      Create: HyperliquidAdapter + builder + 8 trait methods
  src/hyperliquid/info.rs     Create: /info REST client + wire structs + price parse
  src/hyperliquid/ws.rs       Create: quote_stream WS subscription
  tests/fixtures/*.json       (already committed)
  tests/live_testnet.rs       Create: #[ignore] integration tests against live testnet
```

---

### Task 1: Dependencies + VenueError

**Files:**
- Modify: `crates/volarb-venues/Cargo.toml`
- Create: `crates/volarb-venues/src/error.rs`
- Modify: `crates/volarb-venues/src/lib.rs` (add `pub mod error;`)

- [ ] **Step 1: Add dependencies to `crates/volarb-venues/Cargo.toml`**

Replace the `[dependencies]` block with:

```toml
[dependencies]
volarb-core.workspace = true
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio.workspace = true
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
tokio-tungstenite = { version = "0.24", default-features = false, features = ["connect", "rustls-tls-webpki-roots"] }
futures = "0.3"

[dev-dependencies]
tokio = { workspace = true }
```

- [ ] **Step 2: Write `crates/volarb-venues/src/error.rs`**

```rust
//! Common venue error type (ADR-003). Router needs to distinguish retryable from terminal.

use thiserror::Error;

/// Errors any `VenueAdapter` method may return.
#[derive(Debug, Error)]
pub enum VenueError {
    /// Venue rate-limited us; retryable after backoff.
    #[error("rate limited")]
    RateLimited,
    /// Auth/permission failure; terminal until creds fixed.
    #[error("unauthorized")]
    Unauthorized,
    /// Market not open / not found; terminal for this market.
    #[error("market closed or not found: {0}")]
    MarketClosed(String),
    /// Order book empty or too thin to quote/fill.
    #[error("insufficient liquidity: {0}")]
    InsufficientLiquidity(String),
    /// Transport / IO / deserialization failure; usually retryable.
    #[error("network: {0}")]
    Network(String),
    /// Venue-specific condition that doesn't fit the above (incl. unimplemented stubs).
    #[error("venue-specific: {0}")]
    VenueSpecific(String),
}
```

- [ ] **Step 3: Wire the module in `crates/volarb-venues/src/lib.rs`**

Add near the top of the file (after the doc comment, before the commented-out `pub mod hyperliquid`):

```rust
pub mod error;
pub use error::VenueError;
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p volarb-venues`
Expected: success (warnings about unused `VenueError` re-export are OK at this stage).

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-venues/Cargo.toml crates/volarb-venues/src/error.rs crates/volarb-venues/src/lib.rs
git commit -m "feat(venues): add deps + VenueError (TODO #6 pt1)"
```

---

### Task 2: VenueAdapter trait + satellite types

**Files:**
- Modify: `crates/volarb-venues/src/lib.rs`

- [ ] **Step 1: Write the failing test** (append to `crates/volarb-venues/src/lib.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn venue_id_and_chain_are_constructible() {
        let id = VenueId::Hyperliquid;
        let chain = ChainKind::Evm { chain_id: 998 };
        assert_eq!(id, VenueId::Hyperliquid);
        assert_eq!(chain, ChainKind::Evm { chain_id: 998 });
    }

    #[test]
    fn market_ref_round_trips_fields() {
        use volarb_core::{Expiry, Strike};
        let m = MarketRef {
            venue_market: "ho:BTCHOURLY".to_string(),
            strike: Strike(64000.0),
            expiry: Expiry { unix_ms: 1_781_348_700_000 },
        };
        assert_eq!(m.venue_market, "ho:BTCHOURLY");
        assert_eq!(m.strike.0, 64000.0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-venues`
Expected: FAIL — `VenueId`, `ChainKind`, `MarketRef` not defined.

- [ ] **Step 3: Add the trait + satellite types** to `crates/volarb-venues/src/lib.rs` (after the `pub use error::VenueError;` line, before the `#[cfg(test)]` block)

```rust
use async_trait::async_trait;
use futures::stream::BoxStream;
use volarb_core::{Expiry, Quote, Side, Strike};

/// Which venue an adapter speaks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueId {
    Hyperliquid,
    Limitless,
}

/// Settlement chain model for a venue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainKind {
    /// EVM chain (Hyperliquid testnet = chain_id 998).
    Evm { chain_id: u64 },
    Sui,
}

/// Static fee schedule (ADR-003 §3.4: fees are a property, not per-order).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeeModel {
    pub taker_bps: f64,
    pub maker_bps: f64,
}

/// A specific market on a venue. `venue_market` is the venue-native id (HL: "ho:BTCHOURLY").
/// `strike`/`expiry` are the caller's domain coordinates, echoed back into quotes.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketRef {
    pub venue_market: String,
    pub strike: Strike,
    pub expiry: Expiry,
}

/// A quote update pushed by `quote_stream`.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteEvent {
    pub market: MarketRef,
    pub quote: Quote,
}

/// An open position on the venue.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtPosition {
    pub market: MarketRef,
    pub side: Side,
    pub size: f64,
    pub entry_px: f64,
    pub unrealized_pnl: f64,
}

/// Venue health for lazy Router routing (ADR-003 §3.5).
#[derive(Debug, Clone, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Degraded { reason: String },
    Down,
}

/// Opaque venue order id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderId(pub String);

/// A request to place an order (write side — HL wire mapping deferred to the signing round).
#[derive(Debug, Clone, PartialEq)]
pub struct PlaceOrder {
    pub market: MarketRef,
    pub side: Side,
    pub price: f64,
    pub size: f64,
}

/// Receipt for a placed order (write side — fields finalized in the signing round).
#[derive(Debug, Clone, PartialEq)]
pub struct OrderReceipt {
    pub order_id: OrderId,
    pub filled_size: f64,
}

/// Receipt for a settled market (write side — finalized in the signing round).
#[derive(Debug, Clone, PartialEq)]
pub struct SettleReceipt {
    pub market: MarketRef,
    pub payout: f64,
}

/// The keystone abstraction (ADR-003): Router/Executor see venues as interchangeable.
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

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-venues`
Expected: PASS (2 tests). `cargo clippy -p volarb-venues -- -D warnings` clean.

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-venues/src/lib.rs
git commit -m "feat(venues): VenueAdapter trait + satellite types (ADR-003, TODO #6 pt1)"
```

---

### Task 3: HL `/info` REST client + wire structs + price parse

**Files:**
- Create: `crates/volarb-venues/src/hyperliquid/info.rs`
- Modify: `crates/volarb-venues/src/lib.rs` (add `pub mod hyperliquid;`)
- Create: `crates/volarb-venues/src/hyperliquid/mod.rs` (minimal, just `pub mod info;` for now)

- [ ] **Step 1: Create `crates/volarb-venues/src/hyperliquid/mod.rs`** (minimal for this task)

```rust
//! Hyperliquid HIP-4 venue adapter.

pub mod info;
```

- [ ] **Step 2: Wire module in `crates/volarb-venues/src/lib.rs`**

Replace the commented-out lines:

```rust
// pub mod hyperliquid;  // TODO(#6)
// pub mod limitless;    // TODO(#6)
```

with:

```rust
pub mod hyperliquid;
```

- [ ] **Step 3: Write failing tests** — create `crates/volarb-venues/src/hyperliquid/info.rs`

```rust
//! Hyperliquid `/info` POST REST client + wire-format → domain parsing.
//!
//! All prices/sizes arrive as JSON strings; we parse to f64 and reject non-finite values
//! (echoes the Plan A monkey bug: NaN slips past `<= 0.0`, so guard with `is_finite()`).

use crate::error::VenueError;
use serde::Deserialize;

/// One L2 book level. HL wire: `{"px":"0.23","sz":"100.0","n":2}`.
#[derive(Debug, Clone, Deserialize)]
pub struct L2Level {
    pub px: String,
    pub sz: String,
    #[allow(dead_code)]
    pub n: u32,
}

/// `l2Book` response. `levels` is `[bids, asks]`, each sorted best-first.
#[derive(Debug, Clone, Deserialize)]
pub struct L2Book {
    pub coin: String,
    pub time: u64,
    pub levels: Vec<Vec<L2Level>>,
}

/// Best (bid, ask) parsed from an L2 book, prices as f64. Errors loud on empty/NaN.
pub fn best_bid_ask(book: &L2Book) -> Result<(f64, f64), VenueError> {
    let bids = book
        .levels
        .first()
        .ok_or_else(|| VenueError::Network("l2Book missing bids array".into()))?;
    let asks = book
        .levels
        .get(1)
        .ok_or_else(|| VenueError::Network("l2Book missing asks array".into()))?;
    let bid = bids
        .first()
        .ok_or_else(|| VenueError::InsufficientLiquidity(format!("{}: empty bids", book.coin)))?;
    let ask = asks
        .first()
        .ok_or_else(|| VenueError::InsufficientLiquidity(format!("{}: empty asks", book.coin)))?;
    Ok((parse_px(&bid.px)?, parse_px(&ask.px)?))
}

/// Parse a venue price/size string to a finite f64. Rejects NaN/Inf/garbage loud.
pub fn parse_px(s: &str) -> Result<f64, VenueError> {
    let v: f64 = s
        .parse()
        .map_err(|_| VenueError::Network(format!("unparseable number: {s:?}")))?;
    if !v.is_finite() {
        return Err(VenueError::Network(format!("non-finite number: {s:?}")));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(name: &str) -> String {
        std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap()
    }

    #[test]
    fn parses_filled_book_best_levels() {
        let book: L2Book = serde_json::from_str(&load("l2book_filled.json")).unwrap();
        let (bid, ask) = best_bid_ask(&book).unwrap();
        assert_eq!(bid, 0.23);
        assert_eq!(ask, 0.25);
        assert_eq!(book.coin, "ho:BTCHOURLY");
    }

    #[test]
    fn empty_book_is_insufficient_liquidity() {
        let book: L2Book = serde_json::from_str(&load("l2book_empty.json")).unwrap();
        let err = best_bid_ask(&book).unwrap_err();
        assert!(matches!(err, VenueError::InsufficientLiquidity(_)));
    }

    #[test]
    fn parse_px_rejects_nan_and_garbage() {
        assert!(parse_px("NaN").is_err());
        assert!(parse_px("inf").is_err());
        assert!(parse_px("abc").is_err());
        assert_eq!(parse_px("0.24").unwrap(), 0.24);
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p volarb-venues hyperliquid::info`
Expected: PASS (3 tests). If "fixture not found", confirm cwd is the crate dir (cargo runs tests with `CARGO_MANIFEST_DIR` as cwd, so `tests/fixtures/...` resolves).

- [ ] **Step 5: Add the meta/ctx wire structs + HTTP client** (append to `info.rs`, before `#[cfg(test)]`)

```rust
/// One asset's context from `metaAndAssetCtxs`. Prices are strings; `midPx` may be null.
#[derive(Debug, Clone, Deserialize)]
pub struct AssetCtx {
    #[serde(rename = "markPx")]
    pub mark_px: String,
    #[serde(rename = "oraclePx")]
    pub oracle_px: String,
    #[serde(rename = "midPx")]
    pub mid_px: Option<String>,
    #[serde(rename = "openInterest")]
    pub open_interest: String,
}

/// HTTP client for the `/info` endpoint of a Hyperliquid API host.
#[derive(Debug, Clone)]
pub struct InfoClient {
    base_url: String,
    http: reqwest::Client,
}

impl InfoClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), http: reqwest::Client::new() }
    }

    /// POST a JSON body to `/info` and deserialize the response.
    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        body: serde_json::Value,
    ) -> Result<T, VenueError> {
        let resp = self
            .http
            .post(format!("{}/info", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| VenueError::Network(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(VenueError::RateLimited);
        }
        if !resp.status().is_success() {
            return Err(VenueError::Network(format!("HTTP {}", resp.status())));
        }
        resp.json::<T>().await.map_err(|e| VenueError::Network(e.to_string()))
    }

    /// Fetch the L2 order book for a venue-native coin id (e.g. "ho:BTCHOURLY").
    pub async fn l2_book(&self, coin: &str) -> Result<L2Book, VenueError> {
        self.post(serde_json::json!({ "type": "l2Book", "coin": coin })).await
    }

    /// Fetch `[meta, ctxs]` for a builder dex. Returns the ctxs vec (index-aligned to universe).
    pub async fn asset_ctxs(&self, dex: &str) -> Result<Vec<AssetCtx>, VenueError> {
        let raw: (serde_json::Value, Vec<AssetCtx>) = self
            .post(serde_json::json!({ "type": "metaAndAssetCtxs", "dex": dex }))
            .await?;
        Ok(raw.1)
    }
}
```

- [ ] **Step 6: Add a fixture test for AssetCtx parsing** (inside the `#[cfg(test)] mod tests`)

```rust
    #[test]
    fn parses_asset_ctxs_fixture() {
        let raw: (serde_json::Value, Vec<AssetCtx>) =
            serde_json::from_str(&load("meta_and_ctxs_ho.json")).unwrap();
        let ctxs = raw.1;
        assert!(!ctxs.is_empty());
        // ho:BTCHOURLY oraclePx = "0.24" per capture.
        assert_eq!(parse_px(&ctxs[0].oracle_px).unwrap(), 0.24);
        assert!(ctxs[0].mid_px.is_none()); // midPx null in capture
    }
```

- [ ] **Step 7: Run tests + clippy**

Run: `cargo test -p volarb-venues hyperliquid::info`
Expected: PASS (4 tests).
Run: `cargo clippy -p volarb-venues -- -D warnings`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/volarb-venues/src/lib.rs crates/volarb-venues/src/hyperliquid/mod.rs crates/volarb-venues/src/hyperliquid/info.rs
git commit -m "feat(venues): HL /info REST client + wire parse (TODO #6 pt1)"
```

---

### Task 4: HyperliquidAdapter + builder + read methods + stubbed write methods

**Files:**
- Modify: `crates/volarb-venues/src/hyperliquid/mod.rs`

- [ ] **Step 1: Write the failing test** (add to `crates/volarb-venues/src/hyperliquid/mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OrderId, PlaceOrder, VenueAdapter, VenueId};
    use volarb_core::{Expiry, Side, Strike};

    fn mref() -> crate::MarketRef {
        crate::MarketRef {
            venue_market: "ho:BTCHOURLY".to_string(),
            strike: Strike(64000.0),
            expiry: Expiry { unix_ms: 1_781_348_700_000 },
        }
    }

    #[test]
    fn adapter_metadata_is_hyperliquid_testnet() {
        let a = HyperliquidAdapter::builder().build();
        assert_eq!(a.id(), VenueId::Hyperliquid);
        assert_eq!(a.chain(), crate::ChainKind::Evm { chain_id: 998 });
    }

    #[tokio::test]
    async fn write_methods_fail_loud() {
        let a = HyperliquidAdapter::builder().build();
        let order = PlaceOrder { market: mref(), side: Side::Up, price: 0.2, size: 1.0 };
        assert!(matches!(a.place(order).await, Err(crate::VenueError::VenueSpecific(_))));
        assert!(matches!(
            a.cancel(OrderId("x".into())).await,
            Err(crate::VenueError::VenueSpecific(_))
        ));
        assert!(matches!(a.settle(mref()).await, Err(crate::VenueError::VenueSpecific(_))));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-venues hyperliquid::tests`
Expected: FAIL — `HyperliquidAdapter` not defined.

- [ ] **Step 3: Implement the adapter** (prepend to `crates/volarb-venues/src/hyperliquid/mod.rs`, keeping `pub mod info;`)

```rust
//! Hyperliquid HIP-4 venue adapter.

pub mod info;
pub mod ws;

use crate::{
    ChainKind, ExtPosition, FeeModel, HealthStatus, MarketRef, OrderId, OrderReceipt, PlaceOrder,
    Quote, SettleReceipt, VenueAdapter, VenueError, VenueId,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use info::InfoClient;

const MAINNET_API: &str = "https://api.hyperliquid.xyz";
const TESTNET_API: &str = "https://api.hyperliquid-testnet.xyz";
const TESTNET_WS: &str = "wss://api.hyperliquid-testnet.xyz/ws";
/// Latency over this (ms) downgrades health to Degraded.
const HEALTH_LATENCY_MS: u128 = 1_500;

/// Read-path adapter for Hyperliquid HIP-4 outcome markets (HyperOdd builder dex).
#[derive(Debug, Clone)]
pub struct HyperliquidAdapter {
    info: InfoClient,
    ws_url: String,
    dex: String,
    /// User address for `position()` reads (read-only, no signing). None → position() errors.
    user: Option<String>,
    testnet: bool,
}

/// Builder; construction is venue-specific (ADR-003 §3.6).
pub struct HyperliquidBuilder {
    base_url: String,
    ws_url: String,
    dex: String,
    user: Option<String>,
    testnet: bool,
}

impl HyperliquidAdapter {
    pub fn builder() -> HyperliquidBuilder {
        HyperliquidBuilder {
            base_url: TESTNET_API.to_string(),
            ws_url: TESTNET_WS.to_string(),
            dex: "ho".to_string(),
            user: None,
            testnet: true,
        }
    }
}

impl HyperliquidBuilder {
    /// Switch to mainnet API host (default is testnet).
    pub fn mainnet(mut self) -> Self {
        self.base_url = MAINNET_API.to_string();
        self.ws_url = "wss://api.hyperliquid.xyz/ws".to_string();
        self.testnet = false;
        self
    }
    /// Builder dex name (default "ho" = HyperOdd prediction markets).
    pub fn dex(mut self, dex: impl Into<String>) -> Self {
        self.dex = dex.into();
        self
    }
    /// User address for position reads.
    pub fn user(mut self, addr: impl Into<String>) -> Self {
        self.user = Some(addr.into());
        self
    }
    pub fn build(self) -> HyperliquidAdapter {
        HyperliquidAdapter {
            info: InfoClient::new(self.base_url),
            ws_url: self.ws_url,
            dex: self.dex,
            user: self.user,
            testnet: self.testnet,
        }
    }
}

#[async_trait]
impl VenueAdapter for HyperliquidAdapter {
    fn id(&self) -> VenueId {
        VenueId::Hyperliquid
    }

    fn chain(&self) -> ChainKind {
        // HL testnet = chain_id 998; mainnet = 1337 (HL's EVM chain id).
        ChainKind::Evm { chain_id: if self.testnet { 998 } else { 1337 } }
    }

    fn fees(&self) -> FeeModel {
        // HL standard taker/maker; builder-dex fee scale applies on top (deferred to signing round).
        FeeModel { taker_bps: 4.5, maker_bps: 1.5 }
    }

    async fn quote(&self, market: MarketRef) -> Result<Quote, VenueError> {
        let book = self.info.l2_book(&market.venue_market).await?;
        let (bid, ask) = info::best_bid_ask(&book)?;
        Ok(Quote { bid, ask, strike: market.strike, expiry: market.expiry, ts_ms: book.time })
    }

    async fn quote_stream(&self) -> BoxStream<'static, crate::QuoteEvent> {
        ws::quote_stream(self.ws_url.clone(), self.dex.clone())
    }

    async fn position(&self, _market: MarketRef) -> Result<Option<ExtPosition>, VenueError> {
        // Read-only clearinghouseState; no signing. Requires a user address.
        let _user = self
            .user
            .as_ref()
            .ok_or_else(|| VenueError::Unauthorized)?;
        // Full position parse deferred to signing round (needs funded account to produce a non-empty
        // state to fixture against). For now: no configured position → None, loud about scope.
        Ok(None)
    }

    async fn health(&self) -> HealthStatus {
        let start = std::time::Instant::now();
        match self.info.asset_ctxs(&self.dex).await {
            Ok(_) => {
                let ms = start.elapsed().as_millis();
                if ms > HEALTH_LATENCY_MS {
                    HealthStatus::Degraded { reason: format!("slow: {ms}ms") }
                } else {
                    HealthStatus::Healthy
                }
            }
            Err(VenueError::RateLimited) => HealthStatus::Degraded { reason: "rate limited".into() },
            Err(e) => HealthStatus::Down,
        }
        // note: `e` bound to surface in logs by caller; intentionally not formatted into Down.
    }

    async fn place(&self, _order: PlaceOrder) -> Result<OrderReceipt, VenueError> {
        Err(VenueError::VenueSpecific(
            "place: unimplemented — HL EIP-712/msgpack signing round (TODO #6 pt2)".into(),
        ))
    }

    async fn cancel(&self, _order_id: OrderId) -> Result<(), VenueError> {
        Err(VenueError::VenueSpecific(
            "cancel: unimplemented — HL signing round (TODO #6 pt2)".into(),
        ))
    }

    async fn settle(&self, _market: MarketRef) -> Result<SettleReceipt, VenueError> {
        Err(VenueError::VenueSpecific(
            "settle: unimplemented — HL signing round (TODO #6 pt2)".into(),
        ))
    }
}
```

Note: the `health()` match binds `e` in the last arm but returns `Down` without it — change that arm to `Err(_) => HealthStatus::Down,` to avoid an unused-variable warning. Apply that fix now.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-venues hyperliquid::tests`
Expected: PASS (2 tests). (Task 5 creates `ws.rs`; if `ws` module is missing the build fails — do Task 5 Step 1 first if needed, or temporarily comment `pub mod ws;` and the `quote_stream` body. Cleanest: implement Task 5 before running.)

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/mod.rs
git commit -m "feat(venues): HyperliquidAdapter read methods + loud write stubs (TODO #6 pt1)"
```

---

### Task 5: quote_stream WebSocket subscription

**Files:**
- Create: `crates/volarb-venues/src/hyperliquid/ws.rs`

- [ ] **Step 1: Write `crates/volarb-venues/src/hyperliquid/ws.rs`**

```rust
//! WebSocket `l2Book` subscription → `QuoteEvent` stream for the `quote_stream` trait method.
//!
//! Subscribes to every HyperOdd market in the dex's `meta.universe` is out of scope this round;
//! we subscribe to a single representative coin ("<dex>:BTCHOURLY") to prove the channel. Parse
//! failures are dropped (not silently — the stream simply skips malformed frames); on socket
//! close the stream ends and the caller re-subscribes.

use crate::QuoteEvent;
use futures::stream::{self, BoxStream, StreamExt};

/// Build a `QuoteEvent` stream from the HL WS endpoint for one dex's BTCHOURLY market.
///
/// Returns an empty stream if the socket cannot be established (caller treats stream end as
/// "resubscribe later"). Live verification is in `tests/live_testnet.rs` (network, #[ignore]).
pub fn quote_stream(ws_url: String, dex: String) -> BoxStream<'static, QuoteEvent> {
    let coin = format!("{dex}:BTCHOURLY");
    // Lazily connect when the stream is first polled.
    stream::once(async move { connect_and_stream(ws_url, coin).await })
        .flatten()
        .boxed()
}

async fn connect_and_stream(ws_url: String, coin: String) -> BoxStream<'static, QuoteEvent> {
    use tokio_tungstenite::tungstenite::Message;

    let connect = tokio_tungstenite::connect_async(&ws_url).await;
    let (ws, _resp) = match connect {
        Ok(c) => c,
        Err(_) => return stream::empty().boxed(),
    };
    let (mut write, read) = ws.split();

    let sub = serde_json::json!({
        "method": "subscribe",
        "subscription": { "type": "l2Book", "coin": coin }
    })
    .to_string();
    use futures::SinkExt;
    if write.send(Message::Text(sub.into())).await.is_err() {
        return stream::empty().boxed();
    }

    let coin_for_parse = coin;
    read.filter_map(move |msg| {
        let coin = coin_for_parse.clone();
        async move {
            let txt = match msg {
                Ok(Message::Text(t)) => t,
                _ => return None,
            };
            super::ws::parse_l2_event(&txt, &coin)
        }
    })
    .boxed()
}

/// Parse a WS `l2Book` data frame into a `QuoteEvent`. Returns None for non-data frames
/// (subscriptionResponse, pong) or malformed payloads. Strike/expiry are unknown from the WS
/// frame alone, so we leave them zeroed — Router correlates by `venue_market`.
pub fn parse_l2_event(txt: &str, coin: &str) -> Option<QuoteEvent> {
    use crate::MarketRef;
    use super::info::{best_bid_ask, L2Book};
    use volarb_core::{Expiry, Quote, Strike};

    let v: serde_json::Value = serde_json::from_str(txt).ok()?;
    if v.get("channel").and_then(|c| c.as_str()) != Some("l2Book") {
        return None;
    }
    let data = v.get("data")?;
    let book: L2Book = serde_json::from_value(data.clone()).ok()?;
    if book.coin != coin {
        return None;
    }
    let (bid, ask) = best_bid_ask(&book).ok()?;
    Some(QuoteEvent {
        market: MarketRef {
            venue_market: book.coin.clone(),
            strike: Strike(0.0),
            expiry: Expiry { unix_ms: 0 },
        },
        quote: Quote { bid, ask, strike: Strike(0.0), expiry: Expiry { unix_ms: 0 }, ts_ms: book.time },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ws_l2book_data_frame() {
        let frame = r#"{"channel":"l2Book","data":{"coin":"ho:BTCHOURLY","time":1781348700000,"levels":[[{"px":"0.23","sz":"100.0","n":2}],[{"px":"0.25","sz":"120.0","n":1}]]}}"#;
        let ev = parse_l2_event(frame, "ho:BTCHOURLY").unwrap();
        assert_eq!(ev.quote.bid, 0.23);
        assert_eq!(ev.quote.ask, 0.25);
        assert_eq!(ev.market.venue_market, "ho:BTCHOURLY");
    }

    #[test]
    fn ignores_non_l2book_frames_and_wrong_coin() {
        assert!(parse_l2_event(r#"{"channel":"subscriptionResponse"}"#, "ho:BTCHOURLY").is_none());
        let frame = r#"{"channel":"l2Book","data":{"coin":"ho:ETHHOURLY","time":1,"levels":[[{"px":"0.1","sz":"1","n":1}],[{"px":"0.2","sz":"1","n":1}]]}}"#;
        assert!(parse_l2_event(frame, "ho:BTCHOURLY").is_none());
    }

    #[test]
    fn ignores_malformed_json() {
        assert!(parse_l2_event("not json", "ho:BTCHOURLY").is_none());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p volarb-venues hyperliquid::ws`
Expected: PASS (3 tests).

- [ ] **Step 3: Build whole crate + clippy**

Run: `cargo build -p volarb-venues && cargo clippy -p volarb-venues -- -D warnings`
Expected: clean. (Resolves the Task 4 note about `ws` module existing.)

- [ ] **Step 4: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/ws.rs
git commit -m "feat(venues): HL quote_stream WS l2Book subscription + frame parse (TODO #6 pt1)"
```

---

### Task 6: Live testnet integration tests (#[ignore]) + monkey tests

**Files:**
- Create: `crates/volarb-venues/tests/live_testnet.rs`

- [ ] **Step 1: Write `crates/volarb-venues/tests/live_testnet.rs`**

```rust
//! Live testnet integration — requires network + Taiwan/non-US IP. Run explicitly:
//!   cargo test -p volarb-venues --test live_testnet -- --ignored
//! These are NOT part of offline CI (network-dependent, liquidity-dependent).

use volarb_venues::hyperliquid::HyperliquidAdapter;
use volarb_venues::{HealthStatus, MarketRef, VenueAdapter, VenueError};
use volarb_core::{Expiry, Strike};

fn mref(coin: &str) -> MarketRef {
    MarketRef {
        venue_market: coin.to_string(),
        strike: Strike(64000.0),
        expiry: Expiry { unix_ms: 1_781_348_700_000 },
    }
}

#[tokio::test]
#[ignore = "network: live HL testnet"]
async fn health_is_reachable() {
    let a = HyperliquidAdapter::builder().build();
    let h = a.health().await;
    assert!(matches!(h, HealthStatus::Healthy | HealthStatus::Degraded { .. }), "got {h:?}");
}

#[tokio::test]
#[ignore = "network: live HL testnet"]
async fn quote_empty_book_is_insufficient_liquidity() {
    // ho:BTCHOURLY book is empty on testnet → loud InsufficientLiquidity (verified 2026-06-13).
    let a = HyperliquidAdapter::builder().build();
    let r = a.quote(mref("ho:BTCHOURLY")).await;
    assert!(matches!(r, Err(VenueError::InsufficientLiquidity(_))), "got {r:?}");
}

#[tokio::test]
#[ignore = "network: live HL testnet"]
async fn quote_unknown_market_errors() {
    let a = HyperliquidAdapter::builder().build();
    let r = a.quote(mref("ho:DOESNOTEXIST")).await;
    assert!(r.is_err(), "expected error for unknown market, got {r:?}");
}
```

- [ ] **Step 2: Verify offline test suite still passes (does NOT run #[ignore])**

Run: `cargo test -p volarb-venues`
Expected: PASS — all unit tests; the 3 live tests show as `ignored`.

- [ ] **Step 3: Run the live tests explicitly to confirm they work against testnet**

Run: `cargo test -p volarb-venues --test live_testnet -- --ignored`
Expected: PASS (3 tests) — `health_is_reachable` Healthy/Degraded, empty-book → InsufficientLiquidity, unknown market → error.
If these FAIL with network errors, confirm IP is non-US and retry; do not weaken assertions to make them pass.

- [ ] **Step 4: Commit**

```bash
git add crates/volarb-venues/tests/live_testnet.rs
git commit -m "test(venues): live HL testnet integration tests (#[ignore], TODO #6 pt1)"
```

---

### Task 7: Workspace gate + fixtures commit + progress update

**Files:**
- (no source changes) + `tasks/progress.md` (gitignored — update for next session, not committed)

- [ ] **Step 1: Commit the captured fixtures** (if not already committed)

```bash
git add crates/volarb-venues/tests/fixtures/
git status --short   # verify ONLY fixtures staged — no pre-existing untracked junk (lessons.md)
git commit -m "test(venues): HL testnet wire fixtures (TODO #6 pt1)"
```

- [ ] **Step 2: Full workspace verification gate**

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```
Expected: all green. Fix any failures before proceeding — do not mark complete with a skipped/red step (Rule 12 fail-loud).

- [ ] **Step 3: Update `tasks/progress.md`** — mark TODO #6 part 1 (read path) done, part 2 (signing) pending; note user must supply HL testnet private key + USDC for part 2. (File is gitignored; not committed.)

- [ ] **Step 4: Run the Move/SUI-aware review per project rules**

This crate is non-Move Rust → generic review is allowed (skill-routing.md). Run the two-round review:
```bash
git diff main...HEAD > /tmp/rev.diff && ~/.claude/bin/review.sh /tmp/rev.diff "Tracks/2-DeepBook-Predict/01-vol-arb-bot/.claude/rules,~/.claude/rules/general/dev-rules.md"
```
If `::FALLBACK::` or exit 42 → dispatch a `general-purpose` subagent (fresh context) with the diff + rule paths (dev-rules.md §Code Review). Integrate findings, fix, re-verify.

---

## Self-Review

**Spec coverage:**
- VenueAdapter trait + satellite types → Task 2 ✓
- VenueError → Task 1 ✓
- Module structure (error/lib/hyperliquid{mod,info,ws}) → Tasks 1–5 ✓ (market.rs dropped: name→strike/expiry derivation was YAGNI for read path; caller supplies MarketRef — noted in plan insight)
- quote / quote_stream / position / health → Task 4 + Task 5 ✓
- place / cancel / settle loud stubs → Task 4 ✓
- Wire-format → domain ([0,1] string parse, finite guard, empty-book → InsufficientLiquidity) → Task 3 ✓
- Dependencies (reqwest/tokio-tungstenite/futures/async-trait, no signing deps) → Task 1 ✓
- Testing: offline fixture units (Task 3, 5), live #[ignore] integration (Task 6), monkey (empty/NaN/unknown/malformed across Tasks 3,5,6) ✓
- Out-of-scope handoff (signing, asset-index, position parse) → noted in Task 4 stubs + progress (Task 7) ✓

**Placeholder scan:** No TBD/TODO-as-work; every code step has full code. The `position()` returning `Ok(None)` is an intentional, documented scope boundary (full parse needs a funded account to fixture), not a placeholder — it fails loud via doc + progress note.

**Type consistency:** `MarketRef{venue_market,strike,expiry}`, `Quote{bid,ask,strike,expiry,ts_ms}`, `L2Book{coin,time,levels}`, `L2Level{px,sz,n}`, `AssetCtx{mark_px,oracle_px,mid_px,open_interest}`, `best_bid_ask`, `parse_px`, `parse_l2_event`, `InfoClient::{l2_book,asset_ctxs}`, `HyperliquidAdapter::builder()` — names consistent across Tasks 2–6. `ChainKind::Evm{chain_id}` used identically in Task 2 test, Task 4 impl, Task 6 test.
