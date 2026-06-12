# L0 Parity Harness + Basis 量測 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 frozen devInspect fixtures 證明 Rust L0 port（`crates/volarb-pricing/src/onchain.rs`）對鏈上 `oracle::compute_price` bit-exact，並量測 L0-vs-L1 basis。

**Architecture:** 兩層 fixtures（math 純函式 sweep + e2e live OracleSVI snapshot）由一次性 capture bin（`volarb-sui`）從 testnet devInspect 抓下、frozen 進 git；離線 parity harness（`volarb-pricing/tests/`）逐筆 `assert_eq!`（零 tolerance，含 abort 對齊）；basis 報告為 example bin，不進 CI。

**Tech Stack:** Rust（workspace 既有）；capture bin 加 dev 介接依賴（`reqwest` blocking、`serde_json`、`bcs`、`base64`、sui Rust SDK types）隔離在 `volarb-sui`；`volarb-pricing` 只加 `serde`/`serde_json` 為 **dev-dependencies**。

**Spec:** `docs/specs/2026-06-11-l0-parity-basis-harness-design.md`（含 sui-architect F1–F5）。

**對象 pkg:** `0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138`（testnet，Immutable）。

**Branch:** `feat/l0-parity-harness`（從 main 開）。

---

## 任務依賴

Task 1（曝光 math API + fixture schema）→ Task 2（harness，selftest 綠）→ Task 3（capture spike，需網路）→ Task 4（math sweep capture）→ Task 5（oracle 發現 + e2e capture）→ Task 6（跑 capture、freeze、harness 全綠）→ Task 7（basis 報告 + findings doc）→ Task 8（收尾）。

Task 3–6 需要 testnet 網路；Task 1–2 純離線。

---

### Task 1: 曝光 math 函式 + fixture JSON schema

**Files:**
- Modify: `crates/volarb-pricing/src/onchain.rs`（`fn` → `pub fn` ×6）
- Modify: `crates/volarb-pricing/Cargo.toml`（dev-deps）
- Create: `crates/volarb-pricing/tests/fixture_schema.rs`

- [ ] **Step 1: math 函式開 pub**

`onchain.rs` 中以下簽章把 `fn` 改 `pub fn`，各加一行 doc `/// Exposed for the L3 parity harness (Part 2). Mirrors chain op-for-op.`：

```rust
pub fn db_mul(a: u64, b: u64) -> Res<u64>
pub fn db_div(a: u64, b: u64) -> Res<u64>
pub fn exp(x: &I64) -> Res<u64>
pub fn sqrt(a: u64, b: u64) -> Res<u64>
pub fn ln(x: u64) -> Res<I64>
pub fn normal_cdf(x: &I64) -> Res<u64>
```

（`exp_series`/`sqrt_u128`/`normalize`/`ln_u128`/`poly_fold` 維持 private——鏈上也非 exposed，fixture 只打 exposed 函式。）

- [ ] **Step 2: dev-deps**

`crates/volarb-pricing/Cargo.toml` 加：

```toml
[dev-dependencies]
serde = { workspace = true, features = ["derive"] }
serde_json = "1"
```

（若 workspace.dependencies 無 serde_json，root `Cargo.toml` 補 `serde_json = "1"` 並改用 `serde_json.workspace = true`。）

- [ ] **Step 3: 寫 schema 測試（先紅）**

`crates/volarb-pricing/tests/fixture_schema.rs`：

