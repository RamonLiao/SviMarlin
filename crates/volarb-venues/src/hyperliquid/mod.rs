//! Hyperliquid HIP-4 venue adapter.

pub mod exchange;
pub mod info;
pub mod signing;
pub mod ws;

use crate::{
    ChainKind, ExtPosition, FeeModel, HealthStatus, MarketRef, OrderId, OrderReceipt, PlaceOrder,
    Quote, SettleReceipt, VenueAdapter, VenueError, VenueId,
};
use alloy_signer_local::PrivateKeySigner;
use async_trait::async_trait;
use futures::stream::BoxStream;
use info::InfoClient;

const MAINNET_API: &str = "https://api.hyperliquid.xyz";
const TESTNET_API: &str = "https://api.hyperliquid-testnet.xyz";
const TESTNET_WS: &str = "wss://api.hyperliquid-testnet.xyz/ws";
/// Latency over this (ms) downgrades health to Degraded.
const HEALTH_LATENCY_MS: u128 = 1_500;

/// Adapter for Hyperliquid HIP-4 outcome markets (HyperOdd builder dex).
///
/// Debug-safe: `signer` is `Option<PrivateKeySigner>`; alloy's `PrivateKeySigner` Debug redacts
/// the secret (it prints only the derived address / verifying key), so deriving Debug here cannot
/// leak `HL_TESTNET_PRIVATE_KEY`.
#[derive(Debug, Clone)]
pub struct HyperliquidAdapter {
    info: InfoClient,
    exchange: exchange::ExchangeClient,
    ws_url: String,
    dex: String,
    /// User address for `position()` reads (read-only, no signing). None → position() errors.
    user: Option<String>,
    /// Signing key (from `HL_TESTNET_PRIVATE_KEY`). None → write methods return `Unauthorized`.
    signer: Option<PrivateKeySigner>,
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
        // Signer from env only; parse silently (a bad/missing key → None → writes Unauthorized).
        // NEVER log the key or the parse error (the error Display can echo key material).
        let signer = std::env::var("HL_TESTNET_PRIVATE_KEY")
            .ok()
            .and_then(|k| k.parse::<PrivateKeySigner>().ok());
        HyperliquidAdapter {
            info: InfoClient::new(self.base_url.clone()),
            exchange: exchange::ExchangeClient::new(self.base_url),
            ws_url: self.ws_url,
            dex: self.dex,
            user: self.user,
            signer,
            testnet: self.testnet,
        }
    }
}

