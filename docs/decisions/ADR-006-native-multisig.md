# ADR-006 — Sui Native Multisig for UpgradeCap & Governance

> Status: **Accepted** · 2026-05-29
> Scope: All UpgradeCaps (`volarb::events`, v2 vault package), v2 vault governance signer set, ADR-001 dispute arbitration signer set
> Stakeholders: ops, governance, audit
> Related: ADR-001 (dispute arbitration uses same signer set), system spec §12.3

---

## 1. Context

The Vol-Arb Bot project needs multisig authority in three places:

1. **UpgradeCap holders** for every published Move package (`volarb::events` MVP, plus v2 vault's 4 modules).
2. **Governance multisig** that signs vault parameter updates, PCR whitelist changes, and timelock-bypass emergency overrides.
3. **Dispute arbitration multisig** that resolves challenged NAV attestations during the 12h window (ADR-001 §4).

Two ways to implement multisig on Sui:

- **Native multisig** (`sui::multisig`) — first-class Sui feature since 2024. A multisig address is computed from `(threshold, [(public_key, weight), ...])`. Any transaction signed by a satisfying signature combination is accepted by validators. Zero Move code involved.
- **Custom Move multisig module** — write `MultisigCommittee`, `Proposal`, `Vote` structs; on-chain signature aggregation; explicit `execute_proposal(proposal, sigs)`.

This ADR locks the choice across all three use cases.

---

## 2. Decision

**Use Sui native multisig (`sui::multisig`) everywhere.** No custom Move multisig module.

| Use case | Address type | Threshold / weights |
|---|---|---|
| `volarb::events` UpgradeCap holder | Native multisig | 2 / 3 (team only: lead, co-lead, ops) |
| v2 vault package UpgradeCap holder | Native multisig | 4 / 5 (team×2, community×1, auditor×1, reserve×1) |
| v2 governance multisig (parameter updates, PCR whitelist) | Same address as UpgradeCap holder | 4 / 5 |
| v2 dispute arbitration | Same address (ADR-001 §4) | 4 / 5; emergency timelock-bypass = 5 / 5 |

All four use the same `MystenLabs/sui` ≥ 1.30 native multisig spec. Public keys (Ed25519) collected at v2 launch; published in `docs/governance/multisig.md`.

---

## 3. Rationale

### 3.1 Why native over custom Move?

| Property | Native multisig | Custom Move module |
|---|---|---|
| LOC to maintain | 0 | ~800 |
| Audit surface | Already audited by Mysten | New attack surface (signature aggregation, replay, reentrancy on proposal execution) |
| Gas cost per signed tx | Same as a single-sig (signature check at validator level) | Higher (storage reads for proposal, vote tally, struct dispatch) |
| Threshold change | Re-publish multisig address | Move call + on-chain vote |
| Composability | Any tool that accepts a Sui address works (CLI, wallets, explorers) | Wallets need custom UI |
| Hardware-wallet support | Native (Ledger signs as one of the keys) | Hard — requires custom flow |
| Recovery if one key lost | Standard k-of-n math (e.g. 4/5 keeps working after 1 lost) | Same, but recovery flow needs to be written in Move and audited |

Native multisig has been **the recommended pattern since Sui v1.30**. Multiple top-protocol audits (Cetus, Suilend, Navi) explicitly call out custom Move multisig as "unnecessary risk" when native is sufficient.

The only reason to write custom Move multisig is when on-chain proposal *state* must be visible to other Move code (e.g., DAO governance with on-chain proposal feeds, automated execution triggers). Our use cases don't need that — every multisig action is operator-initiated.

### 3.2 Why the same address for governance + dispute arbitration?

Operationally simpler:
- One signer set to coordinate (less calendar Tetris when keys rotate)
- One PagerDuty rotation for emergencies
- One published multisig address for LPs to verify

Risk consideration: a compromise of 4 of the 5 keys breaks both governance and dispute arbitration simultaneously. We accept this because:
- The same compromise on separate signer sets would still be catastrophic in practice (overlapping personnel)
- Diversified sets create governance ambiguity ("who has authority?") which is its own attack surface
- ADR-001 already requires Walrus-pinned attestation snapshots, so a fully compromised multisig still can't silently re-write NAV history

### 3.3 Why 4/5 and not 3/5 for v2?

- 3/5 means a 3-person collusion can drain governance authority. 4/5 raises that bar by ~one order of magnitude (the marginal honest party is much harder to flip).
- 4/5 is still recoverable from a single key loss (4 remaining sign).
- 4/5 is what Compound, Maker, Optimism use for comparable authority. Industry convention is well-calibrated here.

5/5 reserved for emergency timelock-bypass (e.g., live exploit requiring a same-block patch). Higher friction is appropriate for higher-impact actions.

### 3.4 Why 2/3 (not 2/2 or 3/3) for MVP `volarb::events`?

- MVP authority scope is tiny (only the indexer-hook event emitter package).
- 2/2 too brittle (one key on holiday → can't ship a patch).
- 3/3 inverse problem.
- 2/3 is the smallest setup that survives single-key absence + matches v1 SaaS team size.

### 3.5 Why include an external auditor key in v2?

The auditor key is a **break-glass insurance** signer, not an active participant. They sign only:
- Emergency pause when team + community both unreachable
- Final tiebreaker on disputed NAV attestation when team voted 2-2

In normal ops, the auditor never signs. This pattern (institutional-investor-style independent member) is what brings LP trust at v2 mainnet AUM scale.

---

## 4. Alternatives Considered

### 4.1 Custom Move `MultisigCommittee` module

Rejected per §3.1 — high cost, low marginal value.

### 4.2 Per-domain multisig addresses (separate addr for governance vs dispute)

Rejected per §3.2 — operational fragility outweighs theoretical compartmentalization benefit at our scale.

### 4.3 zkLogin-based multisig

Considered for v2 to lower LP friction. Rejected for governance use: zkLogin keys are derived from OAuth providers (Google, Apple), which introduces a centralization vector that breaks the multisig threat model. zkLogin remains the recommended path for **LP deposit UX** (separate concern).

### 4.4 Squads / Hyperware / other off-chain multisig coordinators

These would add an external dependency for routing proposals. Native multisig requires only Sui CLI + a shared spreadsheet for collecting signatures during the bootstrap phase, upgrading to a UI (e.g., Suiet multisig builder) when traffic warrants.

### 4.5 BLS-aggregated threshold signature scheme

Cryptographically elegant (single signature visible on chain regardless of threshold), but no first-class Sui support yet. Revisit when Sui core ships native BLS verifier.

---

## 5. Consequences

### Positive
- Zero Move code to maintain or audit for multisig logic
- Standard tooling: Sui CLI's `client multisig-tx-execute` works out of the box
- All UpgradeCaps and governance actions visible on Sui explorer with standard signature lists
- Future migration path open (can swap to BLS or custom module later by re-`transfer`'ing UpgradeCap)

### Negative
- Signature collection ceremony required for every action (each signer must locally sign + share their sig). Mitigated by Suiet's multisig UI in v1 onwards.
- Threshold change requires republishing the multisig address and re-transferring UpgradeCap. Acceptable; threshold changes should be rare.
- No on-chain proposal history (signatures are off-chain artifacts until the tx is published). Mitigated by maintaining a public `governance-log.md` in the repo, each entry referencing the on-chain tx digest.

### Neutral
- Multisig public keys are visible on every signed tx. This is the standard transparency level expected for protocol governance.

---

## 6. Implementation Notes

### Computing the multisig address (Sui CLI)

```bash
sui keytool multi-sig-address \
  --pks $PK_TEAM_LEAD $PK_COLEAD $PK_OPS $PK_COMMUNITY $PK_AUDITOR \
  --weights 1 1 1 1 1 \
  --threshold 4
```

Output is the address that holds the UpgradeCap. Publish in `docs/governance/multisig.md` with each signer's key fingerprint + role.

### Signing a multisig tx

```bash
# 1. Build the tx (any signer)
sui client publish ...   # or upgrade ...   produces tx-bytes

# 2. Each signer signs locally
sui keytool sign --address $SIGNER_ADDR --data $TX_BYTES_B64

# 3. Combine signatures
sui keytool multi-sig-combine-partial-sig --pks ... --weights ... --threshold 4 --sigs $SIG1 $SIG2 $SIG3 $SIG4

# 4. Execute (any signer)
sui client execute-signed-tx --tx-bytes $TX_BYTES_B64 --signatures $COMBINED_SIG
```

CI publishes signed tx-bytes; signers attach to a public PR; combiner runs on the merge.

---

## 7. Open Follow-ups

1. **Suiet multisig UI integration** — adopt once vault v2 ships and signing volume justifies the dependency.
2. **Hardware wallet inventory** — Ledger app for Sui supports Ed25519; ensure each signer has a hardware key by v1 mainnet.
3. **Key rotation runbook** — written procedure for rotating any one signer's key without losing UpgradeCap access (must be done before any signer leaves).
4. **Governance log** — `docs/governance/governance-log.md` with chronological list of all multisig actions, each row: `(date, tx_digest, action, signer_set, link to PR)`.
5. **Threshold escalation plan** — if vault AUM crosses $10M / $100M, consider raising to 5/7 / 7/9 with broader community participation.

---

## 8. Version History

| Date | Author | Change |
|---|---|---|
| 2026-05-29 | team + Claude/Opus 4.7 | Initial. Native multisig locked across all three use cases. |