```rust
//! Frozen-fixture JSON schema. Shared by the parity harness and the capture bin
//! (capture bin duplicates these structs — volarb-sui must not depend on pricing test code;
//! the JSON file format IS the contract).

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct FixtureFile {
    pub meta: Meta,
    pub math: Vec<MathCase>,
    pub e2e: Vec<E2eCase>,
}

#[derive(Debug, Deserialize)]
pub struct Meta {
    pub chain: String,        // "testnet"
    pub package: String,      // 0xf5ea…
    pub protocol_version: u64,
    pub captured_at_ms: u64,
    pub channel: String,      // "json-rpc" | "grpc"
    pub seed: u64,
}

/// 一筆 math 純函式呼叫。args 是 1e9-FP；I64 參數展平成 (magnitude, is_negative)。
#[derive(Debug, Deserialize)]
pub struct MathCase {
    pub func: String, // "ln" | "exp" | "sqrt" | "normal_cdf" | "mul_scaled" | "div_scaled" | "square_scaled"
    pub args: Vec<u64>,
    pub neg_flags: Vec<bool>,        // 對應 I64 參數的 is_negative；非 I64 函式為空
    pub ret: Option<RetVal>,         // None ⇔ expect_abort
    pub expect_abort: Option<u64>,   // 鏈上 abort code
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
    /// OracleSVI snapshot（capture 時讀 object 欄位）
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
    pub ret: Option<u64>,          // on-chain compute_price 回傳
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
         "ret":308537538,"expect_abort":null}
      ],
      "e2e": [
        {"oracle_id":"0xabc","forward":1000000000,"a":10000000,"b":50000000,
         "sigma":200000000,"rho_mag":100000000,"rho_neg":true,"m_mag":0,"m_neg":false,
         "settlement":null,"expiry_ms":2,"strike":1000000000,
         "ret":495000000,"expect_abort":null}
      ]
    }"#;
    let f: FixtureFile = serde_json::from_str(j).unwrap();
    assert_eq!(f.math.len(), 3);
    assert!(matches!(f.math[2].ret, Some(RetVal::U64(_))));
    assert_eq!(f.e2e[0].forward, 1_000_000_000);
}
```

- [ ] **Step 4: 跑紅** — `cargo test -p volarb-pricing --test fixture_schema`，Expected: FAIL（serde 未加時 compile error）→ 加完 deps 後 PASS。
- [ ] **Step 5: 全 gate** — `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`，Expected: 全綠（Part 1 33 tests 不退步）。
- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat(pricing): expose L0 math fns + parity fixture schema"`

---

### Task 2: Parity harness（selftest 機制先綠，chain fixtures 留紅）

**Files:**
- Create: `crates/volarb-pricing/tests/onchain_parity.rs`
- Create: `crates/volarb-pricing/tests/fixtures/.gitkeep`

- [ ] **Step 1: 寫 harness**

`tests/onchain_parity.rs`（schema structs 用 `#[path]` include 共用，避免重複）：

