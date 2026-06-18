# Architecture Decision Records

每個重大架構決策一份檔。格式遵循 Michael Nygard 的 ADR 慣例（Title / Status / Context / Decision / Rationale / Alternatives / Consequences / Open follow-ups）。

| # | Title | Status | Affects |
|---|---|---|---|
| [001](./ADR-001-nav-oracle.md) | Vault NAV Oracle (Nautilus TEE + 12h challenge window + fungible Coin share) | Accepted | v2 vault |
| [002](./ADR-002-rust-core-engine.md) | Off-chain core engine in Rust + Sui SDK strategy | Accepted | Whole off-chain stack |
| [003](./ADR-003-venue-adapter-trait.md) | VenueAdapter trait design (6 methods + 2 metadata) | Accepted | `crates/volarb-venues` |
| [004](./ADR-004-unwind-window.md) | Cross-chain leg unwind window (5s soft / 30s hard, external-first) | Accepted | `crates/volarb-executor` |
| [005](./ADR-005-indexer-strategy.md) | Self-built Postgres indexer (alongside Mysten feed) | Accepted | `crates/volarb-indexer` |
| [006](./ADR-006-native-multisig.md) | Sui native multisig for UpgradeCap & governance (no custom Move multisig) | Accepted | UpgradeCaps, v2 governance, ADR-001 arbitration |
| [007](./ADR-007-clock-vs-epoch-timestamp.md) | `sui::clock::Clock` over `TxContext::epoch_timestamp_ms()` for all on-chain timestamps | Accepted | All Move modules |

## How to add a new ADR

1. Copy the template structure from any existing ADR
2. Number sequentially (`ADR-006-...`)
3. Status starts as `Proposed`; flips to `Accepted` only when consensus reached
4. Never edit accepted ADRs in place — instead, write a new ADR that **supersedes** it (mark old one `Superseded by ADR-XXX`)

## Why ADRs?

Spec describes the system as it stands. ADRs describe *why* it stands that way. Six months from now when someone asks "why Rust not TS?", the answer is `ADR-002`, not a Slack archaeology dig.
