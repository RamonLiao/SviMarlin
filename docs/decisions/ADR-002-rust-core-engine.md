# ADR-002 — Off-Chain Core Engine in Rust + Sui SDK Strategy

> Status: **Accepted** · 2026-05-28
> Scope: All off-chain modules in `crates/volarb-*` (Pricing, Router, Risk, Executor, Venues, Indexer, RPC)
> Stakeholders: backend engineers, quant developers, on-call ops
> Related: ADR-003 (VenueAdapter), ADR-004 (Unwind window), system spec §3

---

## 1. Context

The Vol-Arb Bot off-chain engine must:

1. Subscribe to **Sui `OracleSVIUpdated`** events at high frequency and fit SVI surfaces in single-digit milliseconds.
2. Maintain **N parallel WebSocket connections** to external venues (Hyperliquid, Limitless, future v1 venues) with sub-100ms quote-to-decision latency.
3. Execute **atomic 2-leg cross-chain trades** with a persistent state machine that survives process crashes.
4. Run the **Nautilus NAV oracle (v2)** inside an AWS Nitro Enclave, which mandates a minimal-deps, reproducible-build language.
5. Expose a **gRPC API** to the Next.js dashboard.

Two viable language choices: **all-Rust core** versus **TypeScript core** (Node.js with `@mysten/sui`).

The choice has lock-in effect on hiring, build pipeline, enclave compatibility, and library ecosystem. Worth an ADR.

---

## 2. Decision

**Off-chain engine in Rust. Dashboard in TypeScript. gRPC between them.**

| Layer | Language | Primary SDK |
|---|---|---|
| Pricing / Router / Risk / Executor | Rust 1.78+ | `sui-sdk` (Mysten crates.io) for Sui, custom `reqwest` + `tokio-tungstenite` for venues |
| Sui PTB builder | Rust | `sui-sdk` + `sui-transaction-builder` |
| NAV oracle (v2, in-enclave) | Rust | minimal: `sui-sdk` + `serde` only |
| Indexer writer | Rust | `sqlx` |
| gRPC server | Rust | `tonic` |
| Dashboard | TypeScript | `@mysten/sui`, `@mysten/dapp-kit-react` |

**Fallback for Move ABI gaps**: if `sui-sdk` Rust crate lags `@mysten/sui` for a specific Predict/DeepBook binding, we generate TypeScript PTBs via a sidecar Node process and call into it via Unix socket. **Not** FFI — sidecar keeps the boundary process-clean.

---

## 3. Rationale

### 3.1 Why Rust for the core?

| Property | Rust | TypeScript |
|---|---|---|
| SVI fitting throughput | ~50µs per fit (nlopt + ndarray) | ~2ms (numjs or pure JS) — **40× slower** |
| Tail latency under load (p99) | <2ms | 20-100ms (GC pauses) |
| Memory safety for long-running daemon | Compile-time guarantees | Runtime, with `process.on('uncaughtException')` band-aids |
| Concurrency model | `tokio` async, structured concurrency | Single-thread event loop, worker_threads if forced |
| Nautilus enclave compatibility | First-class (Mysten official enclave example is Rust) | Possible but unproven; Node runtime is 80MB+ overhead |
| Type system for trait-based architecture (`VenueAdapter`) | Powerful, enforced | Structural, runtime-erased |
| Refactor safety on 15k+ LOC core | High (cargo check catches everything) | Low (TS catches ~70% before runtime) |

The deciding factor is **enclave compatibility**. v2 vault NAV is the project's mainnet-day-one moat (ADR-001). Choosing TS for the core would require either rewriting NAV oracle in Rust later (~8 eng-days, risk of divergence) or shipping NAV in Node-on-Nitro (unproven, larger attack surface, slower attestation).

### 3.2 Why TypeScript for the dashboard?

| Property | TypeScript | Rust (Leptos/Yew) |
|---|---|---|
| Hiring pool | Very large | Small |
| Next.js / shadcn / Tailwind ecosystem | First-class | Adapter shims needed |
| `@mysten/dapp-kit-react` integration | Native | Custom binding required |
| Time-to-first-feature for hackathon | Hours | Days |
| Build pipeline complexity (Vercel) | One command | Custom |

