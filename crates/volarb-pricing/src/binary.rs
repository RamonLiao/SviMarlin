//! BS binary digital pricing + implied-vol inversion (HL leg).

use statrs::distribution::{ContinuousCDF, Normal};

/// Standard normal CDF.
fn norm_cdf(x: f64) -> f64 {
    // Normal::new(0,1) is infallible for these args; unwrap is fine.
    Normal::new(0.0, 1.0).unwrap().cdf(x)
}

/// Price of a cash-or-nothing binary call (pays 1 if terminal price > strike), under
/// Black-Scholes with r = 0 priced off the forward `F`: `p = N(d2)`,
/// `d2 = (ln(F/K) - 0.5*sigma^2*T) / (sigma*sqrt(T))`. Returns `NaN` on non-positive inputs.
pub fn binary_price(forward: f64, strike: f64, t_years: f64, sigma: f64) -> f64 {
    if t_years <= 0.0 || sigma <= 0.0 || forward <= 0.0 || strike <= 0.0 {
        return f64::NAN;
    }
    let sqrt_t = t_years.sqrt();
    let d2 = ((forward / strike).ln() - 0.5 * sigma * sigma * t_years) / (sigma * sqrt_t);
    norm_cdf(d2)
}

/// Bisect for sigma on `[lo, hi]` where `binary_price(.., sigma) - target` is monotone and
/// brackets a root. Returns `None` if not bracketed.
fn bisect_sigma(
    target: f64,
    forward: f64,
    strike: f64,
    t: f64,
    mut lo: f64,
    mut hi: f64,
) -> Option<f64> {
    let g = |s: f64| binary_price(forward, strike, t, s) - target;
    let (mut glo, ghi) = (g(lo), g(hi));
    if glo.abs() < 1e-12 {
        return Some(lo);
    }
    if ghi.abs() < 1e-12 {
        return Some(hi);
    }
    if glo * ghi > 0.0 {
        return None;
    }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        let gm = g(mid);
        if gm.abs() < 1e-12 || (hi - lo) < 1e-12 {
            return Some(mid);
        }
        if glo * gm < 0.0 {
            hi = mid;
        } else {
            lo = mid;
            glo = gm;
        }
    }
    Some(0.5 * (lo + hi))
}

