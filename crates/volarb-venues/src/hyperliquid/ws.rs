//! WebSocket `l2Book` subscription → `QuoteEvent` stream for the `quote_stream` trait method.
//!
//! Subscribing to every HyperOdd market in the dex's `meta.universe` is out of scope this round;
//! we subscribe to a single representative coin ("<dex>:BTCHOURLY") to prove the channel. Parse
//! failures are dropped (the stream skips malformed frames); on socket close the stream ends and
//! the caller re-subscribes.

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
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let (ws, _resp) = match tokio_tungstenite::connect_async(&ws_url).await {
        Ok(c) => c,
        Err(_) => return stream::empty().boxed(),
    };
    let (mut write, read) = ws.split();

    let sub = serde_json::json!({
        "method": "subscribe",
        "subscription": { "type": "l2Book", "coin": coin }
    })
    .to_string();
    if write.send(Message::Text(sub)).await.is_err() {
        return stream::empty().boxed();
    }

    read.filter_map(move |msg| {
        let coin = coin.clone();
        async move {
            match msg {
                Ok(Message::Text(t)) => parse_l2_event(t.as_str(), &coin),
                _ => None,
            }
        }
    })
    .boxed()
}

/// Parse a WS `l2Book` data frame into a `QuoteEvent`. Returns None for non-data frames
/// (subscriptionResponse, pong) or malformed payloads.
///
/// ⚠️ pt1 LIMITATION (TODO #6 pt2): `strike`/`expiry` are emitted as `0` placeholders — the WS
/// frame carries only the `venue_market` string, and name→(strike,expiry) derivation is deferred
/// (YAGNI this round; no Router consumer exists yet). Consumers MUST correlate by `venue_market`,
/// NOT by strike/expiry, until pt2 lands real coordinates. Single hardcoded `<dex>:BTCHOURLY`
/// subscription is likewise a pt1 channel-proof, not the full multi-market stream.
pub fn parse_l2_event(txt: &str, coin: &str) -> Option<QuoteEvent> {
    use super::info::{L2Book, best_bid_ask};
    use crate::MarketRef;
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
        quote: Quote {
            bid,
            ask,
            strike: Strike(0.0),
            expiry: Expiry { unix_ms: 0 },
            ts_ms: book.time,
        },
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
