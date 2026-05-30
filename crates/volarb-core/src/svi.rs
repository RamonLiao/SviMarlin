use crate::numeric::{Expiry, Strike, VolPoints};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Gatheral raw SVI parameters for a single smile (one expiry). Design spec §3.2.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SVIParams {
    pub a: f64,
    pub b: f64,
    pub rho: f64,
    pub m: f64,
    pub sigma: f64,
}

/// Implied-vol surface: one SVI smile per expiry, keyed by expiry `unix_ms`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SVISurface {
    pub per_expiry: BTreeMap<u64, SVIParams>,
}

impl SVISurface {
    /// Implied vol at `(strike, expiry)`. Returns `None` if no smile exists for that expiry.
    /// SVI evaluation math lands in TODO #5 (`volarb-pricing`) — the eval path is `todo!()`.
    pub fn sigma_at(&self, strike: Strike, expiry: Expiry) -> Option<VolPoints> {
        let _params = self.per_expiry.get(&expiry.unix_ms)?;
        let _ = strike;
        todo!("SVI evaluation — TODO #5 (volarb-pricing)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // WHY: "no smile for this expiry" is normal control flow (router skips that venue), not a
    // panic. The `?` on the absent key must short-circuit BEFORE the unimplemented eval path.
    #[test]
    fn sigma_at_absent_expiry_returns_none_without_panicking() {
        let surface = SVISurface::default();
        let r = surface.sigma_at(Strike(50_000.0), Expiry { unix_ms: 1_700_000_000_000 });
        assert!(r.is_none());
    }
}
