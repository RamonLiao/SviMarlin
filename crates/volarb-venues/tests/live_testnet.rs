//! Live testnet integration — requires network + Taiwan/non-US IP. Run explicitly:
//!   cargo test -p volarb-venues --test live_testnet -- --ignored
//! These are NOT part of offline CI (network-dependent, liquidity-dependent).

use volarb_core::{Expiry, Side, Strike};
use volarb_venues::hyperliquid::HyperliquidAdapter;
use volarb_venues::{HealthStatus, MarketRef, PlaceOrder, VenueAdapter, VenueError};

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

#[tokio::test]
#[ignore = "live testnet; requires HL_TESTNET_PRIVATE_KEY"]
async fn live_signed_place_fails_on_margin_not_signature() {
    let Ok(_key) = std::env::var("HL_TESTNET_PRIVATE_KEY") else {
        return;
    };
    let user = std::env::var("HL_TESTNET_ACCOUNT_ADDRESS").expect("account addr");
    let a = HyperliquidAdapter::builder().user(user).build();
    let market = MarketRef {
        venue_market: "ho:BTCHOURLY".into(),
        strike: Strike(64000.0),
        expiry: Expiry {
            unix_ms: 1_781_348_700_000,
        },
    };
    let res = a
        .place(PlaceOrder {
            market,
            side: Side::Up,
            price: 0.05,
            size: 1.0,
        })
        .await;
    let err = res.expect_err("unfunded place must fail");
    let msg = format!("{err:?}").to_lowercase();
    assert!(
        !msg.contains("signature")
            && !msg.contains("does not exist")
            && !msg.contains("deserialize"),
        "signature/format rejected — signing is WRONG: {msg}"
    );
    // expected: margin/insufficient/funds-class error → signature was accepted.
    eprintln!("live place rejected as expected: {msg}");
}
