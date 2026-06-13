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
