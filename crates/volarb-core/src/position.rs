use crate::market::Side;
use crate::numeric::{Expiry, Strike, UsdcAmount, VolPoints};
use serde::{Deserialize, Serialize};

/// An open position on one leg.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub side: Side,
    pub size: UsdcAmount,
    pub entry_iv: VolPoints,
    pub strike: Strike,
    pub expiry: Expiry,
}
