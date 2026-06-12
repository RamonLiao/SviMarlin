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
const RPC: &str = "https://fullnode.testnet.sui.io:443";
const SENDER: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
const SEED: u64 = 42;
const BATCH: usize = 40;

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
#[allow(dead_code)] // populated in the e2e capture (Task 5)
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

fn move_call(module: &str, function: &str, arguments: Vec<Argument>) -> Result<Command> {
    Ok(Command::MoveCall(MoveCall {
        package: Address::from_str(PKG)?,
        module: Identifier::new(module)?,
        function: Identifier::new(function)?,
        type_arguments: vec![],
        arguments,
    }))
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
        // db_mul/db_div mirror DeepBook math::mul/div; on chain we reach the identical code
        // through i64::mul_scaled/div_scaled with positive operands (port note in capture_points).
        "db_mul" | "db_div" => {
            let f = if case.func == "db_mul" {
                "mul_scaled"
            } else {
                "div_scaled"
            };
            inputs.push(Input::Pure(bcs::to_bytes(&case.args[0])?));
            let a_mag = Argument::Input((inputs.len() - 1) as u16);
            inputs.push(Input::Pure(bcs::to_bytes(&false)?));
            let a_neg = Argument::Input((inputs.len() - 1) as u16);
            commands.push(move_call("i64", "from_parts", vec![a_mag, a_neg])?);
            let a = Argument::Result((commands.len() - 1) as u16);
            inputs.push(Input::Pure(bcs::to_bytes(&case.args[1])?));
            let b_mag = Argument::Input((inputs.len() - 1) as u16);
            inputs.push(Input::Pure(bcs::to_bytes(&false)?));
            let b_neg = Argument::Input((inputs.len() - 1) as u16);
            commands.push(move_call("i64", "from_parts", vec![b_mag, b_neg])?);
            let b = Argument::Result((commands.len() - 1) as u16);
            ("i64", f, vec![a, b], RetKind::I64)
        }
        other => bail!("unknown case func {other}"),
    };
    commands.push(move_call(module, function, args)?);
    Ok((commands.len() - 1, ret))
}

/// Decode a target-command return value into the fixture `ret` JSON.
/// db_* cases: chain returned a (positive) I64 — record its magnitude as u64.
fn decode_ret(case: &Case, kind: &RetKind, bytes: &[u8]) -> Result<serde_json::Value> {
    Ok(match kind {
        RetKind::U64 => serde_json::json!(bcs::from_bytes::<u64>(bytes)?),
        RetKind::I64 => {
            let (mag, neg): (u64, bool) = bcs::from_bytes(bytes)?;
            if case.func.starts_with("db_") {
                anyhow::ensure!(!neg, "db_* fixture came back negative");
                serde_json::json!(mag)
            } else {
                serde_json::json!({"magnitude": mag, "is_negative": neg})
            }
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

    let file = FixtureFile {
        meta,
        math,
        e2e: vec![], // e2e capture lands in Task 5 (separate file)
    };
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../volarb-pricing/tests/fixtures/math_sweep.json"
    );
    std::fs::write(path, serde_json::to_string_pretty(&file)?)?;
    eprintln!("wrote {path}");
    Ok(())
}
