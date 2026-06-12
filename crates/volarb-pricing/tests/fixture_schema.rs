//! Frozen-fixture JSON schema. Shared by the parity harness (via `#[path]` include) and the
//! capture bin (volarb-sui duplicates the Serialize side — the JSON file format IS the contract).
//
// Shared across test targets with different usage (harness reads ret/abort; the basis example
// reads meta/expiry) — dead_code is expected per-target.
#![allow(dead_code)]

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct FixtureFile {
    pub meta: Meta,
    pub math: Vec<MathCase>,
    pub e2e: Vec<E2eCase>,
}

#[derive(Debug, Deserialize)]
pub struct Meta {
    pub chain: String,   // "testnet"
    pub package: String, // 0xf5ea…
    pub protocol_version: u64,
    pub captured_at_ms: u64,
    pub channel: String, // "json-rpc" | "grpc" | "selftest"
    pub seed: u64,
}

/// One pure-math devInspect call. Args are 1e9-FP; I64 args flatten to (magnitude, is_negative).
#[derive(Debug, Deserialize)]
pub struct MathCase {
    pub func: String,
    pub args: Vec<u64>,
    /// is_negative flags for I64 args (empty for u64-only funcs).
    pub neg_flags: Vec<bool>,
    /// None ⇔ expect_abort is Some.
    pub ret: Option<RetVal>,
    /// On-chain abort code.
    pub expect_abort: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RetVal {
    U64(u64),
    I64 { magnitude: u64, is_negative: bool },
}

#[derive(Debug, Deserialize)]
pub struct E2eCase {
    pub oracle_id: String,
    pub forward: u64,
    pub a: u64,
    pub b: u64,
    pub sigma: u64,
    pub rho_mag: u64,
    pub rho_neg: bool,
    pub m_mag: u64,
    pub m_neg: bool,
    pub settlement: Option<u64>,
    pub expiry_ms: u64,
    pub strike: u64,
    /// On-chain compute_price return.
    pub ret: Option<u64>,
    pub expect_abort: Option<u64>,
}

#[test]
fn schema_roundtrip() {
    let j = r#"{
      "meta": {"chain":"testnet","package":"0xf5","protocol_version":124,
               "captured_at_ms":1,"channel":"json-rpc","seed":42},
      "math": [
        {"func":"sqrt","args":[4000000000,1000000000],"neg_flags":[],
         "ret":2000000000,"expect_abort":null},
        {"func":"ln","args":[0],"neg_flags":[],"ret":null,"expect_abort":0},
        {"func":"normal_cdf","args":[500000000],"neg_flags":[true],
         "ret":308537538,"expect_abort":null},
        {"func":"ln","args":[500000000],"neg_flags":[],
         "ret":{"magnitude":693147180,"is_negative":true},"expect_abort":null}
      ],
      "e2e": [
        {"oracle_id":"0xabc","forward":1000000000,"a":10000000,"b":50000000,
         "sigma":200000000,"rho_mag":100000000,"rho_neg":true,"m_mag":0,"m_neg":false,
         "settlement":null,"expiry_ms":2,"strike":1000000000,
         "ret":495000000,"expect_abort":null}
      ]
    }"#;
    let f: FixtureFile = serde_json::from_str(j).unwrap();
    assert_eq!(f.math.len(), 4);
    assert!(matches!(f.math[2].ret, Some(RetVal::U64(_))));
    assert!(matches!(
        f.math[3].ret,
        Some(RetVal::I64 {
            is_negative: true,
            ..
        })
    ));
    assert_eq!(f.e2e[0].forward, 1_000_000_000);
    assert_eq!(f.math[1].expect_abort, Some(0));
}
