# ADR-007 — Clock Shared Object over TxContext Epoch Timestamp

> Status: **Accepted** · 2026-05-29
> Scope: All on-chain timestamp emission and time-based checks in `volarb::events`, v2 vault, and any future Move module
> Stakeholders: backend (indexer ordering depends on this), v2 vault (timelock + challenge window depend on this)
> Related: System spec §6.1 (`volarb::events`), ADR-001 (12h challenge window timing)

---

## 1. Context

Move code on Sui has two ways to obtain "current time":

1. **`TxContext::epoch_timestamp_ms()`** — returns the **start time of the current epoch**. Same value for every transaction in the epoch (Sui epoch ≈ 24 hours).
2. **`sui::clock::Clock`** — a Sui-system shared object at the fixed address `0x6`. Returns true wall-clock ms updated by validators every checkpoint (~250ms).

A Move developer reaching for "the current timestamp" naturally types `ctx.epoch_timestamp_ms()` because it's available without adding a shared-object dependency. But for any use case more granular than "what day is it," this is wrong.

The architect validation review (2026-05-29) flagged this in the original `volarb::events` design. This ADR records the decision to standardize on `Clock` and explains why.

---

## 2. Decision

**Use `sui::clock::Clock` (id `0x6`) for every timestamp needed in Move code.** Never use `TxContext::epoch_timestamp_ms()` except for the specific case of "which epoch is this" semantics (which we don't currently need).

| Module | Timestamp use | Source |
|---|---|---|
| `volarb::events::emit_arb_intent` | `ArbIntent::timestamp_ms` for indexer ordering | `&Clock` |
| v2 `vol_arb_vault::deposit` / `withdraw` | event timestamps | `&Clock` |
| v2 `vol_arb_vault` challenge window | `pending_nav.posted_at_ms`, `now - posted_at > 12h?` | `&Clock` |
| v2 `vault_admin::execute_after_timelock` | `now - proposed_at > 24h?` | `&Clock` |
| v2 `nautilus_verifier` attestation freshness | `now - attestation.ts < 60s?` | `&Clock` |

Every entry function that emits an event or checks a deadline takes `clock: &Clock` as a parameter. PTBs supply `tx.object("0x6")`.

---

## 3. Rationale

### 3.1 Why `epoch_timestamp_ms()` is wrong for our use cases

Sui epochs are ~24 hours. `epoch_timestamp_ms()` returns the epoch's **start time** — every single tx in that 24h window gets the **identical** timestamp.

Concrete failures this would cause:

- **`volarb::events::ArbIntent`** — indexer orders cycles by `timestamp_ms`. If every cycle in a day has the same ts, ordering collapses to "insert order in Postgres", which is non-deterministic under concurrent writers and breaks backtest reproducibility.
- **v2 challenge window** — `now - posted_at > 12h` would round to "did the epoch change?" Twelve-hour challenge windows become "until next epoch boundary," varying from 0h to 24h depending on when the attestation was posted.
- **v2 timelock** — same problem. 24h timelock becomes "next epoch ≥ posted_epoch + 1," which is also actually ~24h *but* with no granularity to detect operator gaming (e.g., post proposal at epoch-end to make effective timelock = 30 minutes).

### 3.2 Why `Clock` is the right answer

- **True ms precision**: updated every checkpoint (~250ms) by Sui consensus.
- **Trust model identical to chain consensus**: the Clock value is part of the consensus state; faking it requires breaking Sui itself.
- **Standard pattern**: `0x2::clock::Clock` is the canonical Sui time source. Every protocol that needs precise time (DeepBook, Cetus, Suilend) uses it. Audit reviewers expect it.
- **Move 2024 ergonomics**: `clock.timestamp_ms()` method syntax is concise.

### 3.3 Why not just declare a custom `TimeOracle`?

Tempting (other chains do this), but Sui already shipped the correct primitive. Adding a layer adds:
- Trust assumption on whoever updates the oracle
- Gas cost
- Audit surface
- Confusion for future readers

Use the platform's native answer.

### 3.4 Cost of taking `&Clock` everywhere

- Adds one shared-object reference to every PTB that emits a timestamped event. Trivial.
- PTB construction (TS side): `tx.object("0x6")`. One line.
- Adapter writers must remember to include Clock in any custom entry function. Caught by code review + the `move-code-quality` skill checklist.

This is the price of correctness. Negligible.

---

## 4. Alternatives Considered

### 4.1 `TxContext::epoch_timestamp_ms()`

Wrong for our use cases (§3.1). Acceptable only if granularity = "which day."

### 4.2 Off-chain timestamps injected via PTB pure argument

Approach: PTB caller signs `now_ms` and passes it as a `u64` argument; Move trusts it. Rejected: any user can lie about `now_ms`. Defeats the purpose of on-chain time.

### 4.3 Custom oracle-published `TimeObject`

Adds operator trust without adding capability. Rejected.

### 4.4 Block / checkpoint number as proxy for time

Sui Mysticeti has variable checkpoint cadence; checkpoint count is not a reliable wall-clock proxy. Rejected.

### 4.5 Hybrid: `Clock` for v2 vault, `epoch` for `volarb::events`

Considered for "simplicity" of MVP. Rejected because the indexer-ordering failure mode (§3.1) bites even at MVP scale, and the cost of using `Clock` is one extra `tx.object("0x6")` per PTB.

---

## 5. Consequences

### Positive
- Indexer ordering is deterministic across the entire history.
- v2 timelock / challenge window math is sound.
- Pattern matches every audited Sui protocol — auditors won't flag it.

### Negative
- Every PTB that calls a timestamp-emitting function must include the Clock object. PTB builders (`volarb-sui` crate) must remember.
- Future Move code reviewer needs to enforce "never `ctx.epoch_timestamp_ms()` unless you really mean it". Add to the project's Move code-quality checklist.

### Neutral
- Move test setup uses `clock::create_for_testing(ctx)` to inject a controllable Clock in unit tests. Standard pattern.

---

## 6. Code Quality Checklist Entry

Add to `move-code-quality` rules for this project:

> **MQ-031** — Timestamp source must be `sui::clock::Clock`, not `TxContext::epoch_timestamp_ms()`. The latter returns epoch start time (~24h resolution) and is rarely what callers actually want. Exception: literal "which epoch is this" semantics, which must be commented explicitly.

---

## 7. Open Follow-ups

1. **Move lint rule** — write a `clippy`-equivalent lint that warns on every `epoch_timestamp_ms` call in `*.move` files. Block merge unless suppressed with `// allow(epoch-timestamp): <reason>`.
2. **Test fixtures** — provide `volarb_test_utils::clock(starting_at_ms)` helper for Move unit tests.
3. **Indexer schema** — when ingesting `ArbIntent` events, confirm `timestamp_ms` aligns with the chain's clock checkpoint timestamps (sanity check, should always pass).

---

## 8. Version History

| Date | Author | Change |
|---|---|---|
| 2026-05-29 | team + Claude/Opus 4.7 | Initial. Locked `Clock` as the only valid time source. |
