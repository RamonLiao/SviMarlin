//! L3 parity harness — replays frozen devInspect fixtures against the Rust L0 port.
//! Zero tolerance: assert_eq! on u64. Fixtures are ground truth; NEVER edit them to pass.

#[path = "fixture_schema.rs"]
mod schema;
use schema::{E2eCase, FixtureFile, MathCase, RetVal};
use volarb_pricing::onchain::{
    I64, OnchainError, OnchainOracle, db_div, db_mul, exp, ln, normal_cdf, sqrt,
};

/// Exact expected error for a math-function abort. Chain abort codes 0 and 1 are reused by
/// different functions (0 = ln undefined OR i64 magnitude overflow; 1 = exp overflow OR div by
/// zero), so we disambiguate by `func` — a code-only match would let e.g. `ln(0)` pass while the
/// port wrongly returned `MagnitudeOverflow` (codex review 2026-06-13).
fn expected_math_err(func: &str, code: u64) -> OnchainError {
    use OnchainError::*;
    match (func, code) {
        ("ln", 0) => LnZero,
        ("exp", 1) => ExpOverflow,
        ("sqrt", 2) => SqrtDomain,
        ("mul_scaled" | "square_scaled" | "db_mul", 0) => MagnitudeOverflow,
        ("div_scaled" | "db_div", 1) => DivByZero,
        // div also overflows on checked cast-back of (a*1e9/b) -> code 0 (onchain.rs db_div/div_scaled).
        ("div_scaled" | "db_div", 0) => MagnitudeOverflow,
        _ => panic!("unmapped math abort: func={func} code={code}"),
    }
}

/// Exact expected error for a `compute_price`/`compute_nd2` abort. Each code maps 1:1 in this path:
/// 3 = forward not positive, 4 = bracket negative, 5 = w not positive, 0 = ln(F/K) undefined,
/// 2 = sqrt domain. (Code 0 here is only reachable via `ln`, never the i64 magnitude path.)
fn expected_e2e_err(code: u64) -> OnchainError {
    use OnchainError::*;
    match code {
        0 => LnZero,
        2 => SqrtDomain,
        3 => ForwardNonPositive,
        4 => BracketNegative,
        5 => WNonPositive,
        _ => panic!("unmapped e2e abort code {code}"),
    }
}

fn run_math(c: &MathCase) {
    let i64arg = |i: usize| I64::from_parts(c.args[i], *c.neg_flags.get(i).unwrap_or(&false));
    // I64 returns flatten to (mag, neg); u64 returns to (val, false).
    let got: Result<(u64, bool), OnchainError> = match c.func.as_str() {
        "ln" => ln(c.args[0]).map(|v| (v.magnitude(), v.is_negative())),
        "exp" => exp(&i64arg(0)).map(|v| (v, false)),
        "sqrt" => sqrt(c.args[0], c.args[1]).map(|v| (v, false)),
        "normal_cdf" => normal_cdf(&i64arg(0)).map(|v| (v, false)),
        "mul_scaled" => i64arg(0)
            .mul_scaled(&i64arg(1))
            .map(|v| (v.magnitude(), v.is_negative())),
        "div_scaled" => i64arg(0)
            .div_scaled(&i64arg(1))
            .map(|v| (v.magnitude(), v.is_negative())),
        "square_scaled" => i64arg(0).square_scaled().map(|v| (v, false)),
        "db_mul" => db_mul(c.args[0], c.args[1]).map(|v| (v, false)),
        "db_div" => db_div(c.args[0], c.args[1]).map(|v| (v, false)),
        other => panic!("unknown fixture func {other}"),
    };
    match (&c.ret, c.expect_abort, got) {
        (Some(RetVal::U64(want)), None, Ok((g, neg))) => {
            assert!(
                !neg,
                "{}: chain returned u64 but port returned negative I64, args={:?}",
                c.func, c.args
            );
            assert_eq!(
                g, *want,
                "{} args={:?} negs={:?}: port={g} ({g:#x}) chain={want} ({want:#x})",
                c.func, c.args, c.neg_flags
            );
        }
        (
            Some(RetVal::I64 {
                magnitude,
                is_negative,
            }),
            None,
            Ok((g, neg)),
        ) => {
            assert_eq!(
                (g, neg),
                (*magnitude, *is_negative),
                "{} args={:?}: port=({g},{neg}) chain=({magnitude},{is_negative})",
                c.func,
                c.args
            );
        }
        (None, Some(code), Err(e)) => {
            assert_eq!(
                e,
                expected_math_err(&c.func, code),
                "{} args={:?}: chain abort {code} expects {:?} but port err {e:?}",
                c.func,
                c.args,
                expected_math_err(&c.func, code)
            );
        }
        (ret, ab, got) => panic!(
            "{} args={:?}: fixture(ret={ret:?},abort={ab:?}) vs port {got:?}",
            c.func, c.args
        ),
    }
}

fn run_e2e(c: &E2eCase) {
    let oracle = OnchainOracle {
        forward: c.forward,
        a: c.a,
        b: c.b,
        sigma: c.sigma,
        rho: I64::from_parts(c.rho_mag, c.rho_neg),
        m: I64::from_parts(c.m_mag, c.m_neg),
        settlement: c.settlement,
    };
    match (c.ret, c.expect_abort, oracle.compute_price(c.strike)) {
        (Some(want), None, Ok(got)) => assert_eq!(
            got, want,
            "oracle {} strike {}: port={got} ({got:#x}) chain={want} ({want:#x})",
            c.oracle_id, c.strike
        ),
        (None, Some(code), Err(e)) => assert_eq!(
            e,
            expected_e2e_err(code),
            "oracle {} strike {}: chain abort {code} expects {:?}, port err {e:?}",
            c.oracle_id,
            c.strike,
            expected_e2e_err(code)
        ),
        (r, a, g) => panic!(
            "oracle {} strike {}: fixture(ret={r:?},abort={a:?}) vs port {g:?}",
            c.oracle_id, c.strike
        ),
    }
}

fn run_file(path: &std::path::Path) -> (usize, usize) {
    let txt = std::fs::read_to_string(path).unwrap();
    let f: FixtureFile = serde_json::from_str(&txt)
        .unwrap_or_else(|e| panic!("bad fixture {}: {e}", path.display()));
    f.math.iter().for_each(run_math);
    f.e2e.iter().for_each(run_e2e);
    (f.math.len(), f.e2e.len())
}

fn fixture_paths(want_selftest: bool) -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            let is_self = p
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("_selftest");
            if want_selftest { is_self } else { !is_self }
        })
        .collect();
    v.sort();
    v
}

/// Harness mechanics self-test: fixture values produced by the Rust port itself (**not** chain
/// ground truth) — verifies only the runner logic.
#[test]
fn selftest_harness_mechanics() {
    let paths = fixture_paths(true);
    assert!(!paths.is_empty(), "_selftest fixture missing");
    for p in paths {
        run_file(&p);
    }
}

/// True parity: chain-captured frozen fixtures. Missing fixtures = FAIL LOUD (spec §4).
#[test]
fn chain_parity_bit_exact() {
    let paths = fixture_paths(false);
    assert!(
        !paths.is_empty(),
        "NO chain fixtures in tests/fixtures/ — run volarb-sui capture_fixtures (Task 6). \
         This test must not be green before capture."
    );
    let (mut nm, mut ne) = (0, 0);
    for p in &paths {
        let (m, e) = run_file(p);
        nm += m;
        ne += e;
    }
    println!(
        "parity OK: {nm} math cases + {ne} e2e cases across {} files",
        paths.len()
    );
}