```rust
//! L3 parity harness — replays frozen devInspect fixtures against the Rust L0 port.
//! Zero tolerance: assert_eq! on u64. Fixtures are ground truth; NEVER edit them to pass.

#[path = "fixture_schema.rs"]
mod schema;
use schema::{E2eCase, FixtureFile, MathCase, RetVal};
use volarb_pricing::onchain::{
    db_div, db_mul, exp, ln, normal_cdf, sqrt, I64, OnchainError, OnchainOracle,
};

/// 鏈上 abort code → 我們的 OnchainError 對齊表（Part 1 註解的 code 對應）。
fn abort_matches(code: u64, err: &OnchainError) -> bool {
    use OnchainError::*;
    matches!(
        (code, err),
        (0, MagnitudeOverflow) | (0, LnZero) | (1, DivByZero) | (1, ExpOverflow)
            | (2, SqrtDomain) | (3, ForwardNonPositive) | (4, BracketNegative)
            | (5, WNonPositive)
    )
}

fn run_math(c: &MathCase) {
    let i64arg = |i: usize| I64::from_parts(c.args[i], *c.neg_flags.get(i).unwrap_or(&false));
    // I64 回傳統一轉 (mag, neg)；u64 回傳轉 (val, false) 比對
    let got: Result<(u64, bool), OnchainError> = match c.func.as_str() {
        "ln" => ln(c.args[0]).map(|v| (v.magnitude(), v.is_negative())),
        "exp" => exp(&i64arg(0)).map(|v| (v, false)),
        "sqrt" => sqrt(c.args[0], c.args[1]).map(|v| (v, false)),
        "normal_cdf" => normal_cdf(&i64arg(0)).map(|v| (v, false)),
        "mul_scaled" => i64arg(0).mul_scaled(&i64arg(1)).map(|v| (v.magnitude(), v.is_negative())),
        "div_scaled" => i64arg(0).div_scaled(&i64arg(1)).map(|v| (v.magnitude(), v.is_negative())),
        "square_scaled" => i64arg(0).square_scaled().map(|v| (v, false)),
        "db_mul" => db_mul(c.args[0], c.args[1]).map(|v| (v, false)),
        "db_div" => db_div(c.args[0], c.args[1]).map(|v| (v, false)),
        other => panic!("unknown fixture func {other}"),
    };
    match (&c.ret, c.expect_abort, got) {
        (Some(RetVal::U64(want)), None, Ok((g, neg))) => {
            assert!(!neg, "{}: chain returned u64 but port returned negative I64, args={:?}", c.func, c.args);
            assert_eq!(g, *want, "{} args={:?} negs={:?}: port={g} ({g:#x}) chain={want} ({want:#x})", c.func, c.args, c.neg_flags);
        }
        (Some(RetVal::I64 { magnitude, is_negative }), None, Ok((g, neg))) => {
            assert_eq!((g, neg), (*magnitude, *is_negative), "{} args={:?}: port=({g},{neg}) chain=({magnitude},{is_negative})", c.func, c.args);
        }
        (None, Some(code), Err(e)) => {
            assert!(abort_matches(code, &e), "{} args={:?}: chain abort {code} but port err {e:?}", c.func, c.args);
        }
        (ret, ab, got) => panic!("{} args={:?}: fixture(ret={ret:?},abort={ab:?}) vs port {got:?}", c.func, c.args),
    }
}

fn run_e2e(c: &E2eCase) {
    let oracle = OnchainOracle {
        forward: c.forward, a: c.a, b: c.b, sigma: c.sigma,
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
        (None, Some(code), Err(e)) => assert!(
            abort_matches(code, &e),
            "oracle {} strike {}: chain abort {code}, port err {e:?}", c.oracle_id, c.strike
        ),
        (r, a, g) => panic!("oracle {} strike {}: fixture(ret={r:?},abort={a:?}) vs port {g:?}", c.oracle_id, c.strike),
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

fn fixture_paths(filter_selftest: bool) -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            let is_self = p.file_name().unwrap().to_str().unwrap().starts_with("_selftest");
            if filter_selftest { is_self } else { !is_self }
        })
        .collect();
    v.sort();
    v
}

/// Harness 機制自測：fixture 值由 Rust port 自產（**非** chain ground truth），只驗 runner 邏輯。
#[test]
fn selftest_harness_mechanics() {
    let paths = fixture_paths(true);
    assert!(!paths.is_empty(), "_selftest fixture missing");
    for p in paths { run_file(&p); }
}

/// 真 parity：chain-captured frozen fixtures。fixtures 缺失 = FAIL LOUD（spec §4）。
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
        nm += m; ne += e;
    }
    println!("parity OK: {nm} math cases + {ne} e2e cases across {} files", paths.len());
}
```

- [ ] **Step 2: 產 selftest fixture**

寫一次性小程式或用既有 unit test 的 anchor 值，手工組 `tests/fixtures/_selftest.json`：math 至少蓋每個 func 各 1 筆 OK + 1 筆 abort（值 = 跑 Rust port 得到的輸出，如 `sqrt(4e9,1e9)=2e9`、`ln(0)→abort 0`、`exp(-0.5)`、`normal_cdf(±x)`、`mul/div/square_scaled`、`db_mul/db_div`），e2e 1 筆（用 onchain.rs `sample_oracle` 參數 + Rust `compute_price` 輸出）。檔案開頭 meta `"channel":"selftest"`。

- [ ] **Step 3: 跑** — `cargo test -p volarb-pricing --test onchain_parity`。Expected：`selftest_harness_mechanics` PASS、`chain_parity_bit_exact` **FAIL**（無 chain fixtures——這是預期的紅，Task 6 轉綠）。
- [ ] **Step 4: 暫時標記** — 給 `chain_parity_bit_exact` 加 `#[ignore = "fixtures land in Task 6 — same branch"]`，讓中間 commit CI 綠；**Task 6 必須移除 ignore**（plan 內 loud 標註，不准留到 merge）。
- [ ] **Step 5: gate + Commit** — workspace 全 gate 綠後 `git commit -m "test(pricing): L3 parity harness + selftest fixture (chain fixtures pending capture)"`

