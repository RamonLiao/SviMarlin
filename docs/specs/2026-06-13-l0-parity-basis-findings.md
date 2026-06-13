# L0 Parity + Basis Findings (TODO #5 Plan B Part 2)

> Date: 2026-06-13
> Branch: `feat/l0-parity-harness`
> Spec: `docs/specs/2026-06-11-l0-parity-basis-harness-design.md`
> Plan: `docs/plans/2026-06-12-l0-parity-basis-harness.md`

## TL;DR

- **L0 Rust port is bit-exact vs testnet chain** across 216 math cases + 11 e2e `compute_nd2` cases (zero tolerance, `assert_eq!` on u64). "L0 == chain" is now **proven**, not assumed. (+12 settled cases are a port self-consistency check, not chain parity — see below.)
- **L0-vs-L1 basis is negligible**: max **230.7 ticks** (2.31e-7 of price), mean 39.5, p50 0, p99 230.7 over 11 priced strikes. The chain's fixed-point integer path is effectively clean float at probability scale → **fixed-point truncation is NOT a meaningful edge sink**. The router's edge buffer should be driven by spread / day-count / surface-fit basis (L1 + `pricing_config`), not the L0 integer path.

## What was verified

### Math layer (216 cases, `math_sweep.json`)
Deterministic sweep (LCG seed=42) of every primitive `compute_nd2` depends on, via `sui_devInspectTransactionBlock` per-command return values against pkg `0xf5ea…5138` (Immutable):
- package `math::{ln, exp, sqrt, normal_cdf}`, `i64::{mul_scaled, div_scaled, square_scaled}`
- **DeepBook `math::{mul, div}`** at `0xfb28…6982` — `compute_nd2` calls these (round-down), so `db_mul`/`db_div` are validated against the REAL DeepBook math, not a proxy.
- Boundary + abort coverage: `ln(0)`→abort 0, `sqrt(b=0 / b>SCALE)`, `exp` overflow→abort 1, `mul` cast-back overflow, `div`-by-zero, normal_cdf regime A/B break (5.656854249e9). 7 abort cases assert the port returns the matching `OnchainError`.

### E2E layer (23 cases, `e2e_oracles.json`)
`oracle::compute_price`/`compute_nd2` are `public(friend)` → **not** devInspect-callable, and no public wrapper returns the raw price (`get_trade_amounts` applies spread+qty). So `compute_nd2` was **mechanically transcribed op-for-op from `oracle.mv` bytecode** (re-disassembled this task) into chained PTBs: every Move primitive runs on the real chain in bytecode order; the 3 native ops (`inner=sq+sig2`, `w=a+b·mag`, `half_w=w/2`) + abort branches run in Rust. The transcription is derived from bytecode independently of `onchain.rs`, so a match is a genuine cross-check of composition order (the lessons-2026-06-03 risk), not me-vs-me.

- **1 live non-settled oracle** (`0x10bf…b696`, BTC sub-hour) × 11 strikes (0.5×–2.0× forward, deep ITM/OTM + ATM) → full `compute_nd2` path **bit-exact (true chain parity)**.
- **1 settled oracle** (`0x1370…520d`, settled mid-capture) × 12 strikes incl. `strike == settlement`. ⚠️ The settled branch is `s > K ? 1e9 : 0` with **no chain math**, so both the fixture ground-truth and the port compute the same trivial comparison in Rust — this is a **port self-consistency check** of the strict-`>` tie-break direction (ATM-at-settlement → **DOWN** → 0) on a real chain settlement value, **NOT chain-recomputed parity**. The settled composition is not devInspect-reachable (`compute_price` is `public(friend)` and short-circuits before any callable primitive).

### F-findings (from sui-architect review) resolved
- **F1 (oracle enumeration)**: shared `OracleSVI` discovered via `oracle::OracleSVIUpdated` events (`suix_queryEvents`), snapshotted via `getObject`. Object ids frozen in the capture bin.
- **F2 (channel)**: JSON-RPC `devInspectTransactionBlock` confirmed to return per-command BCS return values; gRPC not needed for capture. Fixtures are frozen → production path unaffected (still gRPC-only).
- **F3 (Clock)**: `binary_price_pair`'s `&Clock` arg is **unused** in its body (verified op-by-op) — zero gating, so `compute_price` needs no Clock and the fixture is timeless.

## Limitations (loud)

- **Only one non-settled oracle was live at capture.** 11 compute_nd2 e2e cases on a single real param set (+ broad math sweep + op-by-op source review). If deeper composition coverage is wanted later, re-run capture when more oracles are live (`cargo run -p volarb-sui --bin capture_fixtures`).
- E2E reconstruction does native add/div off-chain (no Move opcode is individually callable); these are exact u64 ops, identical to Move's native semantics for the (small) values involved.
- Basis measured = L0(integer) vs L1(float) **off the same raw `w`** → it isolates fixed-point truncation. It does **not** include SVI-fit error, day-count, or `pricing_config` spread — those are separate, larger basis sources handled at the router (#6/#7).

## Router note (input for #6/#7)

Fixed-point basis ≤ ~231 ticks (2.3e-7) is **below any realistic tick/spread granularity** — do not budget edge for it. Budget for: executable spread (`pricing_config::base_spread/min_spread`, fair ≠ executable), SVI-fit residual, and cross-venue day-count alignment.

## Reproduce

```
cargo run -p volarb-sui --bin capture_fixtures   # re-capture (needs testnet); deterministic seed
cargo test -p volarb-pricing --test onchain_parity   # bit-exact parity (offline, CI)
cargo run -p volarb-pricing --example measure_basis   # basis report (offline, not CI)
```
