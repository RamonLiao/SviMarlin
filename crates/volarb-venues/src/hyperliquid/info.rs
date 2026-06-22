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
    let (bid, ask) = (parse_px(&bid.px)?, parse_px(&ask.px)?);
    validate_quote(&book.coin, bid, ask)?;
    Ok((bid, ask))
}

/// Enforce the `core::Quote` contract for binary outcome markets: prices in `[0,1]` and a
/// non-crossed book (`bid <= ask`). A venue returning out-of-range or crossed prices is an
/// anomaly the Router must not act on — surface loud rather than pass garbage downstream.
fn validate_quote(coin: &str, bid: f64, ask: f64) -> Result<(), VenueError> {
    if !(0.0..=1.0).contains(&bid) || !(0.0..=1.0).contains(&ask) {
        return Err(VenueError::VenueSpecific(format!(
            "{coin}: price out of [0,1] range (bid={bid}, ask={ask})"
        )));
    }
    if bid > ask {
        return Err(VenueError::VenueSpecific(format!(
            "{coin}: crossed book (bid={bid} > ask={ask})"
        )));
    }
    Ok(())
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

/// HTTP client for the `/info` endpoint of a Hyperliquid API host.
#[derive(Debug, Clone)]
pub struct InfoClient {
    base_url: String,
    http: reqwest::Client,
}

impl InfoClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        // Bounded timeouts: a Router health check must never hang on a half-open socket.
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base_url: base_url.into(),
            http,
        }
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
        resp.json::<T>()
            .await
            .map_err(|e| VenueError::Network(e.to_string()))
    }

    /// Fetch the L2 order book for a venue-native coin id (e.g. "ho:BTCHOURLY").
    pub async fn l2_book(&self, coin: &str) -> Result<L2Book, VenueError> {
        self.post(serde_json::json!({ "type": "l2Book", "coin": coin }))
            .await
    }

    /// Fetch `[meta, ctxs]` for a builder dex. Returns the ctxs vec (index-aligned to universe).
    pub async fn asset_ctxs(&self, dex: &str) -> Result<Vec<AssetCtx>, VenueError> {
        let raw: (serde_json::Value, Vec<AssetCtx>) = self
            .post(serde_json::json!({ "type": "metaAndAssetCtxs", "dex": dex }))
            .await?;
        Ok(raw.1)
    }

    /// Resolve the HL builder-dex asset index for `(dex, coin)`.
    ///
    /// Fetches `perpDexs` to find the perp-dex ordinal for `dex`, then per-dex `meta` to find
    /// the coin's index in `universe[]`, and combines them via [`builder_asset_index`].
    ///
    /// NOTE: the offset formula in [`builder_asset_index`] is the SUSPECTED HL convention and is
    /// unverified until the Task 9 live probe confirms it against the real `perpDexs` ordering.
    pub(crate) async fn asset_index(&self, dex: &str, coin: &str) -> Result<u32, VenueError> {
        // `perpDexs` → array; entry 0 is the base perp dex (null), builder dexes follow with a
        // `{"name": ...}` object. Find the ordinal whose name matches `dex`.
        let perp_dexs: serde_json::Value =
            self.post(serde_json::json!({ "type": "perpDexs" })).await?;
        let arr = perp_dexs
            .as_array()
            .ok_or_else(|| VenueError::VenueSpecific("perpDexs: not an array".into()))?;
        let perp_dex_index = arr
            .iter()
            .position(|d| d.get("name").and_then(|n| n.as_str()) == Some(dex))
            .ok_or_else(|| VenueError::VenueSpecific(format!("unknown perp dex: {dex}")))?
            as u32;

        // Per-dex `meta` → `universe[]`; find the coin's index. `coin` here is the bare coin
        // name (the part after the last ':' in a venue_market).
        let meta: serde_json::Value = self
            .post(serde_json::json!({ "type": "meta", "dex": dex }))
            .await?;
        let universe = meta
            .get("universe")
            .and_then(|u| u.as_array())
            .ok_or_else(|| VenueError::VenueSpecific("meta: missing universe array".into()))?;
        let coin_index = universe
            .iter()
            .position(|a| a.get("name").and_then(|n| n.as_str()) == Some(coin))
            .ok_or_else(|| {
                VenueError::VenueSpecific(format!("unknown coin: {coin} in dex {dex}"))
            })? as u32;

        Ok(builder_asset_index(perp_dex_index, coin_index))
    }
}

/// Builder-dex asset index per HL convention.
///
/// SUSPECTED formula: `100000 + perp_dex_index*10000 + coin_index_in_dex`. Verify against the
/// live `perpDexs` ordering (Task 9 probe) before trusting it on the money path.
pub(crate) fn builder_asset_index(perp_dex_index: u32, coin_index_in_dex: u32) -> u32 {
    100_000 + perp_dex_index * 10_000 + coin_index_in_dex
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

    #[test]
    fn rejects_out_of_range_and_crossed_books() {
        // price > 1 on a probability market → VenueSpecific anomaly.
        let oor = r#"{"coin":"ho:X","time":1,"levels":[[{"px":"0.4","sz":"1","n":1}],[{"px":"1.5","sz":"1","n":1}]]}"#;
        let book: L2Book = serde_json::from_str(oor).unwrap();
        assert!(matches!(
            best_bid_ask(&book),
            Err(VenueError::VenueSpecific(_))
        ));
        // crossed book bid > ask → VenueSpecific.
        let crossed = r#"{"coin":"ho:X","time":1,"levels":[[{"px":"0.6","sz":"1","n":1}],[{"px":"0.4","sz":"1","n":1}]]}"#;
        let book: L2Book = serde_json::from_str(crossed).unwrap();
        assert!(matches!(
            best_bid_ask(&book),
            Err(VenueError::VenueSpecific(_))
        ));
    }

    #[test]
    fn builder_asset_index_applies_offset() {
        // perp_dex_index 1 (after the base perp dex 0), coin index 2 in universe →
        // 100000 + 1*10000 + 2 = 110002.
        assert_eq!(super::builder_asset_index(1, 2), 110_002);
    }

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
}
