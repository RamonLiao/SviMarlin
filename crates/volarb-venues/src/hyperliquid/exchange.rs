//! Hyperliquid /exchange POST client (signed write actions).
use crate::VenueError;
use serde::Serialize;

const TIMEOUT_SECS: u64 = 10;

#[derive(Serialize)]
struct ExchangeRequest<'a, A: Serialize> {
    action: &'a A,
    nonce: u64,
    signature: &'a crate::hyperliquid::signing::Signature,
    #[serde(rename = "vaultAddress")]
    vault_address: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExchangeClient {
    http: reqwest::Client,
    base_url: String,
}

impl ExchangeClient {
    pub(crate) fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("reqwest client");
        Self { http, base_url }
    }

    pub(crate) async fn post_action<A: Serialize>(
        &self,
        action: &A,
        signature: &crate::hyperliquid::signing::Signature,
        nonce: u64,
    ) -> Result<serde_json::Value, VenueError> {
        let body = ExchangeRequest {
            action,
            nonce,
            signature,
            vault_address: None,
        };
        let resp = self
            .http
            .post(format!("{}/exchange", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| VenueError::Network(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(VenueError::RateLimited);
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| VenueError::Network(e.to_string()))?;
        Ok(v)
    }
}
