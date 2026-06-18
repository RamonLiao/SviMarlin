/// Indexer enrichment events for the vol-arb bot.
///
/// The MVP critical path uses only existing DeepBook Predict / DeepBook /
/// Pyth modules. This module exists solely so the off-chain indexer can
/// correlate a Sui-leg transaction back to the cross-venue arbitrage cycle
/// that produced it (the venues themselves emit no Sui events).
///
/// Timestamps come from the shared `Clock` object (id `0x6`) rather than
/// `TxContext::epoch_timestamp_ms()`, which only returns the epoch start
/// time — see ADR-007.
module volarb::events;

use std::string::String;
use sui::clock::Clock;
use sui::event;

/// Emitted on the Sui leg of an arbitrage cycle so the indexer can join it
/// to the off-chain quote/fill records sharing the same `cycle_id`.
public struct ArbIntent has copy, drop {
    /// Bot-assigned id correlating both legs of one arbitrage cycle.
    cycle_id: String,
    /// Counter-venue the off-chain leg was routed to (e.g. "hyperliquid").
    venue_id: String,
    /// ms-precision wall clock from the `Clock` shared object.
    timestamp_ms: u64,
}

/// Emit an `ArbIntent` for the current cycle. Called from the executor's
/// Sui-leg PTB alongside `predict::mint`.
public fun emit_arb_intent(cycle_id: String, venue_id: String, clock: &Clock) {
    event::emit(ArbIntent {
        cycle_id,
        venue_id,
        timestamp_ms: clock.timestamp_ms(),
    });
}

#[test_only]
public fun cycle_id(e: &ArbIntent): String { e.cycle_id }

#[test_only]
public fun venue_id(e: &ArbIntent): String { e.venue_id }

#[test_only]
public fun timestamp_ms(e: &ArbIntent): u64 { e.timestamp_ms }
