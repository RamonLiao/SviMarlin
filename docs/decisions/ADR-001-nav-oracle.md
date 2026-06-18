# ADR-001 — Vault NAV Oracle Architecture

> Status: **Accepted** · 2026-05-28
> Scope: v2 `vol_arb_vault` Move package (post-MVP, mainnet-day-one)
> Stakeholders: Vol-Arb Bot team, vault LPs, DeepBook Predict integration partners
> Reviewers (target): Sui Overflow 2026 Track 2 judges

---

## 1. Context

The Vol-Arb Bot v2 ships a tokenised vault that lets LPs deposit USDsui and earn vol-arb yield from positions spread across **three independent chains**:

- **Sui** — DeepBook Predict outcome tokens (`predict::mint` positions)
- **Hyperliquid L1** — HIP-4 Outcome Markets binary positions
- **Base** — Limitless prediction-market positions

No single chain can compute the vault's total Net Asset Value (NAV) on its own. Yet every deposit, withdraw, and performance-fee accrual depends on a correct, up-to-date NAV. This document records how we resolved that.

NAV definition (standard fund accounting):

```
NAV_per_share = (total_assets_across_all_chains − liabilities − accrued_fees) / shares_outstanding
```

---

## 2. Decision

**Architecture: Nautilus TEE-attested NAV oracle + 12-hour challenge window + Fungible Coin vault share.**

| Component | Choice | Default value |
|---|---|---|
| NAV compute environment | AWS Nitro Enclave via Sui Nautilus framework | — |
| NAV update cadence | Configurable; default `5 minutes`; governance can raise/lower based on AUM and market regime | `300_000 ms` |
| Challenge window | Configurable; default `12 hours` between attestation posting and NAV becoming "live" | `43_200_000 ms` |
| Share token | `Coin<VOL_ARB_SHARE>` — fungible, transferable, DEX-listable | — |
| PCR whitelist | Governance-managed list of approved enclave code hashes; only whitelisted PCRs can post NAV | — |
| Dispute mechanism | Any address may post `raise_dispute(alt_attestation)` during window; arbitrated by 4/5 multisig | — |
| Liveness fallback | If no attestation for `>= 60 min`, vault auto-pauses deposits (withdrawals stay open at last-confirmed NAV) | `3_600_000 ms` |

---

## 3. Rationale (judge-facing)

### 3.1 Why Nautilus TEE rather than push-by-operator?

| Approach | Trust assumption | Verifiability | Engineering load |
|---|---|---|---|
| **Operator pushes signed NAV** | Trust the human operator's key + honesty | LPs can only audit after the fact | Low |
| **On-chain pull oracle (per-chain reads)** | Trustless | Cannot read Hyperliquid / Base state from Sui without bridges | Very high (multi-chain light clients) |
| **Nautilus TEE attestation (chosen)** | Trust AWS Nitro hardware + pinned code hash | LPs verify attestation signature + PCR + Walrus snapshot of inputs | Medium |

Nautilus gives us a trust-minimized middle ground: the operator never sees the enclave's private key, and the on-chain verifier rejects any attestation whose code hash doesn't match the governance-pinned PCR. This is the **only realistic answer** for a cross-chain vault on Sui mainnet day-one.

Bonus story value: the project becomes the first known integration to combine **two Sui flagship primitives — DeepBook Predict and Nautilus — in a single product surface.**

### 3.2 Why a challenge window even with TEE?

TEEs are strong but not infallible. The challenge window defends against four documented failure modes:

1. **TEE silicon vulnerabilities** — Intel SGX has shipped 8+ critical CVEs since 2018 (Foreshadow, Plundervolt, ÆPIC Leak, Downfall…). AWS Nitro has had side-channel papers published as recently as 2024. Hardware roots of trust *do* fail.
2. **Tainted inputs** — The enclave code is sealed, but the RPC endpoints it queries (`api.hyperliquid.xyz`, Base JSON-RPC, Sui fullnode) are *not* inside the enclave. A compromised host can MITM these reads, feeding a clean enclave dirty data → a valid attestation over a wrong NAV.
3. **Deployment / PCR drift** — Operator announces PCR=X, governance whitelists X, but a misconfigured deployment ships PCR=Y. Caught at attestation time *only if* the verifier is correctly configured.
4. **Honest bugs** — Off-by-one in fee math, missed venue in the position sweep, stale Pyth feed. Not malice, but identical economic impact.

