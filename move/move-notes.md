# Move Notes — Vol-Arb Bot

## 2026-05-30 — `volarb::events`（MVP 唯一 Move module）

**目的**：indexer enrichment。venue 側（Hyperliquid/Limitless 等）不發 Sui event，靠這顆 event 讓 off-chain indexer 用 `cycle_id` 把 Sui 腿 tx join 回跨 venue arb cycle。

**修改的 module**：新增 `volarb::events`（`move/volarb/sources/events.move`，~45 LOC）。MVP critical path 唯一自寫 Move；其餘全用既有 `predict::*` / `deepbook::*` / `pyth::*` / `clock::*`。

**設計**：
- `ArbIntent has copy, drop`（非 object，純 BCS event payload，無 UID）。欄位：`cycle_id: String`、`venue_id: String`、`timestamp_ms: u64`。
- `emit_arb_intent(cycle_id, venue_id, clock: &Clock)`：executor Sui 腿 PTB 跟 `predict::mint` 同 PTB 呼叫。
- 時戳用 `clock.timestamp_ms()`（Clock id `0x6`），**不用** `TxContext::epoch_timestamp_ms()`（ADR-007，epoch 起始值非當下）。
- `#[test_only]` accessor（`cycle_id`/`venue_id`/`timestamp_ms`）：欄位私有，test 要驗內容需要。生產 ABI 無這些。

**鏈上限制**：event 是單向 sink，production 無「讀回 event」API；`event::events_by_type<T>()` 僅 test 模式存在。

**測試結果**：`sui move test` → 1 pass。test 驗 (a) 恰好 1 event (b) `timestamp_ms == Clock` 值（ADR-007 regression guard，改回 epoch 會炸） (c) cycle_id/venue_id 正確。

**已知風險 / 待議**：
- `ArbIntent` 命名非過去式（Move Book checklist 建議 event 過去式如 `UserRegistered`）。依 spec §6.1 原樣保留 + 語意是「意圖」非完成動作。若團隊在意一致性，可考慮改 `ArbIntentEmitted`，但要同步 spec §6.1 + indexer schema。

**工具鏈**：sui CLI 1.71.0，edition 2024，framework implicit deps，無 explicit Sui/MoveStdlib。
- `assert_eq!` 在 1.71 需 `use std::unit_test::assert_eq;`（非自動 in-scope）。

**Review**：move-code-quality 過（純 event module，無 auth/金流/access control → sui-red-team 不適用）。