---

### Task 3: Capture spike — 通道驗證（需 testnet 網路）

**Files:**
- Modify: `crates/volarb-sui/Cargo.toml`
- Create: `crates/volarb-sui/src/bin/capture_fixtures.rs`（spike 版）

**目的（spec F2）：證明所選通道給得出 per-command BCS return values。** JSON-RPC `devInspectTransactionBlock` 確定支援 `results[].returnValues`；先實測 gRPC `SimulateTransaction`——**先用 `sui-docs-query` 查 gRPC simulate 是否回 command outputs 與 Rust client 現況**，查不到或不支援 → 直接用 JSON-RPC（spec 已准）。

- [ ] **Step 1: 加依賴（隔離在 volarb-sui）**

```toml
[dependencies]
volarb-core.workspace = true
reqwest = { version = "0.12", features = ["blocking", "json"] }
serde = { workspace = true, features = ["derive"] }
serde_json = "1"
bcs = "0.1"
base64 = "0.22"
anyhow = "1"
sui-sdk-types = "0.0.6"            # MystenLabs sui-rust-sdk types（版本以 crates.io 最新 0.x 為準）
sui-transaction-builder = "0.0.6"  # 同上；spike 時若 API 不合用，fallback 手組 BCS TransactionKind
```

> **注意（dev-rules SDK 條款）**：sui Rust SDK 0.x API 不穩定，本 task 第一步先 `cargo add` 後讀該版 docs.rs/source 確認 `TransactionBuilder` 能輸出 `TransactionKind` bytes（devInspect 要的是 **TransactionKind**，非完整 TransactionData）。不合用 → 用 `sui-sdk-types` 的 `TransactionKind::ProgrammableTransaction` 結構手組 + `bcs::to_bytes`。

- [ ] **Step 2: spike main**

`capture_fixtures.rs` spike 版：組單一 PTB `math::sqrt(4_000_000_000, 1_000_000_000)`，打 testnet fullnode `https://fullnode.testnet.sui.io:443` `sui_devInspectTransactionBlock`（sender = `0x0000…0000`，tx_bytes = base64(BCS(TransactionKind))），解析 `result.results[0].returnValues[0]` 的 BCS bytes → u64，assert == `2_000_000_000`，印出全 JSON。

```rust
//! One-shot fixture capture for the L3 parity harness (spec 2026-06-11).
//! Spike phase: verify the channel returns per-command BCS return values (F2).

use anyhow::{bail, Context, Result};

const PKG: &str = "0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138";
const RPC: &str = "https://fullnode.testnet.sui.io:443";
const SENDER: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

fn dev_inspect(tx_kind_b64: &str) -> Result<serde_json::Value> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "sui_devInspectTransactionBlock",
        "params": [SENDER, tx_kind_b64, null, null]
    });
    let resp: serde_json::Value = reqwest::blocking::Client::new()
        .post(RPC).json(&body).send()?.json()?;
    if let Some(err) = resp.get("error") { bail!("rpc error: {err}"); }
    Ok(resp["result"].clone())
}

fn main() -> Result<()> {
    // PTB: math::sqrt(4e9, 1e9) — 組法見 Step 1 的 builder/手組 BCS 決策
    let tx_kind_b64 = build_sqrt_ptb()?; // spike 重點：這個函式跑通
    let result = dev_inspect(&tx_kind_b64)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    let rv = result["results"][0]["returnValues"][0][0]
        .as_array().context("no returnValues — channel unusable (F2)")?;
    let bytes: Vec<u8> = rv.iter().map(|v| v.as_u64().unwrap() as u8).collect();
    let got: u64 = bcs::from_bytes(&bytes)?;
    assert_eq!(got, 2_000_000_000, "sqrt(4.0) should be 2.0 in 1e9-FP");
    println!("CHANNEL OK: per-command return values verified");
    Ok(())
}
```

（`build_sqrt_ptb` 在 spike 中實作：builder 路徑或手組 `ProgrammableTransaction{ inputs: [Pure(bcs(4e9)), Pure(bcs(1e9))], commands: [MoveCall{ package: PKG, module: "math", function: "sqrt", args }] }` → `TransactionKind::ProgrammableTransaction` → `bcs::to_bytes` → base64。）

