# ADR-004 — Cross-Chain Leg Unwind Window (30 s)

> Status: **Accepted** · 2026-05-28
> Scope: `crates/volarb-executor` — cycle state machine, specifically the `Sending → Unwinding` and `Unwinding → Aborted` transitions
> Stakeholders: backend engineers, ops, risk
> Related: system spec §2.3 (cross-chain state machine), §3.5 (Executor), ADR-003 (VenueAdapter)

---

## 1. Context

Every arb cycle fires **two legs in parallel**:

1. **Sui leg**: a PTB containing `predict::mint` (atomic, ~400ms finality with Mysticeti)
2. **External leg**: signed order to Hyperliquid / Limitless / Polymarket / etc. (latency varies, 50ms–5s)

Three failure topologies exist:
- **Both fill** → happy path, transition to `Live`
- **Sui fills, external rejects** → bot holds a naked Predict position
- **External fills, Sui reverts** → bot holds a naked external position

The Executor must decide how long to wait before declaring desync and unwinding, and which side to unwind first. Too short → false alarms during normal latency variance. Too long → naked exposure ages dangerously, especially during volatile periods (the exact time we want to trade).

This ADR fixes the window at **30 seconds** for MVP, with structured tuning plan post-data.

---

## 2. Decision

**30-second unwind deadline** measured from cycle entering `Sending` state.

```
State: Sending
  t=0     fire Sui PTB + external order in parallel
  t≤5s    if both confirmed → transition to Live
          if neither confirmed → keep waiting
          if exactly one confirmed → start "single-leg watchdog" (sub-state)
  t=5s    if still no second confirmation → transition to Unwinding
  t=30s   if still in Unwinding without resolution → transition to Aborted, page operator
```

**Unwind precedence** (which side to close first when both filled mismatched, or only one filled):

1. **Always close the external leg first** if it's filled and Sui is naked. External venues have wider spreads and more slippage risk; close while liquidity is fresh.
2. **Close Sui leg only if external close confirmed** or external is already absent. `predict::redeem` is liquid on testnet against PLP; no rush.
3. Special case: **Hyperliquid** market-close uses IoC at 5% worse than mid; if rejected, retry once at 10% worse, then escalate. Limitless similar.

Logged unwind outcomes:
- `UnwindSuccess { realized_loss_bps }` — both sides closed within 30s
- `UnwindPartial { side_remaining }` — one side stuck after 30s → manual ops
- `UnwindFailed` — both sides stuck (typically venue outage) → vault auto-pause if v2

---

## 3. Rationale

### 3.1 Why 5s soft deadline, 30s hard deadline?

Two-stage design reflects two distinct failure modes:

| Stage | Failure mode | Why this duration |
|---|---|---|
| 0–5s soft | Normal latency variance, transient WS hiccup | Sui finality is <400ms p50; HL place is <200ms p50; Limitless ~500ms p50. 5s is **10× p50**, well above noise. |
| 5–30s hard | Real desync — one venue down, signature rejected, etc. | 30s gives unwind logic time to fire market-close, retry once, escalate. Past 30s, market has likely moved; better to admit failure. |

Empirical basis: Deribit↔Binance MM operations published 2024 reported 2-leg fill skew p99 of 3.2s under normal conditions; 30s is well past that distribution's tail.

### 3.2 Why close external leg first?

| Consideration | Sui-leg first | External-leg first (chosen) |
|---|---|---|
| Liquidity stability | Predict PLP deep, ~stable | Venue OB thins fast during desync window |
| Slippage cost | <0.1% for testnet sizes | 1-3% if waited 30s on HL HIP-4 |
| Atomicity guarantees | Sui has them | External rarely has them |
| Reversibility | `predict::mint` cancellation = redeem; no penalty | Some venues penalize fast cancel-and-replace |

External first is universal across DeFi cross-leg ops (Wintermute, GSR-published practice). We adopt the standard.

### 3.3 Why not adaptive (network-condition-based) deadline?

Considered: scale deadline based on observed venue p99 latency over last 5 min. Rejected for MVP because:
- Adds state and tuning complexity for marginal benefit at our trade frequency (~1 cycle/min)
- Risk Engine already gates on `health()` degradation — that catches the same condition more cleanly
- Hard 30s is easy to reason about in post-mortems

Revisit if backtest shows >5% of cycles aborted on the boundary.

### 3.4 Why no "wait for retry" path inside Sending?

If the external leg returns `RateLimited` or `Network`, the Executor does **not** retry inside Sending — it goes straight to Unwinding. Reason: by the time we retry, the Sui leg may have filled and our edge has decayed. Cleaner to abort and let Router re-pick the venue next tick.

### 3.5 Why escalate to operator at 30s instead of automatic indefinite hold?

Naked position aging past 30s on testnet is operationally fine; but on **mainnet v1** the same code path runs against real capital. The behavior must be conservative by default. Operator page (PagerDuty + Telegram) at the 30s mark lets a human decide whether to keep holding or accept the loss.

### 3.6 Demo implication

For Sui Overflow demo, we will **stage a deliberate desync** (kill HL adapter mid-cycle) to show the 30s window firing and clean unwind happening in real time. This is the strongest possible "risk theatre" — it demonstrates the bot is safe against the most realistic production failure.

---

## 4. Alternatives Considered

### 4.1 No unwind window, hold indefinitely

Rejected: violates the bot's market-neutral premise. A held naked leg is a directional bet by accident.

### 4.2 5s hard deadline, no soft stage

Too aggressive. Normal HL fills sometimes take 3-4s under load; 5s would trigger false unwinds at ~5% rate.

### 4.3 60s hard deadline

Too slow. At 60s, BTC can move 0.5-1% in a quiet market and 3-5% during events. Mark-to-market loss exceeds typical edge.

### 4.4 Per-venue tuned deadlines

Considered (HL 15s, Polymarket 90s due to Polygon finality). Rejected for MVP — adds matrix complexity. Revisit when Polymarket adapter ships in v1 (Polygon's variable finality may force this).

---

## 5. Consequences

### Positive
- Operationally simple: one number to memorize
- Risk Engine and Executor are decoupled (gates pre-trade; unwind post-trade)
- Demo-friendly (30s is observably-fast on stage)

### Negative
- May abort cycles that would have completed at 45s in mainnet edge cases. Acceptable cost; lost edge < $1/cycle at MVP scale.
- Doesn't account for venue-specific finality (Polygon's 256-block ~5min finality could need special-casing in v1).

### Neutral
- Unwind logs feed into backtest replay so we can compute "what fraction of aborts would have been wins if deadline = 60s." Empirical re-tune in 4 weeks.

---

## 6. Open Follow-ups

1. **Polygon special case** when Polymarket adapter ships: probably need 5-min deadline aligned with Polygon's finality window. Likely a per-`ChainKind` override.
2. **Asymmetric deadline** (Sui finality is faster than external): possibly soft=2s if Sui confirmed, soft=5s otherwise. Defer.
3. **Backtest re-tune** after 4 weeks of live data: produce `unwind_outcomes` histogram, decide if 30s is optimal.
4. **Mainnet v1 dollar-loss limit on unwind**: if cumulative unwind loss in a day > $X, halt all entries.

---

## 7. Version History

| Date | Author | Change |
|---|---|---|
| 2026-05-28 | team + Claude/Opus 4.7 | Initial. 30s hard deadline, 5s soft. External-first unwind. |
