//! volarb-core — shared types for the vol-arb engine.
//!
//! Numeric strategy (design spec §3.2): pricing domain uses `f64`; on-chain amounts use
//! `u64` newtypes. On-chain precision conversions are deferred to TODO #6 (UNVERIFIED there).
