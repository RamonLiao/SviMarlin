use crate::numeric::{Expiry, Strike, VolPoints};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Calendar-day annualization (L1 display day-count, design §3.2). The authoritative
/// day-count is the chain's (`oracle::compute_price`); L0/L3 (Plan B) measure any gap.
/// `pub` so the fitter (`volarb-pricing`) uses the SAME constant — if eval and fit disagree on
/// day-count, the fitted smile won't reproduce under eval (silent bug).
pub const MS_PER_YEAR: f64 = 365.0 * 24.0 * 3600.0 * 1000.0;

/// Gatheral raw SVI parameters for a single smile (one expiry). Design spec §3.2.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SVIParams {
    pub a: f64,
    pub b: f64,
    pub rho: f64,
    pub m: f64,
    pub sigma: f64,
}

/// One expiry's smile: SVI params plus the forward they were measured against. Mirrors the
/// chain: each `oracle::OracleSVI` object carries its own `svi` params AND `prices.forward`
/// (design §3.1 / §7). Forward lives WITH params so they cannot desync across snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Smile {
    pub params: SVIParams,
    pub forward: f64,
}

/// Implied-vol surface: an off-chain aggregation of N per-expiry on-chain `OracleSVI` objects.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SVISurface {
    /// Snapshot time for the §242 staleness gate ONLY — NOT used to derive time-to-expiry.
    pub as_of_ms: u64,
    /// expiry `unix_ms` -> smile.
    pub per_expiry: BTreeMap<u64, Smile>,
}

impl SVISurface {
    /// Annualized implied vol at `(strike, expiry)`, valued at `now_ms`. Returns `None` if no
    /// smile exists for that expiry, the strike/forward is non-positive, total variance is
    /// negative (defensive), or the option has expired (`T <= 0`). Result is in vol points
    /// (annualized sigma x 100, e.g. `VolPoints(80.0)` == 80% vol).
    pub fn sigma_at(&self, strike: Strike, expiry: Expiry, now_ms: u64) -> Option<VolPoints> {
        let smile = self.per_expiry.get(&expiry.unix_ms)?;
        let f = smile.forward;
        if f <= 0.0 || strike.0 <= 0.0 {
            return None;
        }
        let k = (strike.0 / f).ln();
        let p = &smile.params;
        let d = k - p.m;
        let w = p.a + p.b * (p.rho * d + (d * d + p.sigma * p.sigma).sqrt());
        if w < 0.0 {
            return None;
        }
        let t = (expiry.unix_ms as f64 - now_ms as f64) / MS_PER_YEAR;
        if t <= 0.0 {
            return None;
        }
        let sigma = (w / t).sqrt();
        if !sigma.is_finite() {
            return None;
        }
        Some(VolPoints(sigma * 100.0))
    }

    /// §242 staleness gate: true if the snapshot is older than `max_age_ms`.
    pub fn is_stale(&self, now_ms: u64, max_age_ms: u64) -> bool {
        now_ms.saturating_sub(self.as_of_ms) > max_age_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn smile() -> Smile {
        Smile {
            params: SVIParams {
                a: 0.04,
                b: 0.4,
                rho: -0.3,
                m: 0.0,
                sigma: 0.1,
            },
            forward: 64_000.0,
        }
    }

    // WHY: "no smile for this expiry" is normal control flow (router skips that venue), not a
    // panic. The `?` on the absent key must short-circuit before any arithmetic.
    #[test]
    fn sigma_at_absent_expiry_returns_none() {
        let surface = SVISurface::default();
        assert!(
            surface
                .sigma_at(
                    Strike(50_000.0),
                    Expiry {
                        unix_ms: 1_700_000_000_000
                    },
                    0
                )
                .is_none()
        );
    }

    // WHY: eval is the money-path formula both legs share; pin it against a hand-computed value.
    // At-the-money (strike == forward) => k = 0, d = -m = 0, so w = a + b*sigma = 0.04 + 0.4*0.1
    // = 0.08. With T = 0.25y, annualized sigma = sqrt(0.08/0.25) = sqrt(0.32) = 0.565685..., so
    // VolPoints = 56.5685... Pin to 1e-9.
    #[test]
    fn sigma_at_atm_matches_hand_computation() {
        let mut surface = SVISurface {
            as_of_ms: 0,
            per_expiry: BTreeMap::new(),
        };
        let now = 0u64;
        let expiry = (MS_PER_YEAR * 0.25) as u64; // T = 0.25 years
        surface.per_expiry.insert(expiry, smile());
        let vp = surface
            .sigma_at(Strike(64_000.0), Expiry { unix_ms: expiry }, now)
            .expect("smile present, not expired");
        assert!((vp.0 - 56.568_542_494_923_804).abs() < 1e-9, "got {}", vp.0);
    }

    // WHY: an expired option (T <= 0) must not produce a bogus huge vol or NaN; the router must
    // see None and skip, not act on garbage.
    #[test]
    fn sigma_at_expired_returns_none() {
        let mut surface = SVISurface {
            as_of_ms: 0,
            per_expiry: BTreeMap::new(),
        };
        surface.per_expiry.insert(1_000, smile());
        assert!(
            surface
                .sigma_at(Strike(64_000.0), Expiry { unix_ms: 1_000 }, 1_000)
                .is_none()
        );
        assert!(
            surface
                .sigma_at(Strike(64_000.0), Expiry { unix_ms: 1_000 }, 2_000)
                .is_none()
        );
    }

    // WHY: staleness gate (§242) is what halts trading on a frozen feed; off-by-one here means
    // trading on stale vol. Pin the boundary exactly.
    #[test]
    fn is_stale_boundary() {
        let surface = SVISurface {
            as_of_ms: 1_000,
            per_expiry: BTreeMap::new(),
        };
        assert!(!surface.is_stale(61_000, 60_000)); // age == 60_000, not > 60_000
        assert!(surface.is_stale(61_001, 60_000)); // age 60_001 > 60_000
    }
}
