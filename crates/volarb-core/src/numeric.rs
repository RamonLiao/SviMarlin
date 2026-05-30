use serde::{Deserialize, Serialize};

/// BTC price level (pricing domain, `f64`). On-chain strike conversion deferred to TODO #6
/// (`StrikeTicks(u64)`, design spec §3.2) — do NOT convert here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Strike(pub f64);

/// Expiry as Clock-based wall-clock unix milliseconds (ADR-007), NOT epoch time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Expiry {
    pub unix_ms: u64,
}

/// USDC amount in 6-decimal fixed point (on-chain `u64`). testnet QuoteAsset decimals
/// UNVERIFIED — see design spec §3.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsdcAmount(pub u64);

impl UsdcAmount {
    pub const DECIMALS: u32 = 6;
    const SCALE: f64 = 1_000_000.0; // 10^DECIMALS

    /// Fixed point → human USDC value.
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / Self::SCALE
    }

    /// Human USDC value → fixed point. Rounds to nearest tick (`f64::round`, half away from
    /// zero); non-positive inputs clamp to 0 because on-chain amounts are unsigned.
    pub fn from_f64(v: f64) -> Self {
        if v <= 0.0 {
            return UsdcAmount(0);
        }
        UsdcAmount((v * Self::SCALE).round() as u64)
    }
}

/// Implied vol expressed in vol points (router edge unit). `VolPoints(1.0)` == 1 vol point.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VolPoints(pub f64);

#[cfg(test)]
mod tests {
    use super::*;

    // WHY: the Sui leg and the external venue leg are sized from the same USDC figure; if
    // ticks don't round-trip exactly, the two legs drift apart and the hedge is mis-sized.
    #[test]
    fn usdc_amount_roundtrip_at_tick_resolution() {
        for raw in [0u64, 1, 1_000_000, 123_456_789] {
            // values below 2^53 are exactly representable in f64, so the round-trip is exact.
            let a = UsdcAmount(raw);
            assert_eq!(UsdcAmount::from_f64(a.to_f64()), a, "raw={raw}");
        }
    }

    // WHY: a 6dp rounding/truncation bug mis-sizes positions on the money path. Pin the
    // rounding DIRECTION with values away from the exact .5 boundary (avoids f64 repr flake).
    #[test]
    fn usdc_amount_rounding_and_clamp() {
        assert_eq!(UsdcAmount::from_f64(0.0000016).0, 2); // 1.6 ticks -> 2
        assert_eq!(UsdcAmount::from_f64(0.0000013).0, 1); // 1.3 ticks -> 1
        assert_eq!(UsdcAmount::from_f64(-5.0), UsdcAmount(0)); // unsigned clamp
        assert_eq!(UsdcAmount(1).to_f64(), 0.000_001);
    }
}