Dashboard requirements (wallet connect, IV plotting, time-series charts, deposit UX) are precisely what the JS ecosystem solves. Rust wins on the hot path; TS wins on the UI. **Use each for what it's good at, communicate via gRPC.**

### 3.3 Why gRPC between Rust core and TS dashboard?

- **Schema-first contract**: `.proto` files become the API spec, both sides codegen
- **Streaming**: live tape, IV surface updates, position events — gRPC-Web handles all three natively
- **Bidi**: dashboard can push config updates (strategy params) without REST round-trips
- **Versioning**: protobuf field numbers prevent silent breaks

Alternative considered: REST + Server-Sent Events. Rejected because streaming guarantees are weaker and we get no codegen.

### 3.4 Why `sui-sdk` Rust crate and not FFI to `@mysten/sui`?

| Approach | Pros | Cons |
|---|---|---|
| **`sui-sdk` Rust (chosen)** | Native types, zero copy, enclave-friendly, no Node in deployment | Lags `@mysten/sui` for new Move framework features by weeks |
| FFI to Node + `@mysten/sui` | Always up-to-date | Two runtimes in one process, GC pauses, enclave incompatible |
| Sidecar Node process (fallback) | Up-to-date, process-isolated | Extra IPC hop (~1-2ms), deployment complexity |

Plan A: native Rust SDK. Plan B (only if Predict bindings lag): sidecar Node for PTB construction, Rust still owns Risk/Router/Executor.

---

## 4. Alternatives Considered

### 4.1 All-TypeScript (Node + Bun)

Rejected because:
- Enclave story unproven for v2 (see §3.1)
- SVI math throughput insufficient for sub-hour expiry granularity (HL HIP-4 15m binaries)
- Tail latency under WS load is bad for cross-chain race conditions in Executor

### 4.2 Go for core

Rejected because:
- `sui-sdk-go` (community) lags official Rust crate significantly as of 2026-05
- Goroutines great but no structured concurrency parity with `tokio`
- Smaller pool of cross-chain DeFi infra written in Go (most precedent is Rust)

### 4.3 Hybrid: Rust for hot path + Python for backtest replay

Initially considered for the quant team's familiarity. Rejected because:
- Replay engine needs the **same** Router/Risk/Executor code as live engine for determinism
- Maintaining parity between two implementations is the #1 source of backtest-vs-live divergence in the industry
- Python kept as ad-hoc analysis tool (Jupyter notebooks read indexer Postgres), not in the live path

### 4.4 Zig for the enclave-side oracle only

Tempting (small binary, no GC) but immature ecosystem; sui-sdk-zig doesn't exist. Revisit in 2027.

---

## 5. Consequences

### Positive
- Single language for core enables shared types across Pricing → Risk → Executor (one Cargo workspace)
- v2 NAV oracle reuses ~60% of MVP code (position-reading, fee math) instead of being a separate codebase
- Sub-millisecond latency budget achievable; opens future HFT-style strategies
- Crate-boundary enforced architecture (system spec §3.1)

### Negative
- Smaller hiring pool than TS. Mitigated by clear `crates/` boundary so contributors can work on one crate without owning the whole engine.
- Slower iteration on Move ABI changes when `sui-sdk` Rust lags. Mitigated by sidecar-Node fallback (§3.4).
- Backtest replay HTML reports require an extra "Rust → JSON → JS render" step. Acceptable.

### Neutral
- Rust learning curve absorbed in week 1-2 of hackathon; quant team already familiar from prior Deribit MM work.

---

## 6. Open Follow-ups

1. **`sui-sdk` Predict bindings audit** (week 1): confirm `predict::mint` / `predict::redeem` are reachable. If not, write the type bindings ourselves and upstream PR.
2. **Cargo workspace lockfile policy**: lock to exact versions of `sui-sdk`, `tonic`, `tokio`, `sqlx`. Renovate bot for security patches only.
3. **Enclave-side Cargo.toml minimization**: NAV oracle has a separate Cargo workspace with only the crates that pass reproducible-build validation. Target: `<100MB` enclave image.
4. **gRPC schema review** before week 3: schemas freeze before dashboard work begins to avoid double-edit thrash.

---

## 7. Version History

| Date | Author | Change |
|---|---|---|
| 2026-05-28 | team + Claude/Opus 4.7 | Initial. Rust core + TS dashboard locked. |