/// Recover Black-Scholes implied vol from a binary-call price `p` in (0, 1).
///
/// A digital's vega changes sign, so price-vs-sigma is non-monotone for out-of-the-money calls
/// (`F < K`): it rises to a peak `p_max` at `sigma* = sqrt(2*ln(K/F)/T)` then falls, giving 0 or
/// 2 solutions. We return the **lower-vol branch** (`sigma <= sigma*`) and `None` when the target
/// exceeds `p_max` (unreachable on that branch). For `F >= K` price is monotone-decreasing in
/// sigma and the solution is unique. Returns `None` on out-of-range / degenerate inputs.
pub fn implied_vol_from_binary(p: f64, forward: f64, strike: f64, t_years: f64) -> Option<f64> {
    if !(p > 0.0 && p < 1.0) || t_years <= 0.0 || forward <= 0.0 || strike <= 0.0 {
        return None;
    }
    let c = (forward / strike).ln();
    let lo = 1e-6;
    if c >= 0.0 {
        // F >= K: price is monotone-decreasing in sigma. For ITM (F > K) price -> 1 as sigma -> 0;
        // for exactly ATM (F == K) price -> 0.5 as sigma -> 0; both -> 0 as sigma grows. hi = 50.0
        // (5000% vol) is far above any tradeable vol, so the bracket is never the binding constraint.
        bisect_sigma(p, forward, strike, t_years, lo, 50.0)
    } else {
        let sigma_star = (2.0 * (-c) / t_years).sqrt();
        let p_max = binary_price(forward, strike, t_years, sigma_star);
        if p > p_max {
            return None;
        }
        bisect_sigma(p, forward, strike, t_years, lo, sigma_star)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // WHY: at-the-money (F == K) with r = 0, d2 = -0.5*sigma*sqrt(T) < 0, so the binary call is
    // worth strictly less than 0.5 — a sign error in d2 (the classic BS digital bug) would push
    // it above 0.5. Pin the direction and a hand value: sigma=0.8, T=0.25 => d2 = -0.5*0.8*0.5 =
    // -0.2, N(-0.2) = 0.42074...
    #[test]
    fn binary_price_atm_below_half() {
        let p = binary_price(100.0, 100.0, 0.25, 0.8);
        assert!(p < 0.5, "ATM binary call must be < 0.5, got {p}");
        assert!((p - 0.420_740_290_560_897).abs() < 1e-9, "got {p}");
    }

    // WHY: degenerate inputs must not panic or return Inf — callers branch on NaN.
    #[test]
    fn binary_price_degenerate_is_nan() {
        assert!(binary_price(100.0, 100.0, 0.0, 0.8).is_nan());
        assert!(binary_price(100.0, 100.0, 0.25, 0.0).is_nan());
        assert!(binary_price(0.0, 100.0, 0.25, 0.8).is_nan());
    }

    // WHY: round-trip is the contract — a vol priced and then recovered must come back. ATM
    // (F == K, monotone branch) pins the happy path.
    #[test]
    fn inversion_roundtrips_atm() {
        let sigma = 0.8;
        let p = binary_price(100.0, 100.0, 0.25, sigma);
        let recovered = implied_vol_from_binary(p, 100.0, 100.0, 0.25).expect("invertible");
        assert!((recovered - sigma).abs() < 1e-4, "got {recovered}");
    }

    // WHY: in-the-money (F > K) is monotone — recovery must be unique and exact.
    #[test]
    fn inversion_roundtrips_itm() {
        let sigma = 0.5;
        let p = binary_price(110.0, 100.0, 0.25, sigma);
        let recovered = implied_vol_from_binary(p, 110.0, 100.0, 0.25).expect("invertible");
        assert!((recovered - sigma).abs() < 1e-4, "got {recovered}");
    }

    // WHY: out-of-the-money digitals have a max price; a target above it has NO real vol. We must
    // return None, not a bogus root — trading on a fabricated IV is the failure we're preventing.
    #[test]
    fn inversion_above_otm_max_returns_none() {
        // F < K (OTM): peak price p_max < 0.5; ask for something above it.
        let sigma_star = (2.0 * (100.0_f64 / 90.0).ln() / 0.25).sqrt();
        let p_max = binary_price(90.0, 100.0, 0.25, sigma_star);
        assert!(implied_vol_from_binary(p_max + 0.05, 90.0, 100.0, 0.25).is_none());
    }

    // WHY: OTM (F < K) is the non-monotone branch — price rises to a peak then falls, so there are
    // two vols for a given price. We must return the LOWER-vol branch (sigma <= sigma*). A round-trip
    // of a vol BELOW sigma* must come back; a regression that bisects the upper branch fails this.
    #[test]
    fn inversion_roundtrips_otm_lower_branch() {
        let forward = 90.0_f64;
        let strike = 100.0_f64;
        let t = 0.25_f64;
        let c = (forward / strike).ln(); // < 0
        let sigma_star = (2.0 * (-c) / t).sqrt();
        let sigma = sigma_star * 0.5; // strictly on the lower-vol branch
        assert!(sigma < sigma_star);
        let p = binary_price(forward, strike, t, sigma);
        let recovered =
            implied_vol_from_binary(p, forward, strike, t).expect("invertible on lower branch");
        assert!(
            (recovered - sigma).abs() < 1e-4,
            "want {sigma}, got {recovered} (upper-branch regression?)"
        );
    }

    // WHY: out-of-range / degenerate prices must yield None, never panic.
    #[test]
    fn inversion_boundary_inputs_return_none() {
        assert!(implied_vol_from_binary(0.0, 100.0, 100.0, 0.25).is_none());
        assert!(implied_vol_from_binary(1.0, 100.0, 100.0, 0.25).is_none());
        assert!(implied_vol_from_binary(0.5, 100.0, 100.0, 0.0).is_none());
    }
}
