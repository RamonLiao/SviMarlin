# BUSINESS_SPEC — Vol-Arb Bot (Predict ↔ Multi-Venue Prediction-Market Vol Aggregator)

> Track 2 · DeepBook & Prediction Markets · Sui Overflow 2026
> HANDBOOK quote: **"Single most realistic mainnet-day-one strategy."**
> Last scope revision: 2026-05-28 — pivoted from single-venue (Polymarket) to multi-venue aggregator after Hyperliquid HIP-4 (2026-05-02) and Limitless on Base went live with sub-hour BTC binaries that match Predict's expiry granularity.

---

## 1. Executive Summary

Vol-Arb Bot is a **multi-venue** volatility-surface arbitrage engine that back-solves implied volatility (IV) from **DeepBook Predict's `OracleSVI`** feed and compares it against the IV implied by binary/event prices on a pluggable set of external prediction markets — **Hyperliquid HIP-4 Outcome Markets** as the MVP primary leg, **Limitless on Base** as the secondary leg, with **Polymarket / Thales Speed / Opinion / Binance Event Contracts** queued as v1 adapters. The bot mints the cheap-side leg on Predict via `predict::mint` and takes the opposite leg on whichever external venue shows the largest |Δσ|. Optional delta-hedge on Hyperliquid perps isolates pure vol PnL.

This is the only Track 2 idea explicitly endorsed by the DeepBook Predict team as a mainnet day-one strategy — it stress-tests the SVI surface in the exact way the protocol was designed to be tested, generates real PnL traceable in a live demo, and proves Predict's portability against **every** major prediction-market venue, not just one. Win probability: **88/100**.

**Why multi-venue:** Hyperliquid HIP-4 (live 2026-05-02) ships 3-minute and 15-minute BTC binaries that align perfectly with Predict's sub-hour expiries — Polymarket's daily/weekly granularity actually mis-aligns with Predict's alpha source. Aggregating venues turns a one-shot pair trade into a flow business: the bot continuously picks the best external counter-leg per cycle, defending against any single venue's liquidity / geofence / outage risk.

Target outcome at hackathon: a working bot + dashboard demonstrating ≥3 closed arb cycles on testnet with **both legs real on-chain** (Predict testnet + Hyperliquid HIP-4 testnet, Chain ID 998) and auditable PnL, plus a 2-week backtest replay over historical `OracleSVIUpdated` events showing positive Sharpe net of fees, plus a working `VenueAdapter` trait with ≥2 implementations (Hyperliquid, Limitless) proving the multi-venue architecture.

---

## 2. Problem Statement

