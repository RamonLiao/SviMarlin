//! volarb-core — shared types for the vol-arb engine.
//!
//! Numeric strategy (design spec §3.2): pricing domain uses `f64`; on-chain amounts use
//! `u64` newtypes. On-chain precision conversions are deferred to TODO #6 (UNVERIFIED there).

pub mod numeric;

pub use numeric::{Expiry, Strike, UsdcAmount, VolPoints};

pub mod svi;

pub use svi::{SVIParams, SVISurface};

pub mod market;
pub mod position;

pub use market::{Quote, Side};
pub use position::Position;