- [ ] **Step 3: 跑通** — `cargo run -p volarb-sui --bin capture_fixtures`。Expected：印 `CHANNEL OK`。失敗 → 印的全 JSON 進 debug；連 JSON-RPC 都不行才回頭查 gRPC。
- [ ] **Step 4: 順手驗 abort 形狀** — spike 加打一筆 `math::ln(0)`，確認 response 的 `effects.status`（或 `error` 欄位）帶 `MoveAbort` + code，記下解析路徑（Task 4 要用）。
- [ ] **Step 5: Commit** — `git commit -m "feat(sui): capture spike — devInspect channel verified (per-command return values)"`

---

### Task 4: Math sweep capture（點生成 + 批次 + abort）

**Files:**
- Modify: `crates/volarb-sui/src/bin/capture_fixtures.rs`
- Create: `crates/volarb-sui/src/bin/capture/points.rs`（`#[path]` module 或 `mod points;` 子檔）

- [ ] **Step 1: 先寫點生成的 unit test（純函式，離線可測）**

```rust
// points.rs — deterministic sweep point generation. NO Date::now / rand crates.
pub const SCALE: u64 = 1_000_000_000;
const B_BREAK: u64 = 5_656_854_249; // normal_cdf regime A/B 切換（findings doc + Part 1）

/// 簡單 LCG，固定 seed，capture 可重現（spec F4/meta.seed）。
pub struct Lcg(pub u64);
impl Lcg {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }
    pub fn in_range(&mut self, lo: u64, hi: u64) -> u64 { lo + self.next() % (hi - lo + 1) }
}

pub struct Case { pub func: &'static str, pub args: Vec<u64>, pub neg_flags: Vec<bool> }

/// 每個函式：邊界值 + 隨機點。abort 邊界刻意包含（如 ln(0)、sqrt(b=0)、exp 大正值）。
pub fn math_sweep(seed: u64) -> Vec<Case> {
    let mut r = Lcg(seed);
    let mut v = Vec::new();
    // ln: 0(abort), 1, SCALE-1, SCALE, SCALE+1, 大值, 隨機 ×20
    for x in [0u64, 1, SCALE - 1, SCALE, SCALE + 1, u64::MAX / SCALE] {
        v.push(Case { func: "ln", args: vec![x], neg_flags: vec![] });
    }
    for _ in 0..20 { v.push(Case { func: "ln", args: vec![r.in_range(1, 100 * SCALE)], neg_flags: vec![] }); }
    // exp: ±{0,1,LN2,SCALE,5*SCALE,30*SCALE(±overflow/saturate 邊界),隨機×20}
    for (m, n) in [(0u64, false), (693_147_180, false), (693_147_180, true),
                   (SCALE, false), (SCALE, true), (30 * SCALE, false), (30 * SCALE, true),
                   (50 * SCALE, false), (50 * SCALE, true)] {
        v.push(Case { func: "exp", args: vec![m], neg_flags: vec![n] });
    }
    for _ in 0..20 {
        v.push(Case { func: "exp", args: vec![r.in_range(0, 40 * SCALE)], neg_flags: vec![r.next() % 2 == 0] });
    }
    // sqrt(a, b): 完全平方、0、SCALE 邊界、b=0(abort)、b>SCALE(abort)、隨機×20
    for (a, b) in [(4 * SCALE, SCALE), (0, SCALE), (SCALE, SCALE), (u64::MAX / SCALE, SCALE),
                   (SCALE, 0), (SCALE, SCALE + 1)] {
        v.push(Case { func: "sqrt", args: vec![a, b], neg_flags: vec![] });
    }
    for _ in 0..20 { v.push(Case { func: "sqrt", args: vec![r.in_range(0, 1u64 << 50), SCALE], neg_flags: vec![] }); }
    // normal_cdf: ±{0,1,SCALE/2,SCALE,2S,4S,B_BREAK-1,B_BREAK,B_BREAK+1,8S}, 隨機×30
    for m in [0, 1, SCALE / 2, SCALE, 2 * SCALE, 4 * SCALE, B_BREAK - 1, B_BREAK, B_BREAK + 1, 8 * SCALE] {
        for n in [false, true] { v.push(Case { func: "normal_cdf", args: vec![m], neg_flags: vec![n] }); }
    }
    for _ in 0..30 {
        v.push(Case { func: "normal_cdf", args: vec![r.in_range(0, 7 * SCALE)], neg_flags: vec![r.next() % 2 == 0] });
    }
    // i64 mul/div/square_scaled + db_mul/db_div: 邊界（0、±1、SCALE、cast-back 超界 abort）+ 隨機×15/func
    for (am, an, bm, bn) in [(0, false, SCALE, false), (SCALE, true, SCALE, true),
                              (u64::MAX, false, u64::MAX, false) /* abort: overflow */,
                              (SCALE, false, 0, false) /* div: abort DivByZero */] {
        v.push(Case { func: "mul_scaled", args: vec![am, bm], neg_flags: vec![an, bn] });
        v.push(Case { func: "div_scaled", args: vec![am, bm], neg_flags: vec![an, bn] });
    }
    for _ in 0..15 {
        let (am, bm) = (r.in_range(0, 1u64 << 40), r.in_range(1, 1u64 << 40));
        let (an, bn) = (r.next() % 2 == 0, r.next() % 2 == 0);
        v.push(Case { func: "mul_scaled", args: vec![am, bm], neg_flags: vec![an, bn] });
        v.push(Case { func: "div_scaled", args: vec![am, bm], neg_flags: vec![an, bn] });
        v.push(Case { func: "square_scaled", args: vec![am], neg_flags: vec![an] });
        v.push(Case { func: "db_mul", args: vec![am, bm], neg_flags: vec![] });
        v.push(Case { func: "db_div", args: vec![am, bm], neg_flags: vec![] });
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
        assert!(a.iter().zip(&b).all(|(x, y)| x.args == y.args && x.neg_flags == y.neg_flags));
        for f in ["ln", "exp", "sqrt", "normal_cdf", "mul_scaled", "div_scaled",
                  "square_scaled", "db_mul", "db_div"] {
            assert!(a.iter().any(|c| c.func == f), "missing {f}");
        }
        assert!(a.len() >= 150, "sweep too small: {}", a.len());
    }
}
```

