//! One-shot fixture capture for the L3 parity harness
//! (spec `docs/specs/2026-06-11-l0-parity-basis-harness-design.md`).
//!
//! Spike phase (Task 3): verify the channel returns per-command BCS return values (F2)
//! and learn the abort-shape of the response (used by the math sweep in Task 4).

use anyhow::{Context, Result, bail};
use base64::Engine;
use std::str::FromStr;
use sui_sdk_types::{
    Address, Argument, Command, Identifier, Input, MoveCall, ProgrammableTransaction,
    TransactionKind,
};

const PKG: &str = "0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138";
const RPC: &str = "https://fullnode.testnet.sui.io:443";
const SENDER: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

/// Build base64(BCS(TransactionKind)) for a PTB.
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

fn dev_inspect(tx_kind_b64: &str) -> Result<serde_json::Value> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "sui_devInspectTransactionBlock",
        "params": [SENDER, tx_kind_b64, null, null]
    });
    let resp: serde_json::Value = reqwest::blocking::Client::new()
        .post(RPC)
        .json(&body)
        .send()?
        .json()?;
    if let Some(err) = resp.get("error") {
        bail!("rpc error: {err}");
    }
    Ok(resp["result"].clone())
}

/// Decode `results[i].returnValues[j]` -> raw BCS bytes.
fn return_bytes(result: &serde_json::Value, cmd: usize, ret: usize) -> Result<Vec<u8>> {
    let rv = result["results"][cmd]["returnValues"][ret][0]
        .as_array()
        .with_context(|| {
            format!("no returnValues[{cmd}][{ret}] — channel unusable (F2): {result}")
        })?;
    Ok(rv.iter().map(|v| v.as_u64().unwrap() as u8).collect())
}

fn main() -> Result<()> {
    // --- Spike 1: per-command return values (F2) ---
    // PTB: math::sqrt(4e9, 1e9) == 2e9
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
    let result = dev_inspect(&b64)?;
    let got: u64 = bcs::from_bytes(&return_bytes(&result, 0, 0)?)?;
    anyhow::ensure!(got == 2_000_000_000, "sqrt(4.0) = {got}, want 2e9");
    println!("CHANNEL OK: per-command return values verified (sqrt(4.0) == 2.0)");

    // --- Spike 2: I64 construction via from_parts + chained call, and I64 return shape ---
    // PTB: i64::from_parts(693147180, true) -> math::exp(result) ~= 0.5e9
    let b64 = tx_kind_b64(
        vec![
            Input::Pure(bcs::to_bytes(&693_147_180u64)?),
            Input::Pure(bcs::to_bytes(&true)?),
        ],
        vec![
            move_call(
                "i64",
                "from_parts",
                vec![Argument::Input(0), Argument::Input(1)],
            )?,
            move_call("math", "exp", vec![Argument::Result(0)])?,
        ],
    )?;
    let result = dev_inspect(&b64)?;
    let i64_bytes = return_bytes(&result, 0, 0)?;
    println!("I64 BCS bytes (from_parts(693147180,true)): {i64_bytes:?}");
    let exp_val: u64 = bcs::from_bytes(&return_bytes(&result, 1, 0)?)?;
    println!("exp(-ln2) = {exp_val} (expect ~5e8)");

    // --- Spike 3: abort shape ---
    // PTB: math::ln(0) -> abort. Record where the code lands in the response.
    let b64 = tx_kind_b64(
        vec![Input::Pure(bcs::to_bytes(&0u64)?)],
        vec![move_call("math", "ln", vec![Argument::Input(0)])?],
    )?;
    let result = dev_inspect(&b64)?;
    println!(
        "abort response shape: effects.status = {}",
        serde_json::to_string_pretty(&result["effects"]["status"])?
    );
    println!("error field = {}", result["error"]);
    Ok(())
}
