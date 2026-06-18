# ADR-005 — Self-Built Postgres Indexer (alongside Mysten Feed)

> Status: **Accepted** · 2026-05-28
> Scope: `crates/volarb-indexer` — data persistence, backtest substrate, audit trail
> Stakeholders: backend, quant (backtests), v2 vault LPs (NAV audit)
> Related: system spec §7 (Data Layer), ADR-001 (NAV audit trail requires queryable history)

---

## 1. Context

Three categories of data need persistence across the system:

1. **Sui chain events** — `OracleSVIUpdated`, `predict::*` events, `volarb::events::ArbIntent`
2. **External venue quotes** — second-by-second bid/ask from each adapter
3. **Bot internal state** — arb-cycle history, risk decisions, NAV attestations (v2)

Two viable strategies:

- **Strategy A — Lean on Mysten official feed only**: subscribe to `predict-server.testnet.mystenlabs.com` for Sui events; venue data in-memory only, no historical store.
- **Strategy B — Self-built Postgres indexer**: persist everything to our own Postgres, including Sui events (cached/mirrored from Mysten feed) and venue quotes.

Strategy A is simpler. Strategy B is more work but unlocks core product features. This ADR picks B and explains why.

---

## 2. Decision

**Self-built Postgres indexer is on the MVP critical path.** Mysten feed remains the primary live source for Sui events; the indexer is a downstream consumer that also persists venue quotes and bot state.

| Source | Live source (real-time) | Persisted to local Postgres? | Retention |
|---|---|---|---|
| Sui `OracleSVIUpdated` | Mysten gRPC feed | Yes (mirror) | 90 days (MVP), forever post-v1 |
| Sui `predict::*` events | Mysten feed | Yes | 90 days |
| `volarb::events::ArbIntent` | Sui fullnode gRPC | Yes | Forever |
| HL HIP-4 quotes | HL WS | Yes (1s snapshots + raw frames for backtest) | 30 days raw, 1 year aggregated |
| Limitless quotes | LM WS | Yes | Same |
| v1 venue quotes | Per adapter | Yes | Same |
| Arb-cycle records | Internal | Yes | Forever |
| Risk events | Internal | Yes | 1 year |
| NAV attestations (v2) | On-chain | Yes (with Walrus blob ref) | Forever |

Postgres v16 hosted on Neon (Vercel Marketplace) for MVP; self-hosted Postgres on Fly.io for v1+ when data volume justifies dedicated infra.

---

## 3. Rationale

### 3.1 Why we can't skip the indexer

Four product requirements force the build:

**(a) Backtest replay (BUSINESS_SPEC §7 MVP requirement, system spec §7.3)**
The hackathon demo includes a 2-week backtest with measurable Sharpe / max DD. Mysten's feed retention is not guaranteed to cover historical windows reproducibly. We must own the snapshot.

**(b) Per-venue historical quotes**
Mysten doesn't index Hyperliquid or Limitless (they're not on Sui). Backtests need joinable history of `(svi_at_t, venue_quote_at_t)` — only achievable with a unified store.

**(c) v2 NAV audit trail (ADR-001 §3.1)**
LPs verifying NAV must be able to query "all attestations + raw input snapshots referenced by NAV at time T." This requires Walrus blob references stored alongside attestation rows. Mysten feed doesn't carry Walrus pointers; we add them.

**(d) Per-cycle P&L attribution**
v2 vault performance-fee accrual requires deterministic, queryable per-cycle P&L. Indexer's `arb_cycles` table is the source of truth that fee math reads.

Skipping the indexer means re-architecting all four of these later. The cost is paid once now or many times later.

### 3.2 Why Postgres specifically?

| Property | Postgres | ClickHouse | TimescaleDB | KV store (e.g., FoundationDB) |
|---|---|---|---|---|
| Time-series query perf | Good with proper indexes | Excellent | Excellent (hypertables) | Poor for range scans |
| Transactional consistency for cycle writes | ACID | Eventual | ACID | Configurable |
| Operator familiarity | Universal | Specialized | Niche | Specialized |
| Hosted option (Neon) on Vercel | Yes, native | No marketplace yet | Limited | No |
| JSON-B for `raw_params` payloads | First-class | Yes | Yes | n/a |
| Migration tooling (`sqlx migrate`) | Excellent | Decent | Decent | Custom |

Postgres wins on the ACID side (cycle writes must be transactional — see ADR-004 unwind state machine) and on hosting simplicity. We accept slightly weaker time-series perf in exchange for one storage engine to operate.

If venue quote volume becomes a perf issue (>10M rows/day per venue), v2 ships a TimescaleDB hypertable just for `venue_quotes`; everything else stays in Postgres. **Architecture not blocked by initial Postgres choice.**

### 3.3 Why Neon (Vercel Marketplace) for MVP, not self-hosted?

