//! volarb-pricing — L1 float-domain pricing: Black-Scholes binary inversion (HL leg) and the
//! Zeliade quasi-explicit SVI fitter. Zero IO; fully unit-testable. (L0 chain-parity + L3 parity
//! harness land in Plan B.) Design: `docs/specs/2026-05-30-volarb-pricing-svi-fitter-design.md`.

pub mod binary;
pub mod onchain;
pub mod svi_fit;

pub use svi_fit::fit_smile;

use thiserror::Error;

/// Errors from the pure pricing layer. (Pricing has no venue IO, so no `VenueError` here —
/// that is the venue-trait boundary, ADR-003.)
#[derive(Debug, Error, PartialEq)]
pub enum PricingError {
    #[error("need at least 3 observations to fit a smile, got {0}")]
    TooFewPoints(usize),
    /// Defensive-only backstop: the current inner solve clamps all params to finite bounds, so a
    /// non-finite SSE is unreachable today. Kept as the typed error path for future inner solvers
    /// (e.g. an iterative active-set replacement) that could genuinely diverge.
    #[error("fit did not converge")]
    NonConvergent,
    #[error("degenerate input: {reason}")]
    Degenerate { reason: &'static str },
    #[error("invalid input: {reason}")]
    InvalidInput { reason: &'static str },
}
