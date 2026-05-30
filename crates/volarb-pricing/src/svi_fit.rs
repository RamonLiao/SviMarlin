//! Zeliade quasi-explicit SVI fit (design §4.3). Outer: 2D Nelder-Mead over (m, sigma). Inner:
//! closed-form 3x3 normal-equations solve over (a, c, d) for the linearized total variance
//! `w = a + d*y + c*sqrt(y^2+1)`, `y = (k-m)/sigma`, then feasibility projection onto the no-arb
//! polytope. Back-transform: `b = c/sigma`, `rho = d/c`.

use crate::PricingError;
use volarb_core::{Expiry, MS_PER_YEAR, SVIParams, Smile, Strike, VolPoints};

/// Fit a Gatheral raw-SVI smile to market `(strike, vol)` observations for one expiry.
///
/// `observations` are `(strike, VolPoints)` where `VolPoints` is annualized sigma x 100. Needs
/// >= 3 points and `now_ms < expiry`. Returns the fitted `Smile` (params + the given forward).
pub fn fit_smile(
    forward: f64,
    expiry: Expiry,
    now_ms: u64,
    observations: &[(Strike, VolPoints)],
) -> Result<Smile, PricingError> {
    if observations.len() < 3 {
        return Err(PricingError::TooFewPoints(observations.len()));
    }
    if forward <= 0.0 {
        return Err(PricingError::InvalidInput {
            reason: "forward <= 0",
        });
    }
    let t = (expiry.unix_ms as f64 - now_ms as f64) / MS_PER_YEAR;
    if t <= 0.0 {
        return Err(PricingError::InvalidInput {
            reason: "expiry <= now",
        });
    }

    // Market points -> (k, w) total-variance space.
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(observations.len());
    for (strike, vp) in observations {
        if strike.0 <= 0.0 {
            return Err(PricingError::InvalidInput {
                reason: "strike <= 0",
            });
        }
        let sigma = vp.0 / 100.0;
        if !sigma.is_finite() || sigma < 0.0 {
            return Err(PricingError::InvalidInput {
                reason: "non-finite/negative vol",
            });
        }
        let k = (strike.0 / forward).ln();
        let w = sigma * sigma * t;
        if !k.is_finite() || !w.is_finite() {
            return Err(PricingError::Degenerate {
                reason: "non-finite k or w",
            });
        }
        pts.push((k, w));
    }
    let max_w = pts.iter().map(|p| p.1).fold(0.0_f64, f64::max);

    // Initial (m, sigma) guess from the moneyness spread.
    let ks: Vec<f64> = pts.iter().map(|p| p.0).collect();
    let m0 = ks.iter().sum::<f64>() / ks.len() as f64;
    let var_k = ks.iter().map(|k| (k - m0) * (k - m0)).sum::<f64>() / ks.len() as f64;
    let s0 = var_k.sqrt().max(0.05);

    let objective = |params: [f64; 2]| -> f64 {
        let (m, sigma) = (params[0], params[1].abs().max(1e-6));
        let (_, sse) = inner_solve(&pts, m, sigma, max_w);
        sse
    };

    let best = nelder_mead_2d(objective, [m0, s0], 400);
    let (m, sigma) = (best[0], best[1].abs().max(1e-6));
    let (theta, sse) = inner_solve(&pts, m, sigma, max_w);
    if !sse.is_finite() {
        return Err(PricingError::NonConvergent);
    }
    let (a, d, c) = (theta[0], theta[1], theta[2]);
    let b = c / sigma;
    let rho = if c > 1e-12 {
        (d / c).clamp(-1.0, 1.0)
    } else {
        0.0
    };

    Ok(Smile {
        params: SVIParams {
            a,
            b,
            rho,
            m,
            sigma,
        },
        forward,
    })
}

