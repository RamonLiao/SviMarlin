# Threat Model — Vol-Arb Bot

> Living document. Updated when architecture changes or new attack class observed in the wild.
> Companion: [`../specs/2026-05-28-vol-arb-bot-spec.md`](../specs/2026-05-28-vol-arb-bot-spec.md) §13.

---

## STRIDE per asset

| Asset | S (spoof) | T (tamper) | R (repudiate) | I (info disc) | D (DoS) | E (priv esc) |
|---|---|---|---|---|---|---|
| Sui signer key | KMS/enclave bound | KMS audit | Tx history | Memory dump → enclave (v2) | n/a | n/a |
| HL API secret | KMS | KMS audit | HL audit log | Same | n/a | n/a |
| OracleSVI feed | Sui validator set | Validator set | n/a | Public | Sui consensus | n/a |
| Engine RPC | mTLS + auth | TLS | Audit log per call | TLS | Rate-limit | RBAC on dashboard endpoints |
| Indexer DB | Network-isolated | Parameterized queries | Append-only audit table | Encrypted at rest | Connection pool + readreplicas | DB role least-priv |
| Cross-chain leg | n/a | State machine persistence | Per-cycle indexer row | n/a | 5s timeout + 30s unwind | n/a |
| v2 NAV attestation | PCR pinning | Enclave-signed | On-chain attestation history | Walrus blob (public) | 60min liveness fallback | Multisig override |
| v2 Vault Move | Capability pattern | Move type safety | Event stream | n/a | Gas limits | AdminCap holder check |
| Dashboard | OAuth (v1) | CSP + Trusted Types | Login audit | TLS only | Vercel WAF | Per-route auth |

---

## Top 10 prioritized threats

| # | Threat | Likelihood | Impact | Detect | Respond |
|---|---|---|---|---|---|
| 1 | Cross-chain leg desync (one leg fills, other rejects) | High | High | Executor state mismatch in indexer | 30s unwind window auto-fires |
| 2 | OracleSVI feeder stalls during volatile period | Medium | High | Watchdog `now − last_update > 60s` | Halt entries; auto-flatten on prolonged |
| 3 | Operator host key exfil (MVP env-var stage) | Medium | Critical | KMS audit (post-v1) | Rotate; v1 moves to KMS |
| 4 | HL adapter rate-limited mid-cycle | Medium | Medium | Adapter `health()` degraded | Switch to Limitless via Router fallback |
| 5 | Pyth divergence (BTC mark drift between Pyth and HL) | Medium | High | Risk gate fires `>1%` divergence | Halt + page operator |
| 6 | v2 enclave tainted RPC input (clean code, dirty data) | Low | Critical | Dispute via independent attestation | 12h challenge window + multisig |
| 7 | Indexer crash during cycle write | Low | High | Heartbeat alert | Cycle resume from last persisted state |
| 8 | Sui PTB lands but predict::mint reverts inside | Low | Medium | Tx effects show revert | Cycle aborted; no partial state |
| 9 | Move module upgrade introduces incompatible storage | Low | Critical | Upgrade compat checker in CI | 24h timelock catches; multisig veto |
| 10 | Dashboard XSS via untrusted dashboard input | Low | Medium | CSP report-uri | Patch + invalidate sessions |

---

## Move-module specific (v2 vault)

Run before any mainnet deployment of `vol_arb_vault`:

1. **Access control bypass** — every public function checks `cap`/`witness` correctly
2. **Reentrancy via outcome token transfer** — Sui's borrow checker prevents but explicit test required
3. **Integer overflow** — share mint with `u64::MAX` deposit, NAV with `u128` intermediate
4. **Object lifecycle** — `Vault<T>` shared correctly, never accidentally `freeze`'d or `transfer`'d
5. **Economic exploit** — deposit/withdraw within same epoch using stale NAV (12h window helps but explicit test)
6. **DoS via large dispute payload** — challenge payload size capped; gas budget bounded
7. **PCR replay** — old valid attestation cannot be replayed after governance updates PCR whitelist

Tooling: `sui-red-team` skill produces attack tests for all 7; `move-code-quality` skill enforces Move Book checklist; `sui-security-guard` scans for leaked secrets in repo.

---

## Out of scope (explicit non-defenses)

- **Validator collusion on Sui consensus**: trusted as base layer.
- **AWS Nitro silicon-level break**: defended only by challenge window + dispute (cannot be prevented).
- **Network-level adversary controlling all RPCs**: defended only by multi-source cross-check (Pyth + Sui native + HL native).
- **MEV on Sui** (front-run of `predict::mint`): future work; Mysticeti reduces opportunity, but no explicit defense in MVP.

---

## Document history

| Date | Author | Change |
|---|---|---|
| 2026-05-28 | team + Claude/Opus 4.7 | Initial threat model. Mirrors spec §13. |
