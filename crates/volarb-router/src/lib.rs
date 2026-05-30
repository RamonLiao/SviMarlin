//! volarb-router — spread detection + venue selection (spec §3.3).
//! Consumes pricing SVI surface + venue quotes + risk gates → emits a TradeIntent.
//! TODO #5+: `route(predict_iv, venues) -> Option<TradeIntent>` with MIN_EDGE_VOL_POINTS gate.
