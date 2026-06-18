# Vol-Arb Bot: Predict ↔ Polymarket

**One-line pitch**: Back-solve implied vol from DeepBook Predict's `OracleSVI`, compare against Polymarket's BTC event smile, and trade the spread — optionally delta-hedged on Hyperliquid perps.

## Problem it solves
No one has yet proven DeepBook Predict's vol surface is tradeable against external venues. Without cross-venue arb, SVI stays a toy and PLP yield stays unstressed.

## Core mechanism
1. Stream `oracle::OracleSVIUpdated` → fit local IV surface per strike/expiry.
2. Pull Polymarket BTC binaries → invert to implied prob → implied vol.
3. When |Δσ| > threshold + fee buffer → `predict::mint` on cheap side, take opposite binary on Polymarket.
4. Optional: delta-hedge net exposure with Hyperliquid BTC perp for pure vol PnL.
5. Kelly sizing, kill-switch on feeder lag, stale-SVI guard.

## Why this track
HANDBOOK names this verbatim: **"Single most realistic mainnet-day-one strategy."** Hits "Real-World 50%" by stress-testing SVI exactly as the Predict team wants. Composes Predict + external venues — proves portability.

## Win probability: 88/100
HANDBOOK explicit endorsement = direct judge signal. Demoable with live PnL chart. Risk: requires real capital flows to look convincing, and Polymarket API/geofence friction.

## Risks / weaknesses
- Polymarket API rate limits / geoblocking.
- SVI feeder lag → adverse fills.
- Hyperliquid perp leg adds operational complexity; may need to drop for MVP.
- Reviewers may want stat-sig backtest, not just live trades.

## Required Sui primitives
- DeepBook Predict: `oracle::OracleSVI*`, `predict::mint/redeem`, `PredictManager`.
- Off-chain: Polymarket CLOB API, optional Hyperliquid API.
- Pyth (sanity check on BTC mark).
- `predict-server.testnet.mystenlabs.com` indexer.

## MVP scope
- Live IV surface viewer (read-only).
- Spread monitor dashboard (Predict IV vs Polymarket IV by strike).
- One-click "execute arb" on testnet (Predict leg real, Polymarket leg simulated if geofenced).
- 24h PnL tape + kill-switch UI.
- Backtest replay over 2 weeks of historical SVI updates.
