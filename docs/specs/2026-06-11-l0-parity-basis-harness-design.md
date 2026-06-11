# Design: L0 Parity Harness + Basis 量測（TODO #5 Plan B Part 2）

> Date: 2026-06-11
> Status: Approved（brainstorming 拍板）
> Prereq: Part 1 已 merge（`b03d7de`，`crates/volarb-pricing/src/onchain.rs`，33 tests 綠）
> 目標：證明 Rust L0 port 對鏈上 `oracle::compute_price` **bit-exact**，並量測 L0-vs-L1 basis。

## 1. 拍板決策（brainstorming forks）

1. **兩層 capture**：math 層任意輸入 sweep（純函式，devInspect 可餵任意參數）+ e2e 層 live `OracleSVI` snapshot（compute_price 吃 object，只能用 capture 當下鏈上實際 oracle）。math 層抓 bit-level / 邊界 bug，e2e 層驗組合順序（lessons 2026-06-03：bug 藏在 Horner 組合順序）。
2. **capture 工具 = Rust one-off bin，落 `crates/volarb-sui/src/bin/capture_fixtures.rs`**。sui SDK 依賴隔離在介接層（DAG 合法），`volarb-pricing` 維持零新依賴。fixtures JSON 進 git。
3. **basis 量測 = 一次性報告（不進 CI）**。basis 是 model 差異的事實、是 router edge-buffer 設計的 input，不是 regression invariant。等 router 設計時再決定是否固化成 bound。

## 2. 元件

```
crates/volarb-sui/src/bin/capture_fixtures.rs   ← 一次性 capture 工具（gRPC devInspect, testnet）
crates/volarb-pricing/tests/fixtures/*.json     ← frozen fixtures（進 git；pkg Immutable 故永久有效）
crates/volarb-pricing/tests/onchain_parity.rs   ← parity harness（純離線，CI 跑）
crates/volarb-pricing/examples/measure_basis.rs ← basis 報告工具（不進 CI）
docs/specs/2026-06-XX-l0-parity-basis-findings.md ← basis 結論（capture 後產出）
```

對象 package：`0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138`（testnet，Immutable）。

## 3. Capture 設計（跑一次）

### 3.1 math 層 sweep

對下列純函式各掃 ~50–100 點，devInspect 餵任意參數：

- `math::ln(u64) -> I64`、`math::exp(&I64) -> u64`、`math::sqrt(u64, u64) -> u64`、`math::normal_cdf(&I64) -> u64`
- `i64::mul_scaled`、`i64::div_scaled`、`i64::square_scaled`

點選擇：

- **邊界值**：0、1、SCALE(1e9)±1、normal_cdf regime A/B 切換點（B-break ≈ 5.656854249e9）與 saturation 邊界、exp 2^k range-reduction 邊界、ln normalize 邊界、各函式 abort 邊界（LnZero、ExpOverflow、MagnitudeOverflow、cast-back 超界）。
- **偽隨機點**：固定 seed deterministic 產生（capture 可重現）。
- **abort case 也是 fixture**：記 `expect_abort: <code>`，parity 驗 Rust 端回對應 `OnchainError`（Move abort / Rust Err 邊界對齊是 Part 1 的 load-bearing 設計）。

### 3.2 e2e 層（compute_price / binary_price_pair）

- **OracleSVI 枚舉（sui-architect F1）**：`OracleSVI` 是 shared object，無法用 `getOwnedObjects` 列舉。發現路徑優先序：① `registry` module 的 accessor（pkg 自帶 `registry` 模組，capture 前先拉其 ABI 確認有無 oracle 列表）② 掃近期 `OracleSVIUpdated` events 取 object id ③ Mysten predict-server 訂閱端點。capture bin 實作 ① + ② 其一即可。
- 抓 capture 當下 testnet 所有 live `OracleSVI` objects，**完整欄位 snapshot 進 fixture**（expiry、spot、forward、a、b、rho、m、sigma、timestamp、settlement_price）。
- 每個 oracle × ~10 strikes：ATM 附近階梯 + deep ITM/OTM 極端 + settled tie（若有 settled oracle，驗 strict `>` ties-DOWN）。
- devInspect `oracle::compute_price` 與 `binary_price_pair`，記回傳 u64。
- **Clock 語意（sui-architect F3）**：findings doc 顯示 `binary_price_pair(oracle, K, clock)` 的 clock 在數學上未使用（= compute_price + complement），但 lessons 2026-06-03 規定不可信任二手 doc — **capture 前重新 disassemble `oracle.mv` 確認 Clock 沒有 gating（如 expiry assert / staleness check）**。若有 gating：fixture 只收 `compute_price`，`binary_price_pair` 由 Part 1 unit test（complement 恆等式）覆蓋即可。