impl HyperliquidAdapter {
    /// Nonce for signed actions. HL requires ms-timestamp nonces within a recent window.
    /// Task 8 replaces this with a monotonic counter; for now use wall-clock ms.
    fn next_nonce(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Parse an HL `/exchange` order response into an [`OrderReceipt`].
///
/// Accepts the documented shapes:
/// - `{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":N}}]}}}`
/// - `{...{"filled":{"oid":N,"totalSz":"1.0","avgPx":"0.2"}}}`
/// - top-level error: `{"status":"err","response":"<msg>"}`
/// - per-status error: `{"error":"<msg>"}`
///
/// `coin` is threaded in so the receipt's `OrderId` round-trips as `"<coin>:<oid>"` (Task 6
/// cancel needs the coin back). Any missing/garbage field → `VenueSpecific`, never a panic.
fn parse_order_receipt(resp: &serde_json::Value, coin: &str) -> Result<OrderReceipt, VenueError> {
    // Top-level error status.
    if resp.get("status").and_then(|s| s.as_str()) == Some("err") {
        let msg = resp
            .get("response")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown HL error");
        return Err(VenueError::VenueSpecific(msg.to_string()));
    }
    let status = resp
        .get("response")
        .and_then(|r| r.get("data"))
        .and_then(|d| d.get("statuses"))
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| {
            VenueError::VenueSpecific(format!("unexpected /exchange response: {resp}"))
        })?;

    // Per-status error.
    if let Some(err) = status.get("error").and_then(|e| e.as_str()) {
        return Err(VenueError::VenueSpecific(err.to_string()));
    }

    if let Some(resting) = status.get("resting") {
        let oid = resting
            .get("oid")
            .and_then(|o| o.as_u64())
            .ok_or_else(|| VenueError::VenueSpecific("resting status missing oid".into()))?;
        return Ok(OrderReceipt {
            order_id: OrderId(format!("{coin}:{oid}")),
            filled_size: 0.0,
        });
    }
    if let Some(filled) = status.get("filled") {
        let oid = filled
            .get("oid")
            .and_then(|o| o.as_u64())
            .ok_or_else(|| VenueError::VenueSpecific("filled status missing oid".into()))?;
        let total_sz = filled
            .get("totalSz")
            .and_then(|s| s.as_str())
            .ok_or_else(|| VenueError::VenueSpecific("filled status missing totalSz".into()))?;
        let filled_size = info::parse_px(total_sz)?;
        return Ok(OrderReceipt {
            order_id: OrderId(format!("{coin}:{oid}")),
            filled_size,
        });
    }
    Err(VenueError::VenueSpecific(format!(
        "unrecognized order status: {status}"
    )))
}

/// Parse an HL `/exchange` cancel response into `Ok(())` or a loud error.
///
/// Accepts the documented shapes:
/// - success: `{"status":"ok","response":{"type":"cancel","data":{"statuses":["success"]}}}`
/// - per-status error: `{...statuses":[{"error":"<msg>"}]}`
/// - top-level error: `{"status":"err","response":"<msg>"}`
///
/// Money-path (Rule 12): never fabricate success from an error/garbage response. Any
/// missing/unexpected field → `VenueSpecific`, never a panic (no `.unwrap()`/indexing).
fn parse_cancel_response(resp: &serde_json::Value) -> Result<(), VenueError> {
    // Top-level error status.
    if resp.get("status").and_then(|s| s.as_str()) == Some("err") {
        let msg = resp
            .get("response")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown HL error");
        return Err(VenueError::VenueSpecific(msg.to_string()));
    }
    let status = resp
        .get("response")
        .and_then(|r| r.get("data"))
        .and_then(|d| d.get("statuses"))
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| {
            VenueError::VenueSpecific(format!("unexpected /exchange cancel response: {resp}"))
        })?;

    // Per-status object error: {"error":"<msg>"}.
    if let Some(err) = status.get("error").and_then(|e| e.as_str()) {
        return Err(VenueError::VenueSpecific(err.to_string()));
    }
    // Success is the bare string "success".
    if status.as_str() == Some("success") {
        return Ok(());
    }
    Err(VenueError::VenueSpecific(format!(
        "unrecognized cancel status: {status}"
    )))
}

#[async_trait]
impl VenueAdapter for HyperliquidAdapter {
    fn id(&self) -> VenueId {
        VenueId::Hyperliquid
    }

    fn chain(&self) -> ChainKind {
        // HL testnet = chain_id 998; mainnet = 1337 (HL's EVM chain id).
        ChainKind::Evm {
            chain_id: if self.testnet { 998 } else { 1337 },
        }
    }

    fn fees(&self) -> FeeModel {
        // HL standard taker/maker; builder-dex fee scale applies on top (deferred to signing round).
        FeeModel {
            taker_bps: 4.5,
            maker_bps: 1.5,
        }
    }

    async fn quote(&self, market: MarketRef) -> Result<Quote, VenueError> {
        let book = self.info.l2_book(&market.venue_market).await?;
        let (bid, ask) = info::best_bid_ask(&book)?;
        Ok(Quote {
            bid,
            ask,
            strike: market.strike,
            expiry: market.expiry,
            ts_ms: book.time,
        })
    }