跑：`cargo test -p volarb-sui` Expected: PASS。

- [ ] **Step 2: capture 主流程**

main 改成：`math_sweep(seed=42)` → 對每個 case 組 moveCall（I64 參數先 `i64::from_parts(mag, neg)` moveCall 再把 result 餵進目標函式——同一 PTB 內 `Argument::Result(i)`）→ **批次**：每 PTB 裝 ≤100 個 case（含 from_parts 前置 call，留 1024 限制餘裕；**abort case 必須單獨成一個 PTB**——一個 command abort 會廢掉整個 PTB 的 results）→ 逐 PTB devInspect → OK case 解 `results[i].returnValues`、abort case 解 `effects` 的 `MoveAbort` code → 組 `MathCase`（schema 同 Task 1，capture bin 自帶一份 serde **Serialize** structs，JSON 格式即契約）。
寫出 `crates/volarb-pricing/tests/fixtures/math_sweep.json`（含 meta：chain/package/protocol_version（`sui_getProtocolConfig` 拿）/captured_at_ms（response 的 timestamp 或 checkpoint，**不用本機時鐘**也行，記 RPC 回的）/channel/seed）。

- [ ] **Step 3: I64 回傳解碼** — `ln/mul_scaled/div_scaled` 回 `i64::I64` struct，BCS = `(u64, bool)` 順序照 struct 定義（`magnitude` 在前——以 spike 實測 bytes 確認，**不要假設**）。
- [ ] **Step 4: Commit** — `git commit -m "feat(sui): math sweep capture — deterministic points, batched PTBs, abort capture"`

---

### Task 5: Oracle 發現 + e2e capture（F1/F3）

**Files:**
- Modify: `crates/volarb-sui/src/bin/capture_fixtures.rs`

- [ ] **Step 1（F3 gate）: 重新 disassemble 確認 Clock**