The challenge window converts NAV oracle from a **single point of trust** into **TEE + human oversight in series**. Industry analogue: Optimism / Arbitrum / Espresso optimistic rollups all run challenge windows for exactly this reason — they trust their fraud proofs, but not enough to skip the window.

### 3.3 Why 12 hours specifically?

| Window | Pros | Cons | Verdict |
|---|---|---|---|
| < 2 h | Fast UX | Community can't review in time | Too short |
| 6 h | Balanced | Insufficient for non-US LPs (timezone) to react | Borderline |
| **12 h** | Industry standard (Aave / Compound governance use comparable windows); covers two business-hour cycles globally | Slight UX cost on deposit | **Chosen** |
| 24 h | Maximum safety | LP frustration on small deposits | Excessive for AUM tier ≤ $10M |
| 48 h+ | Bank-grade safety | Capital efficiency drops sharply | Reserved for AUM > $100M |

The window is **configurable on-chain**, so as AUM scales the team can ratchet it up via governance without redeploying.

### 3.4 Why 5-minute update cadence?

| Cadence | RPC + compute load | Gas/day (Sui mainnet est.) | NAV staleness for deposit/withdraw |
|---|---|---|---|
| 1 min | High (60× /h × 3 chain reads) | ~$25 | < 0.06% drift |
| **5 min** | Moderate | **~$5** | **< 0.3% drift** |
| 15 min | Low | ~$2 | ~1% drift (requires slippage caps) |

5 minutes is the sweet spot for hackathon → early mainnet AUM (<$10M). It is **dynamically adjustable** via governance — the spec explicitly calls out raising cadence to 1 min once HFT-style LPs onboard, or dropping to 15 min during low-volatility regimes to save gas. No redeploy required.

### 3.5 Why Fungible `Coin<VOL_ARB_SHARE>` over soulbound NFT?

| Property | `Coin<VOL_ARB_SHARE>` (chosen) | Soulbound NFT |
|---|---|---|
| Secondary market liquidity | Yes (Cetus, DeepBook spot pool) | No |
| Usable as collateral in other Sui protocols | Yes (Suilend, Navi, Scallop integration possible) | No |
| ERC-4626-equivalent UX | Yes | No |
| KYC enforcement | Off-chain (via deposit allowlist) | Native but rigid |
| Composability score | High | Low |

For a *vol-arb yield product* the secondary-market exit path matters more than KYC convenience. LPs who can't get out at NAV will simply not subscribe — capital is mobile. If a regulated jurisdiction requires KYC (US accredited investors, EU MiCA), the right answer is a **separate wrapper vault** with deposit allowlists, not a soulbound base token.

### 3.6 Why a 60-minute liveness fallback?

If the enclave host goes dark (AWS region outage, TEE platform upgrade, operator key rotation), the vault must not keep accepting deposits at a stale NAV. After 60 minutes without a fresh attestation:
- Deposits **pause** automatically (on-chain check, no human action required)
- Withdrawals **remain open** at the last confirmed NAV, since locking out LPs during an outage destroys trust
- Multisig can manually shorten or extend pause window for prolonged outages

---

## 4. Dispute mechanism (12h window)

```
T+0      attestation_v1 posted → status: PENDING, old NAV still live for deposit/withdraw
T+0..12h dispute window open
         · anyone can call vault::raise_dispute(alt_attestation_or_evidence)
         · valid dispute = either (a) a second attestation from a whitelisted PCR with
           different NAV value, or (b) on-chain evidence that a position the enclave
           claimed does not exist
         · valid dispute → vault enters DISPUTED state, deposit + withdraw paused
         · 4/5 multisig (2 team + 1 community + 1 auditor + 1 reserve) arbitrates
           within 48h
T+12h    no dispute → attestation_v1 promoted to LIVE, becomes the NAV for deposit/withdraw
```

Notes:
- The challenge window is **per-attestation**, not global — new attestations keep arriving every 5 min and queue up in PENDING state. The "live" NAV is always the most recent attestation that has cleared its 12h window.
- During a dispute, the vault uses the **last LIVE NAV** (which is at minimum 12h old). This is intentional — it removes any economic incentive for the operator to fabricate a dispute to lock LPs in.

