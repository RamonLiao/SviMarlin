use crate::numeric::{Expiry, Strike};
use serde::{Deserialize, Serialize};

/// Binary outcome leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Up,
    Down,
}

/// A venue quote at a `(strike, expiry)` market point. `bid`/`ask` are binary prices in [0,1].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub bid: f64,
    pub ask: f64,
    pub strike: Strike,
    pub expiry: Expiry,
    pub ts_ms: u64,
}