```bash
# 拿 oracle module bytecode 並 disassemble（流程同 2026-05-31 spike）
curl -s $RPC -X POST -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,
  "method":"sui_getObject","params":["0xf5ea…5138",{"showBcs":true}]}' > /tmp/pkg.json
# base64-decode moduleMap.oracle → /tmp/oracle.mv → sui move disassemble
```

逐 opcode 確認 `binary_price_pair` 的 `&Clock` 是否 gating（expiry assert / staleness）。**有 gating → e2e fixture 只收 `compute_price`**；無 → 兩個都收。結論寫進 findings doc。

- [ ] **Step 2（F1）: OracleSVI 枚舉**

優先序：① `sui_getNormalizedMoveModule`（module=`registry`）看有無 oracle 列表 accessor → ② `suix_queryEvents` 抓近期 `{PKG}::oracle::OracleSVIUpdated`，收集 distinct oracle object ids。capture bin 實作其中可行的一條（lessons 2026-05-29：直接打 RPC，不繞搜尋）。

- [ ] **Step 3: e2e capture**

對每個 oracle id：`sui_getObject {showContent:true}` snapshot 全欄位（forward/a/b/sigma/rho/m/settlement/expiry/timestamp）→ 產 strike grid：`forward × {0.5, 0.8, 0.9, 0.95, 0.99, 1.0, 1.01, 1.05, 1.1, 1.25, 2.0}`（db_mul 比例，覆蓋 deep ITM/OTM + ATM）→ 每 (oracle, strike) devInspect `oracle::compute_price(oracle_obj, strike)`（oracle 是 shared object → input 用 SharedObject arg）→ 記 ret/abort。settled oracle 若存在，strike grid 額外加 `settlement` 本身（驗 strict `>` tie）。寫 `tests/fixtures/e2e_oracles.json`。

- [ ] **Step 4: Commit** — `git commit -m "feat(sui): e2e capture — oracle discovery via registry/events, strike grid devInspect"`

---

### Task 6: 跑 capture、freeze fixtures、parity 全綠

- [ ] **Step 1: 跑 capture** — `cargo run -p volarb-sui --bin capture_fixtures`。產出 `math_sweep.json` + `e2e_oracles.json`。testnet 無 live oracle → math 照 freeze，e2e 缺口 **loud 標註**進 findings doc + progress（spec §6 風險表）。
- [ ] **Step 2: 移除 `#[ignore]`**（Task 2 Step 4 的暫時標記——**不移除不准 merge**）。
- [ ] **Step 3: 跑 parity** — `cargo test -p volarb-pricing --test onchain_parity -- --nocapture`。Expected: `chain_parity_bit_exact` PASS，印 case 統計。
  - **紅 = port bug**：走 systematic-debugging，逐 opcode 對 bytecode 修 `onchain.rs`，**不改 fixture**。修完所有 Part 1 unit tests 也要重跑。
- [ ] **Step 4: onchain.rs module doc 更新** — 把開頭 "NOT yet parity-verified" 段落改為 parity 狀態（math sweep N cases + e2e M cases bit-exact，capture 日期、fixtures 路徑；e2e 若有缺口照實寫）。
- [ ] **Step 5: 全 gate** — `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`。
- [ ] **Step 6: Commit** — `git add -A && git commit -m "test(pricing): freeze chain fixtures — L0 parity bit-exact verified"`

---

### Task 7: Basis 報告 + findings doc

**Files:**
- Create: `crates/volarb-pricing/examples/measure_basis.rs`
- Create: `docs/specs/2026-06-XX-l0-parity-basis-findings.md`（XX = 實際日期）

- [ ] **Step 1: basis example**

