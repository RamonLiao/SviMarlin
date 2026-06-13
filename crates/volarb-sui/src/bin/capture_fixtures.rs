//! One-shot fixture capture for the L3 parity harness
//! (spec `docs/specs/2026-06-11-l0-parity-basis-harness-design.md`).
//!
//! Channel: JSON-RPC `sui_devInspectTransactionBlock` — verified in the Task 3 spike to return
//! per-command BCS return values (F2). I64 BCS layout verified on chain: (magnitude u64 LE,
//! is_negative bool). Abort codes parsed from `effects.status.error` ("MoveAbort(.., N)").
//!
//! Flow: preflight (sqrt anchor) -> math sweep (batched PTBs, abort fallback to individual)
//! -> write `crates/volarb-pricing/tests/fixtures/math_sweep.json`.

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Serialize;
use std::str::FromStr;
use sui_sdk_types::{
    Address, Argument, Command, Identifier, Input, MoveCall, ProgrammableTransaction,
    TransactionKind,
};
use volarb_sui::capture_points::{Case, math_sweep};

const PKG: &str = "0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138";
/// DeepBook math package — `compute_nd2` calls `math::mul/div` (round-down) from HERE, not the
/// package's own `math`. db_mul/db_div fixtures target this so the port is validated against the
/// REAL DeepBook math, not a proxy (oracle.mv `use fb28…6982::math`).
const DEEPBOOK_PKG: &str = "0xfb28c4cbc6865bd1c897d26aecbe1f8792d1509a20ffec692c800660cbec6982";
const RPC: &str = "https://fullnode.testnet.sui.io:443";
const SENDER: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const SEED: u64 = 42;
const BATCH: usize = 40;
const SCALE: u64 = 1_000_000_000;

/// Live BTC sub-hour oracles discovered via `oracle::OracleSVIUpdated` events (Task 5, F1).
/// Frozen ids: the package is Immutable, but oracle objects are mutable shared state — the
/// e2e fixture snapshots their full field set at capture time, so the fixture stays valid
/// even after these oracles settle/expire.
const ORACLE_IDS: &[&str] = &[
    "0x137099db3d7ba7edcc2df967648f1698f6362e652209313ff546214df236520d",
    "0x10bf167846258fe50b811cd8f88a5ff3423ceaf7aca38444d92c0912f52bb696",
];

// ---------- fixture output schema (Serialize side; the JSON format IS the contract with
// volarb-pricing/tests/fixture_schema.rs) ----------

#[derive(Serialize)]
struct FixtureFile {
    meta: Meta,
    math: Vec<MathCaseOut>,
    e2e: Vec<E2eCaseOut>,
}

#[derive(Serialize)]
struct Meta {
    chain: String,
    package: String,
    protocol_version: u64,
    captured_at_ms: u64,
    channel: String,
    seed: u64,
}

#[derive(Serialize)]
struct MathCaseOut {
    func: String,
    args: Vec<u64>,
    neg_flags: Vec<bool>,
    ret: Option<serde_json::Value>,
    expect_abort: Option<u64>,
}

