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
