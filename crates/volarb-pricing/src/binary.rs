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
}
