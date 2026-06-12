//! volarb-sui — Sui PTB builders + `predict::*` call wrappers (spec §5).
//! Owns the Strike/Expiry/UsdcAmount -> on-chain conversions (TODO #6, spec §3.2 — UNVERIFIED).
//! TODO: deposit + mint PTB, owned PredictManager version refetch on retry.

/// Deterministic sweep point generation for the one-shot L3 parity capture bin.
pub mod capture_points;