#[derive(Serialize)]
struct E2eCaseOut {
    oracle_id: String,
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

// ---------- RPC plumbing ----------

fn rpc_call(method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    let resp: serde_json::Value = reqwest::blocking::Client::new()
        .post(RPC)
        .json(&body)
        .send()?
        .json()?;
    if let Some(err) = resp.get("error") {
        bail!("rpc error from {method}: {err}");
    }
    Ok(resp["result"].clone())
}

fn dev_inspect(tx_kind_b64: &str) -> Result<serde_json::Value> {
    rpc_call(
        "sui_devInspectTransactionBlock",
        serde_json::json!([SENDER, tx_kind_b64, null, null]),
    )
}

fn tx_kind_b64(inputs: Vec<Input>, commands: Vec<Command>) -> Result<String> {
    let kind =
        TransactionKind::ProgrammableTransaction(ProgrammableTransaction { inputs, commands });
    Ok(base64::engine::general_purpose::STANDARD.encode(bcs::to_bytes(&kind)?))
}

fn move_call_at(
    pkg: &str,
    module: &str,
    function: &str,
    arguments: Vec<Argument>,
) -> Result<Command> {
    Ok(Command::MoveCall(MoveCall {
        package: Address::from_str(pkg)?,
        module: Identifier::new(module)?,
        function: Identifier::new(function)?,
        type_arguments: vec![],
        arguments,
    }))
}

fn move_call(module: &str, function: &str, arguments: Vec<Argument>) -> Result<Command> {
    move_call_at(PKG, module, function, arguments)
}

fn return_bytes(result: &serde_json::Value, cmd: usize) -> Result<Vec<u8>> {
    let rv = result["results"][cmd]["returnValues"][0][0]
        .as_array()
        .with_context(|| format!("no returnValues for command {cmd} (F2)"))?;
    Ok(rv.iter().map(|v| v.as_u64().unwrap() as u8).collect())
}

fn is_success(result: &serde_json::Value) -> bool {
    result["effects"]["status"]["status"] == "success"
}

/// Parse `MoveAbort(..., CODE) in command N` from effects.status.error.
fn abort_code(result: &serde_json::Value) -> Result<u64> {
    let err = result["effects"]["status"]["error"]
        .as_str()
        .context("failure without error string")?;
    // err looks like: "MoveAbort(MoveLocation { .. }, 0) in command 0"
    let tail = err
        .split("MoveAbort(")
        .nth(1)
        .and_then(|s| s.split(") in command").next())
        .context("no MoveAbort in error")?;
    let code_str = tail
        .rsplit_once(", ")
        .map(|(_, c)| c)
        .context("malformed MoveAbort")?;
    Ok(code_str.trim().parse()?)
}

// ---------- math sweep capture ----------

enum RetKind {
    U64,
    I64,
}

/// Append the PTB commands for one case; returns (target command index, return kind).
fn push_case(
    case: &Case,
    inputs: &mut Vec<Input>,
    commands: &mut Vec<Command>,
) -> Result<(usize, RetKind)> {
    // i64::from_parts(mag, neg) -> Argument::Result
    let i64_arg =
        |i: usize, inputs: &mut Vec<Input>, commands: &mut Vec<Command>| -> Result<Argument> {
            inputs.push(Input::Pure(bcs::to_bytes(&case.args[i])?));
            let a_mag = Argument::Input((inputs.len() - 1) as u16);
            inputs.push(Input::Pure(bcs::to_bytes(&case.neg_flags[i])?));
            let a_neg = Argument::Input((inputs.len() - 1) as u16);
            commands.push(move_call("i64", "from_parts", vec![a_mag, a_neg])?);
            Ok(Argument::Result((commands.len() - 1) as u16))
        };
    let (module, function, args, ret) = match case.func {
        "ln" => {
            inputs.push(Input::Pure(bcs::to_bytes(&case.args[0])?));
            (
                "math",
                "ln",
                vec![Argument::Input((inputs.len() - 1) as u16)],
                RetKind::I64,
            )
        }
        "exp" => {
            let a = i64_arg(0, inputs, commands)?;
            ("math", "exp", vec![a], RetKind::U64)
        }
        "sqrt" => {
            inputs.push(Input::Pure(bcs::to_bytes(&case.args[0])?));
            let a0 = Argument::Input((inputs.len() - 1) as u16);
            inputs.push(Input::Pure(bcs::to_bytes(&case.args[1])?));
            let a1 = Argument::Input((inputs.len() - 1) as u16);
            ("math", "sqrt", vec![a0, a1], RetKind::U64)
        }
        "normal_cdf" => {
            let a = i64_arg(0, inputs, commands)?;
            ("math", "normal_cdf", vec![a], RetKind::U64)
        }
        "mul_scaled" | "div_scaled" => {
            let a = i64_arg(0, inputs, commands)?;
            let b = i64_arg(1, inputs, commands)?;
            ("i64", case.func, vec![a, b], RetKind::I64)
        }
        "square_scaled" => {
            let a = i64_arg(0, inputs, commands)?;
            ("i64", "square_scaled", vec![a], RetKind::U64)
        }
        // db_mul/db_div call the REAL DeepBook math::mul/div (round-down) — the exact functions
        // compute_nd2 invokes. (u64, u64) -> u64, no i64 wrapping.
        "db_mul" | "db_div" => {
            let f = if case.func == "db_mul" { "mul" } else { "div" };
            inputs.push(Input::Pure(bcs::to_bytes(&case.args[0])?));
            let a0 = Argument::Input((inputs.len() - 1) as u16);
            inputs.push(Input::Pure(bcs::to_bytes(&case.args[1])?));
            let a1 = Argument::Input((inputs.len() - 1) as u16);
            commands.push(move_call_at(DEEPBOOK_PKG, "math", f, vec![a0, a1])?);
            return Ok((commands.len() - 1, RetKind::U64));
        }
        other => bail!("unknown case func {other}"),
    };
    commands.push(move_call(module, function, args)?);
    Ok((commands.len() - 1, ret))
}

/// Decode a target-command return value into the fixture `ret` JSON.
fn decode_ret(_case: &Case, kind: &RetKind, bytes: &[u8]) -> Result<serde_json::Value> {
    Ok(match kind {
        RetKind::U64 => serde_json::json!(bcs::from_bytes::<u64>(bytes)?),
        RetKind::I64 => {
            let (mag, neg): (u64, bool) = bcs::from_bytes(bytes)?;
            serde_json::json!({"magnitude": mag, "is_negative": neg})
        }
    })
}

/// Run one case in its own PTB; returns (ret, expect_abort).
fn run_individual(case: &Case) -> Result<(Option<serde_json::Value>, Option<u64>)> {
    let (mut inputs, mut commands) = (Vec::new(), Vec::new());
    let (target, kind) = push_case(case, &mut inputs, &mut commands)?;
    let result = dev_inspect(&tx_kind_b64(inputs, commands)?)?;
    if is_success(&result) {
        let v = decode_ret(case, &kind, &return_bytes(&result, target)?)?;
        Ok((Some(v), None))
    } else {
        Ok((None, Some(abort_code(&result)?)))
    }
}

fn capture_math(cases: &[Case]) -> Result<Vec<MathCaseOut>> {
    let mut out = Vec::with_capacity(cases.len());
    for (ci, chunk) in cases.chunks(BATCH).enumerate() {
        let (mut inputs, mut commands) = (Vec::new(), Vec::new());
        let mut targets = Vec::new();
        for case in chunk {
            targets.push(push_case(case, &mut inputs, &mut commands)?);
        }
        let result = dev_inspect(&tx_kind_b64(inputs, commands)?)?;
        let batch_ok = is_success(&result);
        for (case, (target, kind)) in chunk.iter().zip(&targets) {
            let (ret, expect_abort) = if batch_ok {
                (
                    Some(decode_ret(case, kind, &return_bytes(&result, *target)?)?),
                    None,
                )
            } else {
                // a single abort poisons the whole PTB's results — replay individually (plan T4)
                run_individual(case)?
            };
            out.push(MathCaseOut {
                func: case.func.to_string(),
                args: case.args.clone(),
                neg_flags: case.neg_flags.clone(),
                ret,
                expect_abort,
            });
        }
        eprintln!(
            "batch {}/{}: {} cases ({})",
            ci + 1,
            cases.len().div_ceil(BATCH),
            chunk.len(),
            if batch_ok {
                "batched"
            } else {
                "individual replay"
            }
        );
    }
    Ok(out)
}

// ---------- meta ----------

fn capture_meta(channel: &str) -> Result<Meta> {
    let proto = rpc_call("sui_getProtocolConfig", serde_json::json!([]))?;
    let protocol_version = proto["protocolVersion"]
        .as_str()
        .context("protocolVersion")?
        .parse()?;
    let seq = rpc_call(
        "sui_getLatestCheckpointSequenceNumber",
        serde_json::json!([]),
    )?;
    let cp = rpc_call("sui_getCheckpoint", serde_json::json!([seq.as_str()]))?;
    let captured_at_ms = cp["timestampMs"].as_str().context("timestampMs")?.parse()?;
    Ok(Meta {
        chain: "testnet".into(),
        package: PKG.into(),
        protocol_version,
        captured_at_ms,
        channel: channel.into(),
        seed: SEED,
    })
}

// ---------- e2e: compute_nd2 reconstruction (Task 5, F1) ----------
//
// `oracle::compute_price`/`compute_nd2` are `public(friend)` — NOT callable via devInspect, and
// no public wrapper returns the raw price (get_trade_amounts applies spread+qty). So we MECHANICALLY
// TRANSCRIBE compute_nd2's bytecode (oracle.mv, re-disassembled Task 5) into chained PTBs: every
// Move primitive runs on the REAL chain in bytecode order; the 3 native ops (inner=sq+sig2,
// w=a+b·mag, half_w=w/2) + the abort branches (forward>0 / bracket≥0 / w>0) run in Rust. The
// transcription is independent of the Rust port (derived from bytecode, not onchain.rs), so a
// matching result is a genuine cross-check of the composition order, not me-vs-me.
//
// F3: `binary_price_pair`'s `&Clock` is UNUSED in its body (verified op-by-op) — zero gating, so
// compute_price needs no Clock and the fixture is timeless.

struct Oracle {
    id: String,
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
}

fn as_u64(v: &serde_json::Value) -> Result<u64> {
    Ok(v.as_str().context("expected stringified u64")?.parse()?)
}

fn fetch_oracle(id: &str) -> Result<Oracle> {
    let r = rpc_call(
        "sui_getObject",
        serde_json::json!([id, {"showContent": true}]),
    )?;
    let f = &r["data"]["content"]["fields"];
    let svi = &f["svi"]["fields"];
    let i64f = |field: &serde_json::Value| -> Result<(u64, bool)> {
        let ff = &field["fields"];
        Ok((
            as_u64(&ff["magnitude"])?,
            ff["is_negative"].as_bool().context("is_negative")?,
        ))
    };
    let (rho_mag, rho_neg) = i64f(&svi["rho"])?;
    let (m_mag, m_neg) = i64f(&svi["m"])?;
    let settlement = match &f["settlement_price"] {
        serde_json::Value::Null => None,
        v => Some(as_u64(v)?),
    };
    Ok(Oracle {
        id: id.to_string(),
        forward: as_u64(&f["prices"]["fields"]["forward"])?,
        a: as_u64(&svi["a"])?,
        b: as_u64(&svi["b"])?,
        sigma: as_u64(&svi["sigma"])?,
        rho_mag,
        rho_neg,
        m_mag,
        m_neg,
        settlement,
        expiry_ms: as_u64(&f["expiry"])?,
    })
}

fn pure_u64(inputs: &mut Vec<Input>, x: u64) -> Result<Argument> {
    inputs.push(Input::Pure(bcs::to_bytes(&x)?));
    Ok(Argument::Input((inputs.len() - 1) as u16))
}

/// i64::from_parts(mag, neg) where mag is any Argument (pure or a prior Result). Returns the I64
/// command's Result argument.
fn i64_from(
    inputs: &mut Vec<Input>,
    commands: &mut Vec<Command>,
    mag: Argument,
    neg: bool,
) -> Result<Argument> {
    inputs.push(Input::Pure(bcs::to_bytes(&neg)?));
    let nf = Argument::Input((inputs.len() - 1) as u16);
    commands.push(move_call("i64", "from_parts", vec![mag, nf])?);
    Ok(Argument::Result((commands.len() - 1) as u16))
}

fn last(commands: &[Command]) -> Argument {
    Argument::Result((commands.len() - 1) as u16)
}

fn read_u64(result: &serde_json::Value, cmd: usize) -> Result<u64> {
    Ok(bcs::from_bytes(&return_bytes(result, cmd)?)?)
}

fn read_i64(result: &serde_json::Value, cmd: usize) -> Result<(u64, bool)> {
    Ok(bcs::from_bytes(&return_bytes(result, cmd)?)?)
}

/// Run the transcribed compute_nd2. Returns (Some(price), None) or (None, Some(abort_code)).
/// On any primitive abort the chain reports the code; for the in-Rust branches we emit the same
/// codes compute_nd2 would (4 = bracket negative, 5 = w not positive).
fn chain_compute_price(o: &Oracle, strike: u64) -> Result<(Option<u64>, Option<u64>)> {
    // Settled path [compute_price B0-B4]: s > K ? 1e9 : 0 (strict; ties resolve DOWN). This branch
    // has NO chain math, so the ground-truth is the comparison itself — same op the port runs.
    if let Some(s) = o.settlement {
        return Ok((Some(if s > strike { SCALE } else { 0 }), None));
    }
    if o.forward == 0 {
        return Ok((None, Some(3)));
    }
    // ---- Segment 1: k, diff, sq, sig2, rho_term ----
    let (mut inp, mut cmd) = (Vec::new(), Vec::new());
    let k_arg = pure_u64(&mut inp, strike)?;
    let f_arg = pure_u64(&mut inp, o.forward)?;
    cmd.push(move_call_at(
        DEEPBOOK_PKG,
        "math",
        "div",
        vec![k_arg, f_arg],
    )?); // [18]
    let kdiv = last(&cmd);
    cmd.push(move_call("math", "ln", vec![kdiv])?); // [19] k (I64)  c1
    let c_k = cmd.len() - 1;
    let m_mag_arg = pure_u64(&mut inp, o.m_mag)?;
    let m_i = i64_from(&mut inp, &mut cmd, m_mag_arg, o.m_neg)?;
    let k_res = Argument::Result(c_k as u16);
    cmd.push(move_call("i64", "sub", vec![k_res, m_i])?); // [24] diff
    let c_diff = cmd.len() - 1;
    let diff = Argument::Result(c_diff as u16);
    cmd.push(move_call("i64", "square_scaled", vec![diff])?); // [27] sq
    let c_sq = cmd.len() - 1;
    let sig_a = pure_u64(&mut inp, o.sigma)?;
    let sig_b = pure_u64(&mut inp, o.sigma)?;
    cmd.push(move_call_at(
        DEEPBOOK_PKG,
        "math",
        "mul",
        vec![sig_a, sig_b],
    )?); // [35] sig2
    let c_sig2 = cmd.len() - 1;
    let rho_mag_arg = pure_u64(&mut inp, o.rho_mag)?;
    let rho_i = i64_from(&mut inp, &mut cmd, rho_mag_arg, o.rho_neg)?;
    cmd.push(move_call("i64", "mul_scaled", vec![rho_i, diff])?); // [47] rho_term
    let c_rt = cmd.len() - 1;
    let r1 = dev_inspect(&tx_kind_b64(inp, cmd)?)?;
    if !is_success(&r1) {
        return Ok((None, Some(abort_code(&r1)?)));
    }
    let (k_mag, k_neg) = read_i64(&r1, c_k)?;
    let sq = read_u64(&r1, c_sq)?;
    let sig2 = read_u64(&r1, c_sig2)?;
    let (rt_mag, rt_neg) = read_i64(&r1, c_rt)?;
    let inner = sq.checked_add(sig2).context("inner overflow")?; // native [37-39]

    // ---- Segment 2: sqrt_t, bracket, mag, b·mag ----
    let (mut inp, mut cmd) = (Vec::new(), Vec::new());
    let inner_a = pure_u64(&mut inp, inner)?;
    let e9_a = pure_u64(&mut inp, SCALE)?;
    cmd.push(move_call("math", "sqrt", vec![inner_a, e9_a])?); // [41]
    let sqrti = last(&cmd);
    let sqrt_t = i64_from(&mut inp, &mut cmd, sqrti, false)?; // [42] from_u64≡from_parts(.,false)
    let rt_mag_arg = pure_u64(&mut inp, rt_mag)?;
    let rt_i = i64_from(&mut inp, &mut cmd, rt_mag_arg, rt_neg)?;
    cmd.push(move_call("i64", "add", vec![rt_i, sqrt_t])?); // [51] bracket
    let c_bracket = cmd.len() - 1;
    let bracket = Argument::Result(c_bracket as u16);
    cmd.push(move_call("i64", "magnitude", vec![bracket])?); // [67] mag
    let mag_res = last(&cmd);
    let b_arg = pure_u64(&mut inp, o.b)?;
    cmd.push(move_call_at(
        DEEPBOOK_PKG,
        "math",
        "mul",
        vec![b_arg, mag_res],
    )?); // [68] b·mag
    let c_bmag = cmd.len() - 1;
    let r2 = dev_inspect(&tx_kind_b64(inp, cmd)?)?;
    if !is_success(&r2) {
        return Ok((None, Some(abort_code(&r2)?)));
    }
    let (_, bracket_neg) = read_i64(&r2, c_bracket)?;
    if bracket_neg {
        return Ok((None, Some(4))); // [54-59]
    }
    let bmag = read_u64(&r2, c_bmag)?;
    let w = o.a.checked_add(bmag).context("w overflow")?; // native [69]
    if w == 0 {
        return Ok((None, Some(5))); // [71-77]
    }
    let half_w = w / 2; // native [83-85]

    // ---- Segment 3: sqrt_w, numer, d, d2, normal_cdf ----
    let (mut inp, mut cmd) = (Vec::new(), Vec::new());
    let w_a = pure_u64(&mut inp, w)?;
    let e9_b = pure_u64(&mut inp, SCALE)?;
    cmd.push(move_call("math", "sqrt", vec![w_a, e9_b])?); // [80]
    let sqrtw_u = last(&cmd);
    let sqrt_w = i64_from(&mut inp, &mut cmd, sqrtw_u, false)?; // [81]
    let halfw_mag_arg = pure_u64(&mut inp, half_w)?;
    let halfw_i = i64_from(&mut inp, &mut cmd, halfw_mag_arg, false)?; // [86]
    let k_mag_arg = pure_u64(&mut inp, k_mag)?;
    let k_i = i64_from(&mut inp, &mut cmd, k_mag_arg, k_neg)?;
    cmd.push(move_call("i64", "add", vec![k_i, halfw_i])?); // [90] numer = k + half_w
    let numer = last(&cmd);
    cmd.push(move_call("i64", "div_scaled", vec![numer, sqrt_w])?); // [94] d
    let d = last(&cmd);
    cmd.push(move_call("i64", "neg", vec![d])?); // [97] d2
    let d2 = last(&cmd);
    cmd.push(move_call("math", "normal_cdf", vec![d2])?); // [100]
    let c_res = cmd.len() - 1;
    let r3 = dev_inspect(&tx_kind_b64(inp, cmd)?)?;
    if !is_success(&r3) {
        return Ok((None, Some(abort_code(&r3)?)));
    }
    Ok((Some(read_u64(&r3, c_res)?), None))
}

/// strike = forward · ratio (num/den), covering deep ITM/OTM + ATM.
const STRIKE_RATIOS: &[(u64, u64)] = &[
    (1, 2),
    (4, 5),
    (9, 10),
    (19, 20),
    (99, 100),
    (1, 1),
    (101, 100),
    (21, 20),
    (11, 10),
    (5, 4),
    (2, 1),
];

fn capture_e2e() -> Result<Vec<E2eCaseOut>> {
    let mut out = Vec::new();
    for id in ORACLE_IDS {
        let o = fetch_oracle(id)?;
        eprintln!(
            "oracle {id}: forward={} settled={:?}",
            o.forward, o.settlement
        );
        // strike grid; for a settled oracle add strike == settlement to exercise the strict-`>`
        // tie-break (ATM-at-settlement resolves DOWN -> 0).
        let mut strikes: Vec<u64> = STRIKE_RATIOS
            .iter()
            .map(|(num, den)| ((o.forward as u128) * (*num as u128) / (*den as u128)) as u64)
            .collect();
        if let Some(s) = o.settlement {
            strikes.push(s);
        }
        for strike in strikes {
            let (ret, expect_abort) = chain_compute_price(&o, strike)?;
            out.push(E2eCaseOut {
                oracle_id: o.id.clone(),
                forward: o.forward,
                a: o.a,
                b: o.b,
                sigma: o.sigma,
                rho_mag: o.rho_mag,
                rho_neg: o.rho_neg,
                m_mag: o.m_mag,
                m_neg: o.m_neg,
                settlement: o.settlement,
                expiry_ms: o.expiry_ms,
                strike,
                ret,
                expect_abort,
            });
        }
    }
    Ok(out)
}

fn main() -> Result<()> {
    // Preflight: channel sanity (Task 3 spike anchor).
    let b64 = tx_kind_b64(
        vec![
            Input::Pure(bcs::to_bytes(&4_000_000_000u64)?),
            Input::Pure(bcs::to_bytes(&1_000_000_000u64)?),
        ],
        vec![move_call(
            "math",
            "sqrt",
            vec![Argument::Input(0), Argument::Input(1)],
        )?],
    )?;
    let got: u64 = bcs::from_bytes(&return_bytes(&dev_inspect(&b64)?, 0)?)?;
    anyhow::ensure!(got == 2_000_000_000, "preflight sqrt(4.0) = {got}");
    eprintln!("preflight OK (per-command return values verified)");

    let meta = capture_meta("json-rpc")?;
    let cases = math_sweep(SEED);
    eprintln!("sweep: {} cases", cases.len());
    let math = capture_math(&cases)?;
    let aborts = math.iter().filter(|c| c.expect_abort.is_some()).count();
    eprintln!("captured {} math cases ({aborts} aborts)", math.len());

    let sweep = FixtureFile {
        meta: capture_meta("json-rpc")?,
        math,
        e2e: vec![],
    };
    let sweep_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../volarb-pricing/tests/fixtures/math_sweep.json"
    );
    std::fs::write(sweep_path, serde_json::to_string_pretty(&sweep)?)?;
    eprintln!("wrote {sweep_path}");

    // e2e: transcribed compute_nd2 over live oracles (Task 5).
    let e2e = capture_e2e()?;
    let e2e_aborts = e2e.iter().filter(|c| c.expect_abort.is_some()).count();
    eprintln!("captured {} e2e cases ({e2e_aborts} aborts)", e2e.len());
    let e2e_file = FixtureFile {
        meta,
        math: vec![],
        e2e,
    };
    let e2e_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../volarb-pricing/tests/fixtures/e2e_oracles.json"
    );
    std::fs::write(e2e_path, serde_json::to_string_pretty(&e2e_file)?)?;
    eprintln!("wrote {e2e_path}");
    Ok(())
}