---

## 5. Demo strategy (Sui Overflow 2026, ~3 min segment)

The vault story has to land in the 7-minute demo without consuming it. Plan:

### 5.1 Pre-demo setup
- Run vault on **testnet** with challenge window dialed down to **5 minutes** (governance-set) so the full flow fits in a live demo. The 12h production default is documented in the slide deck.
- Pre-fund vault with $10k dUSDC across 3 mock LPs (visible on dashboard).
- Pre-warm Nautilus enclave; first attestation already on-chain so `current NAV` is non-trivial ($1.0237 / share).

### 5.2 Live demo segment (target: 90 seconds)

| Time | Action | Judge takeaway |
|---|---|---|
| 0:00 | Show vault dashboard: 3 LPs, NAV = $1.0237, AUM = $10.2k, last attestation 2 min ago, PCR pinned | "This is a real cross-chain vault" |
| 0:15 | Click "Show NAV proof" → opens Walrus blob: raw position data from Sui + HL + Base, enclave signature, PCR hash | "TEE attestation, not just operator's word" |
| 0:30 | Trigger live NAV update on stage: enclave reads positions, computes, signs, lands on Sui (PTB on explorer link) | "5-minute cadence working live" |
| 0:50 | Manually `raise_dispute(fake_attestation)` from a second wallet → vault flips to DISPUTED, deposit button greys out | "Safety mechanism is real, not theatre" |
| 1:10 | Multisig resolves dispute (3-of-5 sigs on testnet) → vault back to LIVE | "Human override path is wired end-to-end" |
| 1:25 | Deposit $500 → receive `Coin<VOL_ARB_SHARE>` 488.42 shares (at live NAV $1.0237; 500 / 1.0237 = 488.42) | "Real fungible share token, transferable on DeepBook spot" |

### 5.3 Slide that backs the demo

One slide in the deck titled **"Why every default value"** with the table from §3.3 / §3.4 above, so judges who don't sit through the full demo still see the engineering rigor. Reference this ADR by URL in the slide footer.

### 5.4 Optional B-roll (if time allows)
- Diff a real enclave PCR vs a tampered PCR → on-chain verifier rejects → red error toast.
- Show governance proposal queue: "Lower NAV cadence to 1min, raise challenge window to 18h" → demonstrates parameters are not hardcoded.

---

## 6. Non-goals (out of scope for this ADR)

- Cross-chain bridging of the actual position assets (vault holds USDsui only; positions live on their native chains, NAV captures their value).
- Mainnet KYC wrapper vault (separate ADR when v2 ships to regulated jurisdictions).
- Recursive vault-of-vaults / fund-of-funds structures (v3 territory).
- Slashing of operator stake for bad attestations (v3; needs a separate economic security ADR).

---

## 7. Open follow-ups

1. **Multisig composition** — current plan is 2 team + 1 community + 1 auditor + 1 reserve. Names to lock in before mainnet.
2. **Walrus blob retention policy** — attestations reference Walrus blobs of raw input data; need to decide minimum retention (proposal: 2 years).
3. **Pyth feed staleness threshold** — separate from NAV cadence; enclave should reject Pyth quotes older than 30s when computing mark-to-market.
4. **Audit scope** — Move modules `vol_arb_vault`, `nautilus_verifier`, `vault_admin` need full audit before mainnet. Enclave Rust code needs separate review (recommendation: Asymmetric Research or Trail of Bits).
5. **Dispute anti-griefing** — §4 lets *any* address call `raise_dispute`, which pauses deposits/withdraws. With no bond/cost, a single attacker can DoS the vault by re-disputing every 5-min attestation for free. Must add a dispute bond (slashed if dispute is bogus, refunded if valid) or a rate limit before mainnet. The 60-min liveness fallback does NOT cover this (it only handles missing attestations, not spammed disputes).

---

## 8. Version history

| Date | Author | Change |
|---|---|---|
| 2026-05-28 | Vol-Arb Bot team (with Claude/Opus 4.7) | Initial draft — Nautilus + 12h window + fungible share locked in |