```rust
//! L0-vs-L1 basis measurement (one-shot report, NOT CI; spec §5).
//! L0 = chain-exact integer price; L1 = float BS digital off annualized σ.
//! Run: cargo run -p volarb-pricing --example measure_basis

use volarb_pricing::binary::binary_price; // L1（簽章以現檔為準）
use volarb_pricing::onchain::{I64, OnchainOracle, SCALE};

fn main() {
    // 讀 e2e fixtures（schema 同 tests/fixture_schema.rs，example 內重複一份 Deserialize struct）
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/e2e_oracles.json");
    let f: FixtureFile = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut diffs_ticks: Vec<f64> = Vec::new();
    for c in &f.e2e {
        if c.settlement.is_some() || c.ret.is_none() { continue; } // settled/abort 無 basis 意義
        let l0 = c.ret.unwrap();
        // raw w → annualized σ：w = (鏈上 SVI eval at k)，T = (expiry - captured_at)/MS_PER_YEAR
        // 重算 w 用 port 的 nd2 路徑中間量不可得 → 以同公式 float 重算 w(k)
        let fwd = c.forward as f64 / SCALE as f64;
        let strike = c.strike as f64 / SCALE as f64;
        let k = (strike / fwd).ln();
        let (rho, m) = (sgn(c.rho_mag, c.rho_neg), sgn(c.m_mag, c.m_neg));
        let (a, b, sig) = (c.a as f64 / 1e9, c.b as f64 / 1e9, c.sigma as f64 / 1e9);
        let w = a + b * (rho * (k - m) + ((k - m).powi(2) + sig * sig).sqrt());
        let t_years = (c.expiry_ms.saturating_sub(f.meta.captured_at_ms)) as f64
            / volarb_core::svi::MS_PER_YEAR as f64;
        if w <= 0.0 || t_years <= 0.0 { continue; }
        let sigma_ann = (w / t_years).sqrt();
        let l1 = binary_price(fwd, strike, sigma_ann, t_years); // 簽章不合再調
        let l0_f = l0 as f64 / SCALE as f64;
        diffs_ticks.push((l1 - l0_f).abs() * SCALE as f64);
    }
    diffs_ticks.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let n = diffs_ticks.len();
    println!("n={n}  max={:.0}  mean={:.0}  p50={:.0}  p99={:.0} (ticks, 1 tick = 1e-9)",
        diffs_ticks.last().unwrap_or(&0.0),
        diffs_ticks.iter().sum::<f64>() / n.max(1) as f64,
        diffs_ticks.get(n / 2).unwrap_or(&0.0),
        diffs_ticks.get(n * 99 / 100).unwrap_or(&0.0));
}
fn sgn(mag: u64, neg: bool) -> f64 {
    let v = mag as f64 / 1e9;
    if neg { -v } else { v }
}
```

（`binary_price` 真實簽章/單位以 `src/binary.rs` 為準，執行時調整；example 需要 `serde` → `volarb-pricing` 的 dev-deps 已含（examples 用 dev-deps）。）

- [ ] **Step 2: 跑 + 寫 findings doc** — `cargo run -p volarb-pricing --example measure_basis`，數字 + 分佈 + 「router edge buffer 至少留 X ticks」結論寫進 findings doc。內容含：parity 統計、F3 Clock 結論、e2e 覆蓋缺口（如有）、basis 表。
- [ ] **Step 3: Commit** — `git commit -m "feat(pricing): L0-vs-L1 basis measurement + findings doc"`

---

### Task 8: 收尾

- [ ] **Step 1: 全 gate 最終跑**（test/clippy/fmt --workspace）。
- [ ] **Step 2: dual-review**（dev-rules 兩輪制；Move 無改動所以 generic 流程可用，`onchain.rs` 數值部分留意 lessons 2026-06-03）。
- [ ] **Step 3: merge 決策** — 走 `superpowers:finishing-a-development-branch`。
- [ ] **Step 4: 更新 `tasks/progress.md`**（TODO #5 Part 2 完成、basis 數字、e2e 缺口如有）+ `tasks/lessons.md`（capture 踩雷如有）。

---

## Self-review notes

- Spec §3.1 sweep（含 abort fixtures）→ Task 4；§3.2 e2e + F1 枚舉 + F3 Clock → Task 5；§3.3 F2 通道判準 → Task 3；§4 harness fail-loud + 零 tolerance → Task 2/6（ignore 標記 loud、Task 6 強制移除）；§5 basis → Task 7；§7 成功標準 → Task 6/8。
- 已知不確定點（loud）：sui Rust SDK 0.x API 細節（Task 3 Step 1 強制先讀該版 source）、I64 BCS 欄位序（Task 4 Step 3 實測）、`binary_price` 簽章（Task 7 以現檔為準）。這些都在對應 task 內有驗證步驟，不是 placeholder。
