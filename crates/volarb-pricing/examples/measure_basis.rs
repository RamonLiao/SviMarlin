//! L0-vs-L1 basis measurement (one-shot report, NOT CI; spec §5).
//!
//! L0 = chain-exact integer price (the e2e fixture `ret`, proven bit-exact by the parity harness).
//! L1 = float Black-Scholes cash-or-nothing digital off the SAME raw total variance `w`.
//! The basis is therefore the chain's fixed-point truncation error vs clean float — the number
//! the router must keep as edge buffer so a float signal never crosses a chain price it can't hit.
//!
//! Run: cargo run -p volarb-pricing --example measure_basis

use serde::Deserialize;
use volarb_pricing::binary::binary_price;
use volarb_pricing::onchain::SCALE;

#[derive(Deserialize)]
struct FixtureFile {
    meta: Meta,
    e2e: Vec<E2eCase>,
}

#[derive(Deserialize)]
struct Meta {
    captured_at_ms: u64,
}

#[derive(Deserialize)]
struct E2eCase {
    forward: u64,
    a: u64,
    b: u64,
    sigma: u64,
    rho_mag: u64,
    rho_neg: bool,
    m_mag: u64,
    m_neg: bool,
    settlement: Option<u64>,
    expiry_ms: u64,
    strike: u64,
    ret: Option<u64>,
    expect_abort: Option<u64>,
}

fn signed(mag: u64, neg: bool) -> f64 {
    let v = mag as f64 / SCALE as f64;
    if neg { -v } else { v }
}

fn main() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/e2e_oracles.json"
    );
    let txt = std::fs::read_to_string(path).expect("read e2e fixture");
    let f: FixtureFile = serde_json::from_str(&txt).expect("parse e2e fixture");

    let mut ticks: Vec<f64> = Vec::new();
    let (mut skipped_settled, mut skipped_abort, mut skipped_t) = (0, 0, 0);

    for c in &f.e2e {
        if c.settlement.is_some() {
            skipped_settled += 1;
            continue;
        }
        if c.expect_abort.is_some() || c.ret.is_none() {
            skipped_abort += 1;
            continue;
        }
        // L0: chain integer price, in [0,1].
        let l0 = c.ret.unwrap() as f64 / SCALE as f64;

        // Reconstruct raw total variance w(k) in float (same SVI form compute_nd2 uses).
        let fwd = c.forward as f64 / SCALE as f64;
        let strike = c.strike as f64 / SCALE as f64;
        let k = (strike / fwd).ln();
        let (a, b, sig) = (
            c.a as f64 / SCALE as f64,
            c.b as f64 / SCALE as f64,
            c.sigma as f64 / SCALE as f64,
        );
        let (rho, m) = (signed(c.rho_mag, c.rho_neg), signed(c.m_mag, c.m_neg));
        let w = a + b * (rho * (k - m) + ((k - m).powi(2) + sig * sig).sqrt());

        // T from expiry - capture; convert raw w to annualized sigma so L1's sigma^2*T == w.
        let t_years = (c.expiry_ms.saturating_sub(f.meta.captured_at_ms)) as f64 / MS_PER_YEAR;
        if w <= 0.0 || t_years <= 0.0 {
            skipped_t += 1;
            continue;
        }
        let sigma_ann = (w / t_years).sqrt();
        let l1 = binary_price(fwd, strike, t_years, sigma_ann);

        ticks.push((l1 - l0).abs() * SCALE as f64);
    }

    ticks.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let n = ticks.len();

    println!(
        "=== L0-vs-L1 basis (n={n} priced; skipped {skipped_settled} settled, {skipped_abort} abort, {skipped_t} degenerate) ==="
    );
    if n == 0 {
        println!("no priceable cases");
        return;
    }
    let max = ticks[n - 1];
    let mean = ticks.iter().sum::<f64>() / n as f64;
    let p50 = ticks[n / 2];
    let p99 = ticks[(n * 99 / 100).min(n - 1)];
    println!(
        "ticks (1 tick = 1e-9 of price): max={max:.1} mean={mean:.1} p50={p50:.1} p99={p99:.1}"
    );
    println!(
        "as price fraction: max={:.2e} mean={:.2e}",
        max / SCALE as f64,
        mean / SCALE as f64
    );
}

const MS_PER_YEAR: f64 = 365.0 * 24.0 * 3600.0 * 1000.0;
