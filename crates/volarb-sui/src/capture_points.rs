//! Deterministic sweep point generation for the L3 parity capture
//! (spec `docs/specs/2026-06-11-l0-parity-basis-harness-design.md` §3.1).
//! NO wall-clock / rand crates — fixed-seed LCG so capture is reproducible (meta.seed).

pub const SCALE: u64 = 1_000_000_000;
/// normal_cdf regime A/B break (findings doc + Part 1 port).
const B_BREAK: u64 = 5_656_854_249;
/// ln(2) in 1e9-FP.
const LN2: u64 = 693_147_180;

/// Minimal LCG (Knuth MMIX constants).
pub struct Lcg(pub u64);
impl Lcg {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    pub fn in_range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }
    pub fn flag(&mut self) -> bool {
        self.next_u64().is_multiple_of(2)
    }
}

#[derive(Debug, Clone)]
pub struct Case {
    pub func: &'static str,
    pub args: Vec<u64>,
    pub neg_flags: Vec<bool>,
}

impl Case {
    fn new(func: &'static str, args: Vec<u64>, neg_flags: Vec<bool>) -> Self {
        Case {
            func,
            args,
            neg_flags,
        }
    }
}

/// Boundary + pseudo-random sweep per function. Abort boundaries deliberately included
/// (ln(0), sqrt(b=0 / b>SCALE), exp overflow, mul cast-back overflow, div-by-zero).
pub fn math_sweep(seed: u64) -> Vec<Case> {
    let mut r = Lcg(seed);
    let mut v = Vec::new();
    // ln: abort(0) + boundaries + random
    for x in [0u64, 1, SCALE - 1, SCALE, SCALE + 1, u64::MAX / SCALE] {
        v.push(Case::new("ln", vec![x], vec![]));
    }
    for _ in 0..20 {
        v.push(Case::new("ln", vec![r.in_range(1, 100 * SCALE)], vec![]));
    }
    // exp: ±boundaries incl. saturate / overflow edges
    for (m, n) in [
        (0u64, false),
        (LN2, false),
        (LN2, true),
        (SCALE, false),
        (SCALE, true),
        (30 * SCALE, false),
        (30 * SCALE, true),
        (50 * SCALE, false),
        (50 * SCALE, true),
        (200 * SCALE, true),
    ] {
        v.push(Case::new("exp", vec![m], vec![n]));
    }
    for _ in 0..20 {
        v.push(Case::new(
            "exp",
            vec![r.in_range(0, 40 * SCALE)],
            vec![r.flag()],
        ));
    }
    // sqrt(a, b): perfect squares, domain aborts, random
    for (a, b) in [
        (4 * SCALE, SCALE),
        (0, SCALE),
        (SCALE, SCALE),
        (u64::MAX / SCALE, SCALE),
        (SCALE, 0),
        (SCALE, SCALE + 1),
        (SCALE, SCALE / 2),
    ] {
        v.push(Case::new("sqrt", vec![a, b], vec![]));
    }
    for _ in 0..20 {
        v.push(Case::new(
            "sqrt",
            vec![r.in_range(0, 1u64 << 50), SCALE],
            vec![],
        ));
    }
    // normal_cdf: ±boundaries around regime break + random
    for m in [
        0,
        1,
        SCALE / 2,
        SCALE,
        2 * SCALE,
        4 * SCALE,
        B_BREAK - 1,
        B_BREAK,
        B_BREAK + 1,
        8 * SCALE,
    ] {
        for n in [false, true] {
            v.push(Case::new("normal_cdf", vec![m], vec![n]));
        }
    }
    for _ in 0..30 {
        v.push(Case::new(
            "normal_cdf",
            vec![r.in_range(0, 7 * SCALE)],
            vec![r.flag()],
        ));
    }
    // i64 scaled ops: sign boundaries, cast-back overflow abort, div-by-zero abort, random.
    // db_mul/db_div fixtures go through i64::mul_scaled/div_scaled with positive operands —
    // on chain those wrap the same DeepBook math::mul/div the port's db_mul/db_div mirror.
    for (am, an, bm, bn) in [
        (0, false, SCALE, false),
        (SCALE, true, SCALE, true),
        (u64::MAX, false, u64::MAX, false), // mul: cast-back overflow abort
        (SCALE, false, 0, false),           // div: DivByZero abort
    ] {
        v.push(Case::new("mul_scaled", vec![am, bm], vec![an, bn]));
        v.push(Case::new("div_scaled", vec![am, bm], vec![an, bn]));
    }
    for _ in 0..15 {
        let (am, bm) = (r.in_range(0, 1u64 << 40), r.in_range(1, 1u64 << 40));
        let (an, bn) = (r.flag(), r.flag());
        v.push(Case::new("mul_scaled", vec![am, bm], vec![an, bn]));
        v.push(Case::new("div_scaled", vec![am, bm], vec![an, bn]));
        v.push(Case::new("square_scaled", vec![am], vec![an]));
        v.push(Case::new("db_mul", vec![am, bm], vec![]));
        v.push(Case::new("db_div", vec![am, bm], vec![]));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_is_deterministic_and_covers_all_funcs() {
        let a = math_sweep(42);
        let b = math_sweep(42);
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter()
                .zip(&b)
                .all(|(x, y)| x.args == y.args && x.neg_flags == y.neg_flags)
        );
        for f in [
            "ln",
            "exp",
            "sqrt",
            "normal_cdf",
            "mul_scaled",
            "div_scaled",
            "square_scaled",
            "db_mul",
            "db_div",
        ] {
            assert!(a.iter().any(|c| c.func == f), "missing {f}");
        }
        assert!(a.len() >= 150, "sweep too small: {}", a.len());
    }
}