### 3.3 傳輸通道

- **通道判準（sui-architect F2）**：capture 需要 **per-command BCS return values**（每個 moveCall 的回傳值）。JSON-RPC `devInspectTransactionBlock` 的 `results[].returnValues` 確定支援；gRPC `SimulateTransaction` 是否暴露 command-level outputs 需 capture 時實測。**判準 = 哪個通道給得出 per-command return values 就用哪個**；fixture 是 frozen 產物，capture 通道不影響 production 路徑（production 訂閱仍 gRPC-only）。
- **批次（F4）**：sweep 把多個 moveCall 打包進同一 PTB（單 PTB 上限 ~1024 commands）減少 round trip；command 順序 deterministic（與 seed 對應），fixture 可重現。
- **Provenance（F5）**：fixture metadata 記 chain id、pkg id、protocol version、capture timestamp、通道（gRPC/JSON-RPC）。

## 4. Parity harness（離線，進 CI）

- 讀 fixtures JSON → 每筆：Rust port 計算 → `assert_eq!` 鏈上 u64，**bit-exact、零 tolerance**。
- abort fixture → assert Rust 回對應 `OnchainError` variant。
- mismatch 時印輸入 + 兩邊輸出（dec + hex），供逐 opcode debug。
- fixtures 缺失（檔案不存在）→ test **fail loud**（不 skip silent），但以 feature/env gate 允許 Part 2 落地前的中間 commit 綠燈：harness merge 時 fixtures 必須同 PR 進來，不留長期 gate。

## 5. Basis 報告

- 對 e2e fixtures 每個 (oracle, strike)：
  - L0 price = 鏈上真值（= Rust port 輸出，parity 已證相等）。
  - L1 price = `binary_price`（float BS digital）：raw w → annualized σ（`σ = sqrt(w / T)`，T 由 expiry − capture timestamp）→ float 路徑。
- 輸出 max / mean / p99 basis，單位 tick（1e-9）。
- 結論寫 findings doc：basis 分佈 + 對 router edge buffer 的建議數字。

## 6. 錯誤處理 / 風險

| 風險 | 對策 |
|------|------|
| capture 當下 testnet 無 live OracleSVI（市場空窗） | math sweep 不受影響照做；e2e 改天再 capture（無公開 oracle constructor，無法合成）。loud 標註 e2e 缺口。 |
| parity 紅 = port bug | systematic-debugging 逐 opcode 對 bytecode，**不改 fixture**（fixture 是 ground truth）。 |
| devInspect 對 pure fn 餵 `&I64` 參數的 BCS 編碼 | I64 是 `{magnitude: u64, is_negative: bool}` struct，PTB 以 `i64::from_parts` moveCall 先建再餵（避免手刻 BCS）。 |
| settled oracle 不存在 | settled 分支已有 Part 1 unit test（strict `>`）；e2e 缺 settled 樣本則 loud 標註。 |

## 7. 成功標準

1. math sweep + e2e fixtures 全 bit-exact 綠（含 abort 對齊）。
2. basis 數字落 findings doc。
3. `cargo test --workspace`、`cargo clippy --workspace -D warnings`、`cargo fmt --check` 全綠。
4. fixtures + harness 同 PR 進 main，capture bin 可重跑（deterministic seed）。

## 8. Non-goals

- 不做 live devInspect 常態比對（拍板 = frozen fixtures）。
- 不把 basis bound 進 CI。
- 不動 L1（`binary.rs` / `svi_fit.rs`）實作。
