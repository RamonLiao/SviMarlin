//! Live testnet integration — requires network + Taiwan/non-US IP. Run explicitly:
//!   cargo test -p volarb-venues --test live_testnet -- --ignored
//! These are NOT part of offline CI (network-dependent, liquidity-dependent).

use volarb_core::{Expiry, Strike};
use volarb_venues::hyperliquid::HyperliquidAdapter;
use volarb_venues::{HealthStatus, MarketRef, VenueAdapter, VenueError};

fn mref(coin: &str) -> MarketRef {
    MarketRef {
        venue_market: coin.to_string(),
        strike: Strike(64000.0),
        expiry: Expiry {
            unix_ms: 1_781_348_700_000,
        },
    }
}

#[tokio::test]
#[ignore = "network: live HL testnet"]
async fn health_is_reachable() {
    let a = HyperliquidAdapter::builder().build();
    let h = a.health().await;
    assert!(
        matches!(h, HealthStatus::Healthy | HealthStatus::Degraded { .. }),
        "got {h:?}"
    );
}

#[tokio::test]
#[ignore = "network: live HL testnet"]
async fn quote_empty_book_is_insufficient_liquidity() {
    // ho:BTCHOURLY book is empty on testnet → loud InsufficientLiquidity (verified 2026-06-13).
    let a = HyperliquidAdapter::builder().build();
    let r = a.quote(mref("ho:BTCHOURLY")).await;
    assert!(
        matches!(r, Err(VenueError::InsufficientLiquidity(_))),
        "got {r:?}"
    );
}

#[tokio::test]
#[ignore = "network: live HL testnet"]
async fn quote_unknown_market_errors() {
    let a = HyperliquidAdapter::builder().build();
    let r = a.quote(mref("ho:DOESNOTEXIST")).await;
    assert!(r.is_err(), "expected error for unknown market, got {r:?}");
}
