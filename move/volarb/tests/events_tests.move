#[test_only]
module volarb::events_tests;

use std::unit_test::assert_eq;
use sui::clock;
use sui::event;
use sui::test_scenario as ts;
use volarb::events::{Self, ArbIntent};

const TS_MS: u64 = 1_716_000_000_000;

#[test]
fun emits_one_arb_intent_carrying_clock_timestamp() {
    let sender = @0xA;
    let mut scenario = ts::begin(sender);

    let mut c = clock::create_for_testing(scenario.ctx());
    c.set_for_testing(TS_MS);

    events::emit_arb_intent(
        b"cycle-001".to_string(),
        b"hyperliquid".to_string(),
        &c,
    );

    // Exactly one event, and its timestamp is the Clock value — not an epoch
    // start time (ADR-007). A regression to TxContext::epoch_timestamp_ms()
    // would make this assertion fail.
    let emitted = event::events_by_type<ArbIntent>();
    assert_eq!(emitted.length(), 1);
    let e = &emitted[0];
    assert_eq!(events::timestamp_ms(e), TS_MS);
    assert_eq!(events::cycle_id(e), b"cycle-001".to_string());
    assert_eq!(events::venue_id(e), b"hyperliquid".to_string());

    c.destroy_for_testing();
    scenario.end();
}