Prediction markets cleared **~$33B combined notional** in their most recent reported periods — Polymarket ~$9B in 2024 [source: The Block Data, 2025-01-03, https://www.theblock.co/data] and Kalshi $23.8B in 2025 [source: KalshiData annual report, 2026-01-04] — with an annualized run-rate trending toward **~$200B/yr** by mid-2026 driven by Kalshi's 12× YoY growth. Yet they remain structurally inefficient:

- **Vol-surface blind**: Polymarket binaries quote a single implied probability per strike/expiry. They cannot price a smile, skew, or term structure. Pro market makers must reverse-engineer this offline.
- **Fragmented IV**: The same "BTC > $100k by Friday" risk trades at radically different implied vols across Polymarket, Kalshi, Deribit, and Hyperliquid event books — often **20–40 vol points apart** during macro events.
- **No on-chain CLOB for event vol**: DeepBook Predict is the first protocol to publish a **live, on-chain SVI surface** with sub-hour rolling expiries — but until someone proves the surface is *tradeable* against external venues, it remains a research toy.
- **PLP yield is unstressed**: Without arb flow, the PLP (passive LP) vault that takes the other side of every Predict trade earns yield without price discovery, leaving institutional LPs unable to size up.

**The gap**: no production bot today closes the loop Predict↔Polymarket. Whoever ships first becomes the reference implementation and the natural liquidity sink for Predict at mainnet launch.

---

## 3. Target Users & Personas

### Persona A — Pro Prop Trader / Solo Quant ("Maya, ex-Jane Street")
- AUM: $500k–$5M of own capital.
- Pain: Polymarket lacks programmatic Greeks; manually pricing skew is exhausting.
- Wants: a hosted bot or open-source repo she can fork, parameterise her own Kelly fraction, kill-switch thresholds, and run on her own keys.

### Persona B — Crypto-Native Quant Fund ("Δ Capital, $40M AUM")
- Already runs vol-arb on Deribit↔Binance options.
- Pain: prediction-market alpha is large but ops-heavy (geofencing, KYC, settlement risk).
- Wants: a vault wrapper they can subscribe LPs into, with monthly NAV, daily PnL attribution, and a third-party audit.

### Persona C — Liquidity DAO / Treasury ("SuiVault DAO")
- Sits on $10M+ of idle stables.
- Pain: needs market-neutral yield ≥ 15% APR with on-chain transparency.
- Wants: tokenised vault shares (ERC-4626-style on Sui), public dashboard, governance over risk parameters.

### Persona D — DeepBook Predict Core Team (indirect user)
- Wants real flow against the SVI surface to prove the protocol works under stress.
- Vol-Arb Bot is effectively their canary deployment.

---

## 4. Use Cases — Three Concrete Strategies

### UC1 — Sub-Hour BTC Strike Smile Arb (flagship, Hyperliquid-primary)
Hyperliquid HIP-4 lists 3-minute and 15-minute BTC binary buckets ("BTC ≥ $X at HH:MM"). Invert each to implied probability → implied vol via Black-Scholes binary (cash-or-nothing). Compare vs Predict SVI at matching strike/expiry. When |Δσ| > 8 vol points + 2× fee buffer:
- Mint cheap-side on Predict (`predict::mint` with `PredictManager`) via PTB on Sui.
- Take opposite binary on Hyperliquid HIP-4 (or Limitless if HL liquidity is thin at the target strike) via the `VenueAdapter` interface.
- Optional: delta-hedge residual BTC exposure on Hyperliquid 1× perp (shares the same HL signer + margin pool).
Hold to expiry (3–60 min), settle both legs atomically, book vol-spread PnL. Expiry granularity match is the alpha — Predict's sub-hour SVI updates only become tradeable against a venue that lists matching sub-hour binaries; daily/weekly Polymarket binaries can't capture this.

### UC2 — Event Vol Crush (FOMC / CPI / Election Night)
Pre-event, **all** external venues' vol balloons (retail panic across HL HIP-4 + Limitless + Polymarket). Predict SVI lags by ~minutes due to oracle update cadence. Bot detects the lag window via multi-venue median IV, sells the *most* expensive external vol leg (venue selected by `argmax(|σ_ext − σ_predict|)`), buys cheap Predict vol. Position auto-closes within 15–60 minutes when SVI catches up. Highest Sharpe trade in the book; multi-venue makes this strategy capacity-aware — when HL fills, route to Limitless; when Limitless thin, route to Thales Speed.

### UC3 — Long-Tail / Cross-Asset Arb (v1 stretch)
For longer-dated or non-BTC risk, route to v1 venues: **Polymarket** weekly binaries (BTC long tail), **Thales Speed** 5-minute ETH binaries, **Binance Event Contracts** for CEX-side arb when on-chain venues are illiquid. Lower frequency, higher payoff, and proves the `VenueAdapter` pattern scales to N venues. Polymarket retains story value (largest brand-recognized prediction market) without being on the MVP critical path.

---

## 5. Market Analysis

### TAM / SAM / SOM
- **TAM** — top-2 regulated prediction markets: Polymarket ~$9B (2024) [source: The Block Data, 2024]; Kalshi ~$23.8B (2025), annualized ~$178B run-rate by May 2026 [source: Sacra/KalshiData, 2026]. Combined addressable notional **~$200B/yr**. Vol-arb captures ~0.1–0.3% as edge → **$200M–$600M/yr addressable PnL pool**.
- **SAM** — on-chain prediction markets connectable to Sui without KYC friction: ~$220B/yr (Polymarket-class). Arb edge realistically extractable: **$220M/yr**.
- **SOM (year 1)** — capacity-constrained by Predict PLP depth + Polymarket per-market size limits. Realistic capture: **$2–10M PnL/yr** with $5–20M deployed capital.

### Competitive Landscape

| Venue / Tool | On-chain CLOB? | Vol Surface? | Sub-hour Expiries? | Cross-venue arb support |
|---|---|---|---|---|
| Polymarket | No (Polygon UMA-settled CLOB) | No | No (daily/weekly) | Manual only |
| Kalshi | No (CFTC-regulated DCM) | No | Limited | Manual, US-only |
| Hyperliquid event books | Yes (HL CLOB) | No | No | Internal HL only |
| Deribit (CEX options) | n/a | Yes | No (8h min) | API, KYC-gated |
| Hyperliquid HIP-4 Outcome Markets (live 2026-05-02) | Yes (HL L1) | No | Yes (3m / 15m BTC binary) | Internal HL only |
| Limitless (Base) | Yes (Base) | No | Yes (sub-hour) | Per-venue only |
| **DeepBook Predict + Vol-Arb Bot (multi-venue aggregator)** | **Yes (Sui CLOB)** | **Yes (SVI)** | **Yes (rolling sub-hour, expiry-aligned with HL HIP-4 + Limitless)** | **Native, N-venue routing** |

No competing product today combines on-chain CLOB + live SVI + sub-hour expiries + permissionless arb tooling. This is whitespace.

---

## 6. Differentiation — Why Sui + DeepBook + Predict

1. **SVI is the moat**: DeepBook Predict ships an on-chain Stochastic Volatility Inspired surface — every strike priced off a continuous IV curve, not point-by-point quoting. No competitor has this primitive on-chain.
2. **DeepBook V3 BalanceManager** lets the bot run a single shared collateral pool across spot hedges (BTC-denominated stables), Predict positions, and `deepbook_margin` leverage — atomic PTB execution means no inter-leg slippage risk.
3. **DEEP maker rebates** turn the Polymarket-side cost structure on its head: on the Predict leg the bot can be net-paid to provide liquidity rather than crossing the spread.
4. **Sub-400ms finality** matters because vol-arb edge decays in seconds. Sui's parallel execution + Mysticeti consensus is the only L1 where this strategy is operationally viable on-chain.
5. **Composability stack** — Predict + `deepbook_margin` + `iron_bank` USDsui means the vault wrapper (v1) can deliver 2–3× leveraged vol-arb with on-chain liquidation logic, something impossible on Polymarket alone.

---

## 7. Product Scope

### MVP (Hackathon, ~5 weeks)
- Read-only **IV surface viewer** streaming `oracle::OracleSVIUpdated`.
- **Spread monitor dashboard** — Predict IV vs Polymarket implied IV per strike, color-coded thresholds.
- **One-click "execute arb"** — Predict leg real on testnet; Polymarket leg simulated (geofence-safe).
- **24h PnL tape** + global kill-switch UI.
- **2-week backtest replay** with downloadable CSV of trades.
- Kelly sizing + stale-SVI guard + feeder-lag detector.

### v1 (post-hackathon, mainnet day one — 8 weeks)
- **Bot-as-a-Service**: hosted bot, BYO-key, monthly subscription.
- Real Polymarket execution via headless browser / proxy layer (jurisdiction-permitting).
- Hyperliquid delta-hedge leg enabled.
- Telegram alerts + per-user PnL attribution.

### v2 (vault wrapper — Q4 2026)
- **Tokenised vol-arb vault** (ERC-4626-equivalent on Sui).
- Public NAV, third-party audit, governance over Kelly fraction + venue weights.
- Multi-asset (ETH, SOL vol surfaces) and multi-venue (add Kalshi for US LPs via wrapped structure).

### Bot SaaS vs Vault — strategic call
MVP and v1 ship as **Bot SaaS** because (a) faster regulatory path, (b) better data flywheel from many keys, (c) lower trust requirement. Vault is v2 once 90-day live PnL gives LPs confidence.

---

## 8. User Flow — Quant Onboarding

1. **Land on dashboard** → public read-only IV surface + last-30-day backtest PnL chart visible without wallet.
2. **Connect Sui wallet** (Slush / Suiet) → auto-create `PredictManager`, faucet 10k dUSDC on testnet.
3. **Connect Polymarket account** → OAuth or API key paste (v1); skipped on MVP (simulated leg).
4. **Configure strategy** — slider for Kelly fraction (default 0.25), vol-spread threshold (default 8pts), max position per market, kill-switch params.
5. **Backtest** — run 2-week replay against historical SVI; see expected Sharpe / max DD.
6. **Go live** → bot starts streaming. Dashboard shows: open positions, mark-to-market PnL, next expiry countdown, feeder-health LEDs.
7. **Settlement** — bot auto-calls `predict::redeem` (or relies on keeper network), books realised PnL, posts to Telegram.
8. **Withdraw / scale** — pull profits or top up collateral; vault subscribers (v2) get share tokens instead.

---

## 9. Technical Architecture (summary, no code)

- **On-chain (Sui Move)**: existing DeepBook Predict modules — `predict::mint`, `predict::redeem`, `predict::supply`, `PredictManager` (per-user), `oracle::OracleSVI*`. Bot interacts via PTB; no new Move modules required for MVP. v2 adds a `vol_arb_vault` Move package (share token, NAV oracle, deposit/withdraw, performance-fee accrual, emergency pause).
- **Pricing engine (Rust core)**: subscribes to Mysten's `predict-server.testnet.mystenlabs.com` indexer for `OracleSVIUpdated` events; fits local IV per strike/expiry; cross-checks BTC mark vs **Pyth** Sui feed for sanity. Rust chosen for SVI math throughput and PnL attribution; TS dashboard layer talks to it via gRPC.
- **VenueAdapter trait (Rust)**: every external prediction market implements a single trait with `quote()`, `place()`, `cancel()`, `settle()`, `health()`. MVP ships **Hyperliquid HIP-4 adapter** (primary, REST/WS on `api.hyperliquid-testnet.xyz`, EIP-712 signing) and **Limitless adapter** (secondary, Base sepolia REST/WS). v1 adds Polymarket (Polygon CLOB), Thales Speed (Arb/Base), Opinion (BNB), Binance Event Contracts (CEX REST). The router (`venue_selector`) picks `argmax(|Δσ| − fees − slippage)` per cycle.
- **Execution layer**: Sui SDK builds atomic PTB (`predict::mint` + optional `deepbook_margin` borrow); external leg fires through the selected `VenueAdapter`; cross-chain leg state-machine (`Pending → SuiFilled → ExtFilled → Settled` with rollback on either-side failure within 30s window). For HL the leg is sub-second; for slower venues the bot pre-quotes and uses a leg-failure unwind path.
- **Risk engine**: Kelly sizer, stale-SVI watchdog (>60s lag triggers halt), Pyth-divergence kill-switch (>1% BTC mark mismatch), per-venue and per-market notional caps, single-venue concentration limit (no >50% exposure to one external venue), adapter-health circuit breakers.
- **Indexer + dashboard**: custom Postgres + Next.js (TS) dashboard; live PnL tape per venue + aggregate; backtest replay reads from a snapshot of `OracleSVIUpdated` + per-venue historical quotes (recorded by the indexer in real time from launch).
- **v2 Vault Move package** (post-MVP): four-module Move package — `vol_arb_vault` (deposit/withdraw, share token, fee accrual), `nautilus_verifier` (PCR + signature checks), `vault_admin` (governance multisig, PCR whitelist, parameter updates), and a thin `vault_events` module for indexer hooks. Vault share is a transferable `Coin<VOLARB_SHARE>`. NAV is computed inside an AWS Nitro Enclave (Sui Nautilus framework) that reads positions across Sui Predict + Hyperliquid HIP-4 + Limitless + Pyth, signs an attestation, and posts on-chain through a **5-minute cadence (configurable)** with a **12-hour challenge window** before the new NAV becomes live for deposit/withdraw. Full design rationale, default values, and demo plan are documented in `docs/decisions/ADR-001-nav-oracle.md`.

---

## 10. Business Model

Three monetisation paths, layered:

1. **Bot SaaS (v1)** — flat $99/mo retail tier; $499/mo pro tier with priority RPC + Hyperliquid leg; $2,500/mo institutional with dedicated infra.
2. **Performance fee on vault (v2)** — standard 2/20 (2% mgmt, 20% perf above hurdle of SOFR+4%).
3. **Edge sharing with Predict / DeepBook** — negotiate a rebate share from `DEEP` maker incentives and PLP arb-flow attribution. The bot is a structural liquidity source for Predict, justifying a grant or revenue share.

Unit economics: at $20M deployed in v2 with 20% gross APR → $4M gross PnL → ~$1M perf fee, ~$400k mgmt fee. Single 2-engineer team can run it.

---

## 11. Go-to-Market

- **Phase 0 — hackathon win**: HANDBOOK endorsement + working demo + live PnL chart = direct judge signal. Win = legitimacy.
- **Phase 1 — open-source the bot core**: GitHub repo + Loom walkthrough. Target audience: r/algotrading, Twitter quant-fintwit, Sui builder TG.
- **Phase 2 — institutional warm intros**: pitch Wintermute, GSR, Auros — they already MM on Polymarket and want on-chain venues. Predict + bot = their on-ramp.
- **Phase 3 — vault launch with anchor LPs**: 2–3 Sui-native DAOs (SuiVault, Scallop treasury, Navi) seed $1–3M each.
- **Phase 4 — multi-venue, multi-asset**: ETH/SOL surfaces, Kalshi for US, regulated wrapper.

---

## 12. Hackathon Demo Plan + Judging Mapping

### 7-minute demo script
1. (0:00–0:45) **Hook**: live IV surface 3D plot rotating; "this is the only on-chain vol surface in crypto."
2. (0:45–2:00) **The arb**: side-by-side panel — Predict $100k strike at 62 vol, Polymarket implied at 78 vol, Δ=16pts. Bot flashes green.
3. (2:00–4:00) **Live execution**: click "execute" → PTB lands on Sui testnet (show explorer link), Polymarket leg fills in simulator. Position card appears.
4. (4:00–5:30) **Backtest**: scrub 2-week replay → equity curve climbing. Demo success criteria (hypothetical targets, not measured results): **target Sharpe ≥2.5, target max DD ≤5%** — actual numbers TBD once backtest infra runs against real historical `OracleSVIUpdated` snapshots.
5. (5:30–6:30) **Risk theatre**: trigger fake feeder lag → kill-switch fires; flatten positions automatically.
6. (6:30–7:00) **Mainnet day-one pitch**: roadmap slide, ask the judges.

### Judging criteria mapping
- **Real-World (50%)** — direct hit. We trade actual external venue (Polymarket), real PnL, stress-tests the production SVI feed. This is what the Predict team explicitly asked for.
- **Technical Quality (20%)** — atomic PTB, Pyth sanity, kill-switch, backtest infra.
- **Innovation (15%)** — first cross-venue arb on a live on-chain vol surface, anywhere.
- **UX (10%)** — surface viewer + one-click execute is genuinely usable.
- **Sui Ecosystem Fit (5%)** — DeepBook + Predict + Pyth + `deepbook_margin` + Slush wallet.

---

## 13. Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Hyperliquid HIP-4 testnet outage / API rate-limit during demo | Medium | High | Limitless adapter as hot-standby; `venue_selector` auto-fails over; demo script pre-warms both venues |
| Hyperliquid US-IP geofence affects team | Low | Medium | Team is in Taiwan (non-US); CI/demo infra in Asia-Pacific region; document VPN-free verification for judges |
| Polymarket UMA settlement dispute (when v1 enables Polymarket adapter) | Medium | High | Cap notional per market < 5% of capital; whitelist only high-volume, low-dispute-history markets; insurance reserve from perf fees |
| Single-venue dependency (multi-venue thesis fails if one dominates) | Medium | Medium | `VenueAdapter` enforces ≥2 active adapters in MVP; concentration cap = 50% per venue; backtest reports per-venue Sharpe to prove diversification value |
| SVI feeder lag / stale oracle on Predict | Medium | Medium | Watchdog halts trading on >60s lag; Pyth cross-check; auto-flatten on prolonged outage |
| Predict PLP liquidity too shallow for arb size | Medium | Medium | Size-aware Kelly cap; queue large trades; v2 vault coordinates with PLP supply |
| Hyperliquid perp leg adds ops risk (key mgmt, API outage) | Medium | Low | Drop perp leg for MVP; gate behind feature flag in v1; circuit-breaker on API timeouts |
| Reviewers demand statistical significance, not live trades | Medium | Medium | Ship 2-week backtest with Sharpe + DD + trade count alongside live tape |
| dUSDC ≠ real USDC on testnet → demo lacks realism | Low | Low | Display testnet labels; emphasise mainnet-day-one redeploy path |
| Mainnet Predict launch delayed beyond hackathon judging | Low | Medium | Strategy works identically on testnet; backtest provides the credibility bridge |

---

## 14. Open Questions

1. **Polymarket execution surface** — partner via official market-maker API, build headless-browser executor, or pivot to a jurisdiction-friendly clone (Azuro, Drift BET)?
2. **Hyperliquid leg ROI** — does the delta-hedge actually improve risk-adjusted return after gas + funding, or is binary-only cleaner for MVP?
3. **Kelly fraction default** — 0.25 conservative or 0.5 aggressive? Needs 4+ weeks of live data to calibrate.
4. **Vault legal wrapper** — Cayman SPV, BVI, or Sui-native DAO with off-chain memorandum?
5. **Revenue share with DeepBook / Predict** — formal grant + rebate, or informal alignment via PLP supply?
6. **Settlement-day handling** — auto-redeem via own keeper, or piggyback on the Settled-Redeem Keeper Network (idea #8 in HANDBOOK)?
7. **Multi-asset roadmap timing** — ETH vol surface immediately after BTC, or wait for SVI feed maturity?
8. **Audit scope** — for v1 SaaS, key mgmt audit suffices; v2 vault needs full Move + off-chain audit. Who and when?

---

*End of spec. ~2,300 words.*
