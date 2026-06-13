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
    async fn write_methods_fail_loud() {
        let a = HyperliquidAdapter::builder().build();
        let order = PlaceOrder {
            market: mref(),
            side: Side::Up,
            price: 0.2,
            size: 1.0,
        };
        assert!(matches!(
            a.place(order).await,
            Err(VenueError::VenueSpecific(_))
        ));
        assert!(matches!(
            a.cancel(OrderId("x".into())).await,
            Err(VenueError::VenueSpecific(_))
        ));
        assert!(matches!(
            a.settle(mref()).await,
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
}