/// Inner: given (m, sigma), solve constrained linear LS for (a, d, c) minimizing
/// `sum (a + d*y_i + c*z_i - w_i)^2`, then project onto the no-arb polytope. Returns
/// (theta = [a, d, c], residual SSE at the projected theta).
fn inner_solve(pts: &[(f64, f64)], m: f64, sigma: f64, max_w: f64) -> ([f64; 3], f64) {
    // Design matrix rows phi_i = [1, y_i, z_i]; build normal equations M*theta = r.
    let mut mm = [[0.0_f64; 3]; 3];
    let mut r = [0.0_f64; 3];
    for &(k, w) in pts {
        let y = (k - m) / sigma;
        let z = (y * y + 1.0).sqrt();
        let phi = [1.0, y, z];
        for a in 0..3 {
            r[a] += phi[a] * w;
            for b in 0..3 {
                mm[a][b] += phi[a] * phi[b];
            }
        }
    }
    let mut theta = solve3(mm, r).unwrap_or([max_w.max(0.0), 0.0, 0.0]);

    // Project onto: 0 <= a <= max_w ; 0 <= c <= 4*sigma ; |d| <= min(c, 4*sigma - c).
    theta[2] = theta[2].clamp(0.0, 4.0 * sigma); // c
    let d_bound = theta[2].min(4.0 * sigma - theta[2]).max(0.0);
    theta[1] = theta[1].clamp(-d_bound, d_bound); // d
    theta[0] = theta[0].clamp(0.0, max_w.max(0.0)); // a

    // Residual SSE at projected theta.
    let mut sse = 0.0;
    for &(k, w) in pts {
        let y = (k - m) / sigma;
        let z = (y * y + 1.0).sqrt();
        let model = theta[0] + theta[1] * y + theta[2] * z;
        sse += (model - w) * (model - w);
    }
    (theta, sse)
}