    async fn quote_stream(&self) -> BoxStream<'static, crate::QuoteEvent> {
        ws::quote_stream(self.ws_url.clone(), self.dex.clone())
    }

    async fn position(&self, _market: MarketRef) -> Result<Option<ExtPosition>, VenueError> {
        // Read-only clearinghouseState; no signing. Requires a user address.
        let _user = self.user.as_ref().ok_or(VenueError::Unauthorized)?;
        // Parsing clearinghouseState is deferred to pt2 (needs a funded account to fixture a
        // non-empty state). Returning `Ok(None)` would lie "flat position" to the risk layer —
        // fail loud instead (Rule 12), so a bot never trades on a fabricated empty position.
        Err(VenueError::VenueSpecific(
            "position: clearinghouseState parse unimplemented — HL signing round (TODO #6 pt2)"
                .into(),
        ))
    }

    async fn health(&self) -> HealthStatus {
        let start = std::time::Instant::now();
        match self.info.asset_ctxs(&self.dex).await {
            Ok(_) => {
                let ms = start.elapsed().as_millis();
                if ms > HEALTH_LATENCY_MS {
                    HealthStatus::Degraded {
                        reason: format!("slow: {ms}ms"),
                    }
                } else {
                    HealthStatus::Healthy
                }
            }
            Err(VenueError::RateLimited) => HealthStatus::Degraded {
                reason: "rate limited".into(),
            },
            Err(_) => HealthStatus::Down,
        }
    }

    async fn place(&self, order: PlaceOrder) -> Result<OrderReceipt, VenueError> {
        let signer = self.signer.as_ref().ok_or(VenueError::Unauthorized)?;
        let coin = order.market.venue_market.rsplit(':').next().unwrap_or("");
        let asset = self.info.asset_index(&self.dex, coin).await?;
        // HyperOdd "up" outcome = long → is_buy true.
        let is_buy = matches!(order.side, volarb_core::Side::Up);
        let ow = signing::order_wire(asset, is_buy, order.price, order.size, false, "Gtc")?;
        let action = signing::OrderAction {
            r#type: "order",
            orders: vec![ow],
            grouping: "na",
        };
        let nonce = self.next_nonce();
        let sig = signing::sign_l1_action(signer, &action, nonce, None, !self.testnet)?;
        let resp = self.exchange.post_action(&action, &sig, nonce).await?;
        parse_order_receipt(&resp, coin)
    }

    async fn cancel(&self, order_id: OrderId) -> Result<(), VenueError> {
        let signer = self.signer.as_ref().ok_or(VenueError::Unauthorized)?;
        let (coin, oid_str) = order_id
            .0
            .split_once(':')
            .ok_or_else(|| VenueError::VenueSpecific("OrderId must be '<coin>:<oid>'".into()))?;
        let oid: u64 = oid_str
            .parse()
            .map_err(|_| VenueError::VenueSpecific("OrderId oid not numeric".into()))?;
        let asset = self.info.asset_index(&self.dex, coin).await?;
        let action = signing::CancelAction {
            r#type: "cancel",
            cancels: vec![signing::CancelWire { a: asset, o: oid }],
        };
        let nonce = self.next_nonce();
        let sig = signing::sign_l1_action(signer, &action, nonce, None, !self.testnet)?;
        let resp = self.exchange.post_action(&action, &sig, nonce).await?;
        parse_cancel_response(&resp)
    }

    async fn settle(&self, _market: MarketRef) -> Result<SettleReceipt, VenueError> {
        Err(VenueError::VenueSpecific(
            "settle: unimplemented — HL signing round (TODO #6 pt2)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OrderId, PlaceOrder};
    use volarb_core::{Expiry, Side, Strike};

    fn mref() -> crate::MarketRef {
        crate::MarketRef {
            venue_market: "ho:BTCHOURLY".to_string(),
            strike: Strike(64000.0),
            expiry: Expiry {
                unix_ms: 1_781_348_700_000,
            },
        }
    }

    #[test]
    fn adapter_metadata_is_hyperliquid_testnet() {
        let a = HyperliquidAdapter::builder().build();
        assert_eq!(a.id(), VenueId::Hyperliquid);
        assert_eq!(a.chain(), ChainKind::Evm { chain_id: 998 });
    }

    #[tokio::test]
    async fn settle_fails_loud_place_cancel_need_signer() {
        // No HL_TESTNET_PRIVATE_KEY in the test env → no signer. `place` and `cancel` both sign,
        // so without a key they return Unauthorized (never fabricate, never hit the network). Only
        // settle is still unimplemented → fails loud as VenueSpecific.
        let a = HyperliquidAdapter::builder().build();
        assert!(matches!(
            a.settle(mref()).await,
            Err(VenueError::VenueSpecific(_))
        ));
        assert!(matches!(
            a.place(PlaceOrder {
                market: mref(),
                side: Side::Up,
                price: 0.2,
                size: 1.0
            })
            .await,
            Err(VenueError::Unauthorized)
        ));
        assert!(matches!(
            a.cancel(OrderId("123".into())).await,
            Err(VenueError::Unauthorized)
        ));
    }

    #[test]
    fn parse_cancel_response_maps_all_shapes() {
        // ok + success → Ok(()).
        let ok = serde_json::json!({"status":"ok","response":{"type":"cancel",
            "data":{"statuses":["success"]}}});
        assert!(super::parse_cancel_response(&ok).is_ok());

        // per-status object error → VenueSpecific carrying the message.
        let serr = serde_json::json!({"status":"ok","response":{"type":"cancel",
            "data":{"statuses":[{"error":"Order was never placed, already canceled, or filled"}]}}});
        assert!(matches!(
            super::parse_cancel_response(&serr),
            Err(VenueError::VenueSpecific(m))
                if m == "Order was never placed, already canceled, or filled"
        ));

        // top-level err → VenueSpecific carrying the HL message.
        let terr = serde_json::json!({"status":"err","response":"Must deposit before trading"});
        assert!(matches!(
            super::parse_cancel_response(&terr),
            Err(VenueError::VenueSpecific(m)) if m == "Must deposit before trading"
        ));

        // garbage / missing fields → VenueSpecific, no panic.
        let junk = serde_json::json!({"status":"ok","response":{}});
        assert!(matches!(
            super::parse_cancel_response(&junk),
            Err(VenueError::VenueSpecific(_))
        ));
    }

    #[test]
    fn parse_order_receipt_maps_all_shapes() {
        // resting → oid in id, filled_size 0.
        let resting = serde_json::json!({"status":"ok","response":{"type":"order",
            "data":{"statuses":[{"resting":{"oid":123}}]}}});
        let r = super::parse_order_receipt(&resting, "BTCHOURLY").unwrap();
        assert_eq!(r.order_id, OrderId("BTCHOURLY:123".into()));
        assert_eq!(r.filled_size, 0.0);

        // filled → totalSz parsed.
        let filled = serde_json::json!({"status":"ok","response":{"type":"order",
            "data":{"statuses":[{"filled":{"oid":7,"totalSz":"1.5","avgPx":"0.2"}}]}}});
        let r = super::parse_order_receipt(&filled, "X").unwrap();
        assert_eq!(r.order_id, OrderId("X:7".into()));
        assert_eq!(r.filled_size, 1.5);

        // top-level err → VenueSpecific carrying the HL message.
        let err = serde_json::json!({"status":"err","response":"insufficient margin"});
        assert!(matches!(
            super::parse_order_receipt(&err, "X"),
            Err(VenueError::VenueSpecific(m)) if m == "insufficient margin"
        ));

        // per-status error → VenueSpecific.
        let serr = serde_json::json!({"status":"ok","response":{"type":"order",
            "data":{"statuses":[{"error":"Order rejected"}]}}});
        assert!(matches!(
            super::parse_order_receipt(&serr, "X"),
            Err(VenueError::VenueSpecific(_))
        ));

        // garbage / missing fields → VenueSpecific, no panic.
        let junk = serde_json::json!({"status":"ok","response":{}});
        assert!(matches!(
            super::parse_order_receipt(&junk, "X"),
            Err(VenueError::VenueSpecific(_))
        ));
    }

    #[tokio::test]
    async fn position_without_user_is_unauthorized() {
        let a = HyperliquidAdapter::builder().build();
        assert!(matches!(
            a.position(mref()).await,
            Err(VenueError::Unauthorized)
        ));
    }

    #[tokio::test]
    async fn position_with_user_fails_loud_not_flat() {
        // With a user set we still can't parse state yet — must error loud, never lie Ok(None).
        let a = HyperliquidAdapter::builder().user("0xabc").build();
        assert!(matches!(
            a.position(mref()).await,
            Err(VenueError::VenueSpecific(_))
        ));
    }
}
