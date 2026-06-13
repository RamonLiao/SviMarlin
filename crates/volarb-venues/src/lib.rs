//! volarb-venues — VenueAdapter trait + per-venue adapter modules.
//!
//! VenueAdapter is the architectural keystone (spec §4 / ADR-003): every external prediction
//! market implements it, so Router/Executor stay venue-agnostic by construction.
//!
//! MVP modules: hyperliquid (read path landed TODO #6 pt1; write path = pt2 signing round).

pub mod error;
pub use error::VenueError;

pub mod hyperliquid;

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
    Evm {
        chain_id: u64,
    },
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
        let m = MarketRef {
            venue_market: "ho:BTCHOURLY".to_string(),
            strike: Strike(64000.0),
            expiry: Expiry {
                unix_ms: 1_781_348_700_000,
            },
        };
        assert_eq!(m.venue_market, "ho:BTCHOURLY");
        assert_eq!(m.strike.0, 64000.0);
    }
}