/// Solve a 3x3 linear system via Gaussian elimination with partial pivoting. None if singular.
fn solve3(mut a: [[f64; 3]; 3], mut b: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        // pivot
        let mut piv = col;
        for r in (col + 1)..3 {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-15 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        #[allow(clippy::needless_range_loop)]
        for r in (col + 1)..3 {
            let f = a[r][col] / a[col][col];
            for c in col..3 {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0.0; 3];
    for i in (0..3).rev() {
        let mut s = b[i];
        for j in (i + 1)..3 {
            s -= a[i][j] * x[j];
        }
        x[i] = s / a[i][i];
    }
    Some(x)
}

/// Minimal 2D Nelder-Mead simplex minimizer. Fixed iteration budget; returns best vertex.
fn nelder_mead_2d<F: Fn([f64; 2]) -> f64>(f: F, start: [f64; 2], iters: usize) -> [f64; 2] {
    let mut simplex = [
        start,
        [start[0] + 0.1, start[1]],
        [start[0], start[1] + 0.1],
    ];
    let mut fv = [f(simplex[0]), f(simplex[1]), f(simplex[2])];
    for _ in 0..iters {
        // order: best=0 .. worst=2
        let mut idx = [0, 1, 2];
        idx.sort_by(|&i, &j| {
            fv[i]
                .partial_cmp(&fv[j])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let (b, g, w) = (idx[0], idx[1], idx[2]);
        let centroid = [
            (simplex[b][0] + simplex[g][0]) / 2.0,
            (simplex[b][1] + simplex[g][1]) / 2.0,
        ];
        let reflect = [
            centroid[0] + (centroid[0] - simplex[w][0]),
            centroid[1] + (centroid[1] - simplex[w][1]),
        ];
        let fr = f(reflect);
        if fr < fv[b] {
            let expand = [
                centroid[0] + 2.0 * (centroid[0] - simplex[w][0]),
                centroid[1] + 2.0 * (centroid[1] - simplex[w][1]),
            ];
            let fe = f(expand);
            if fe < fr {
                simplex[w] = expand;
                fv[w] = fe;
            } else {
                simplex[w] = reflect;
                fv[w] = fr;
            }
        } else if fr < fv[g] {
            simplex[w] = reflect;
            fv[w] = fr;
        } else {
            let contract = [
                centroid[0] + 0.5 * (simplex[w][0] - centroid[0]),
                centroid[1] + 0.5 * (simplex[w][1] - centroid[1]),
            ];
            let fc = f(contract);
            if fc < fv[w] {
                simplex[w] = contract;
                fv[w] = fc;
            } else {
                // shrink toward best
                for &v in &[g, w] {
                    simplex[v] = [
                        (simplex[v][0] + simplex[b][0]) / 2.0,
                        (simplex[v][1] + simplex[b][1]) / 2.0,
                    ];
                    fv[v] = f(simplex[v]);
                }
            }
        }
    }
    let best = (0..3)
        .min_by(|&i, &j| fv[i].partial_cmp(&fv[j]).unwrap())
        .unwrap();
    simplex[best]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use volarb_core::SVISurface;

    const FWD: f64 = 64_000.0;

    fn truth() -> SVIParams {
        SVIParams {
            a: 0.04,
            b: 0.4,
            rho: -0.3,
            m: 0.0,
            sigma: 0.1,
        }
    }

    // Generate (strike, VolPoints) observations from a known smile at the given strikes.
    fn synth(
        params: &SVIParams,
        t: f64,
        strikes: &[f64],
        noise: &[f64],
    ) -> Vec<(Strike, VolPoints)> {
        strikes
            .iter()
            .zip(noise.iter().chain(std::iter::repeat(&0.0)))
            .map(|(&strike, &n)| {
                let k = (strike / FWD).ln();
                let dd = k - params.m;
                let w = params.a
                    + params.b * (params.rho * dd + (dd * dd + params.sigma * params.sigma).sqrt());
                let sigma = (w / t).sqrt();
                (Strike(strike), VolPoints(sigma * 100.0 + n))
            })
            .collect()
    }

    fn eval(params: &SVIParams, strike: f64, t: f64) -> f64 {
        let mut s = SVISurface {
            as_of_ms: 0,
            per_expiry: BTreeMap::new(),
        };
        let expiry = (MS_PER_YEAR * t) as u64;
        s.per_expiry.insert(
            expiry,
            Smile {
                params: *params,
                forward: FWD,
            },
        );
        s.sigma_at(Strike(strike), Expiry { unix_ms: expiry }, 0)
            .unwrap()
            .0
    }

    // WHY (gold test): a fitter that cannot invert its own forward model is wrong regardless of
    // how it behaves on real data. We compare the *smile* (sigma_at), not raw params, because raw
    // SVI params are not unique — two param sets can describe the same smile.
    #[test]
    fn fit_recovers_known_smile_dense() {
        let t = 0.25;
        let expiry = Expiry {
            unix_ms: (MS_PER_YEAR * t) as u64,
        };
        let strikes = [
            56_000.0, 58_000.0, 60_000.0, 62_000.0, 64_000.0, 66_000.0, 68_000.0, 70_000.0,
            72_000.0,
        ];
        let obs = synth(&truth(), t, &strikes, &[]);
        let fitted = fit_smile(FWD, expiry, 0, &obs).expect("fit");
        for &strike in &strikes {
            let want = eval(&truth(), strike, t);
            let got = eval(&fitted.params, strike, t);
            assert!(
                (want - got).abs() < 0.5,
                "strike {strike}: want {want} got {got}"
            );
        }
    }

    // WHY: ~5 strikes per sub-hour binary expiry is the real HL regime. Zeliade exists precisely
    // so a sparse fit stays well-posed; a 5-point fit must still track the smile.
    #[test]
    fn fit_sparse_five_points() {
        let t = 0.05;
        let expiry = Expiry {
            unix_ms: (MS_PER_YEAR * t) as u64,
        };
        let strikes = [60_000.0, 62_000.0, 64_000.0, 66_000.0, 68_000.0];
        let obs = synth(&truth(), t, &strikes, &[]);
        let fitted = fit_smile(FWD, expiry, 0, &obs).expect("fit");
        for &strike in &strikes {
            let want = eval(&truth(), strike, t);
            let got = eval(&fitted.params, strike, t);
            assert!(
                (want - got).abs() < 1.0,
                "strike {strike}: want {want} got {got}"
            );
        }
    }

    // WHY: the projection step must keep the smile inside the no-arb domain even under noise.
    // We assert |rho| < 1 and b >= 0 (Gatheral no-arb necessary conditions).
    #[test]
    fn fit_noisy_stays_no_arb() {
        let t = 0.25;
        let expiry = Expiry {
            unix_ms: (MS_PER_YEAR * t) as u64,
        };
        let strikes = [
            58_000.0, 60_000.0, 62_000.0, 64_000.0, 66_000.0, 68_000.0, 70_000.0,
        ];
        let noise = [0.8, -0.6, 0.5, -0.7, 0.4, -0.5, 0.6];
        let obs = synth(&truth(), t, &strikes, &noise);
        let fitted = fit_smile(FWD, expiry, 0, &obs).expect("fit");
        assert!(fitted.params.b >= 0.0, "b = {}", fitted.params.b);
        assert!(fitted.params.rho.abs() < 1.0, "rho = {}", fitted.params.rho);
    }

    // WHY: too-few / degenerate inputs must be typed errors, not panics or garbage smiles.
    #[test]
    fn fit_rejects_bad_input() {
        let expiry = Expiry { unix_ms: 1_000_000 };
        assert_eq!(
            fit_smile(FWD, expiry, 0, &[(Strike(60_000.0), VolPoints(50.0))]).unwrap_err(),
            PricingError::TooFewPoints(1)
        );
        let obs = synth(&truth(), 0.25, &[60_000.0, 64_000.0, 68_000.0], &[]);
        // expiry <= now
        assert!(matches!(
            fit_smile(FWD, Expiry { unix_ms: 0 }, 1_000, &obs).unwrap_err(),
            PricingError::InvalidInput { .. }
        ));
    }
}