| Property | Neon | Self-hosted Fly.io PG |
|---|---|---|
| Time to first row in CI | <5 min (auto-provisioned env var) | 1-2 hours (Dockerfile, volumes, backup) |
| Branch DB per PR | Native | Manual |
| Cost at MVP scale (~5GB) | $0-19/mo | $5/mo + ops time |
| Connection pooling | Built-in | Need pgBouncer |
| Backup / PITR | Included | Manual |
| Migration to self-host later | `pg_dump → restore` | n/a |

MVP value of "doesn't distract from product work" >> any future migration cost. Switch to self-hosted at v1 only if Neon's costs or limits bite.

### 3.4 Why mirror Mysten feed into local Postgres instead of querying it on-demand?

| Approach | Mirror (chosen) | Query on demand |
|---|---|---|
| Backtest latency | <100ms over local index | 200-500ms per query × N strikes |
| Mysten outage resilience | Works offline | Hard dep |
| Schema control | Ours | Theirs (changes when Mysten changes) |
| Join with venue data | Native SQL JOIN | Application-layer join, slow |
| Cost | One ingest stream + storage | Many queries × bandwidth |

Cost of mirror: one WS subscription + ~50MB/day of storage. Trivially cheap.

### 3.5 Why include `risk_events` table?

Every gate decision (approved, rejected, why) is persisted. Why bloat the DB?

- **Demo proof**: show judges the table after the staged feeder-lag injection — "every kill-switch decision is auditable"
- **Backtest fidelity**: replay must reproduce identical gate decisions
- **Post-mortem ops**: when a real production cycle aborts, the row tells operator exactly which gate fired
- **Regulatory readiness (v2)**: vault LPs and auditors expect this

Cost: ~100 rows/cycle, negligible.

### 3.6 Why retention asymmetry (raw frames 30 days, aggregated 1 year)?

Raw WS frames support **adversarial replay** (recreate exact venue conditions for fuzz testing). Useful but bulky (~5GB/venue/month). 30 days is the smallest window that covers a hackathon iteration cycle + 1 retro. Aggregated 1s snapshots are 1000× smaller and serve backtest needs for a year.

---

## 4. Alternatives Considered

### 4.1 No persistence, everything in-memory

Rejected: backtests impossible, recovery from process crash impossible, NAV audit impossible.

### 4.2 Lean entirely on Mysten feed + venue WS replay APIs

Rejected: HL and Limitless replay APIs are spotty; testnet replay is even worse. Self-recording is more reliable.

### 4.3 SQLite for MVP, "upgrade later"

Tempting for hackathon simplicity. Rejected because:
- Concurrent writers (Pricing ingester + Executor + dashboard reader) hit SQLite's writer-lock model hard
- Backtest replay queries are CPU-heavy; Postgres planner is far better
- Switching SQLite → Postgres later is a real migration project

### 4.4 Event-sourcing with an append-only log (e.g., Kafka)

Architecturally cleaner. Rejected for hackathon: deploying Kafka is more ops than the project's whole infra budget. Postgres + JSON-B `raw_params` columns give 80% of event-sourcing benefits with 10% of the ops.

### 4.5 Sui's own GraphQL beta as primary read path

Considered for MVP read-side. Rejected because GraphQL is still beta on v1.72.2 and our event volume might exceed beta SLAs. Use it for dashboard ad-hoc queries (read-once, low-frequency) but not for the bot's hot path.

---

## 5. Consequences

### Positive
- Backtest, NAV audit, P&L attribution, ops post-mortem all unblocked from day 1
- Indexer schema becomes the cross-cutting integration point — Router/Executor/Dashboard all read it, no point-to-point coupling
- Schema is ours; we can evolve it without waiting on Mysten

### Negative
- Engineering cost: ~5 eng-days (one of the larger MVP modules, per system spec §11)
- Ops cost: backups, schema migrations, monitoring
- Schema drift risk between Mysten feed and our mirror — mitigated by integration test that compares 1k random events from each source weekly

### Neutral
- Adaptive Concurrency feature (Sui v1.72.2) used for the live processor; `ConcurrencyConfig` replaces deprecated `Processor::FANOUT` (system spec §7.2).

---

## 6. Open Follow-ups

1. **Schema review** before week 2: lock the `arb_cycles` and `nav_attestations` table designs because they're the most expensive to migrate later.
2. **Mysten feed schema-change alerting**: detect when Mysten ships a new event field; fail loud rather than silently miss data.
3. **Backup strategy** at v1 mainnet: WAL archiving to S3, PITR with 7-day window minimum.
4. **GDPR-equivalent data handling**: if v1 onboards EU LPs, indexer must support data deletion requests. Plan a `pii_data` separate schema with explicit lifecycle.
5. **Walrus integration point** (v2): NAV attestation row references Walrus blob ID; need to confirm Walrus retention SLA matches our forever-retention promise.

---

## 7. Version History

| Date | Author | Change |
|---|---|---|
| 2026-05-28 | team + Claude/Opus 4.7 | Initial. Self-built Postgres on Neon for MVP, self-host path documented. |
