# Hyperliquid Venue Write Path Implementation Plan — TODO #6 Part 2

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `place`/`cancel` loud stubs and the `position` parse stub in the Hyperliquid adapter with real implementations, signing orders with HL's phantom-agent scheme verified byte-exact against the official python SDK test vectors.

**Architecture:** Add a `signing.rs` (msgpack action-hash + EIP-712 phantom-agent sign via alloy) and an `exchange.rs` (POST /exchange client) under `crates/volarb-venues/src/hyperliquid/`. `mod.rs` wires a `PrivateKeySigner` (key from `.env`) into `place`/`cancel`. `info.rs` gains a `clearinghouseState` → `ExtPosition` parser. `settle` stays a loud stub.

**Tech Stack:** Rust, `alloy-signer-local` + `alloy-signer` (SignerSync) + `alloy-primitives` (B256/Address/keccak) + `alloy-sol-types` (`sol!` EIP-712), `rmp-serde` (msgpack), existing `reqwest`/`serde`/`tokio`.

## Global Constraints

- Edition 2024, toolchain pinned 1.94.1 (`rust-toolchain.toml`). Do not change.
- `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` MUST pass before every commit.
- **No trait-signature change** — `VenueAdapter` (`lib.rs`) is ADR-003-locked. Only implement existing methods.
- **Secrets**: private key only from `std::env::var("HL_TESTNET_PRIVATE_KEY")`. Key NEVER enters git / `.rs` source / fixtures / logs / error messages / `Debug` output. The SDK public test key `0x0123456789012345678901234567890123456789012345678901234567890123` is the ONLY key allowed in test source (it is published in the HL SDK, signs nothing real).
- **msgpack determinism**: every signable action is an explicit `#[derive(Serialize)]` struct with fields in HL's exact order. NEVER `HashMap` / `serde_json::Value` for actions (key reorder → wrong hash).
- Commit per task with explicit paths only (`git add <path>`), never `git add -A` (repo has unrelated untracked files — lessons 2026-06-13).
- `settle` remains `Err(VenueError::VenueSpecific(...))` loud stub this round (out of scope).

## Golden test vectors (from HL python SDK `tests/signing_test.py`, pulled 2026-06-22)

All use signer key `0x0123...0123`. `action_hash(action, vault, nonce, expires_after)` = `keccak(msgpack(action) ++ nonce.to_be_bytes(8) ++ vault_byte ++ expires_byte)` where `vault_byte` = `0x00` if no vault, and `expires_byte` omitted when `expires_after is None`.

- **V1 connectionId** (mainnet, `source="a"`): order asset `4`, `is_buy=true`, `sz=0.0147`, `limit_px=1670.1`, `tif="Ioc"`, no cloid, nonce `1677777606040`, vault None → `connectionId == 0x0fcbeda5ae3c4950a548021552a4fea2226858c4453571bf3f24ba017eac2908`
- **V2 dummy sig**: action `{"type":"dummy","num":100000000000}` (u64; = `float_to_int_for_hashing(1000)`), nonce `0`, vault None
  - mainnet: `r=0x53749d5b30552aeb2fca34b530185976545bb22d0b3ce6f62e31be961a59298`, `s=0x755c40ba9bf05223521753995abb2f73ab3229be8ec921f350cb447e384d8ed8`, `v=27`
  - testnet: `r=0x542af61ef1f429707e3c76c5293c80d01f74ef853e34b76efffcb57e574f9510`, `s=0x17b8b32f086e8cdede991f1e2c529f5dd5297cbe8128500e00cbaf766204a613`, `v=28`
- **V3 order sig**: order asset `1`, `is_buy=true`, `sz=100`, `limit_px=100`, `tif="Gtc"`, no cloid, nonce `0`, vault None
  - mainnet: `r=0xd65369825a9df5d80099e513cce430311d7d26ddf477f5b3a33d2806b100d78e`, `s=0x2b54116ff64054968aa237c20ca9ff68000f977c93289157748a3162b6ea940e`, `v=28`
  - testnet: `r=0x82b2ba28e76b3d761093aaded1b1cdad4960b3af30212b343fb2e6cdfa4e3d54`, `s=0x6b53878fc99d26047f4d7e8c90eb98955a109f44209163f52d8dc4278cbbd9f5`, `v=27`
- **V4 float_to_wire / float_to_int_for_hashing** semantics:
  - `float_to_wire(x)` = `format!("{x:.8f}")`, error if rounding loss ≥ 1e-12, `"-0"`→`"0"`, then strip trailing zeros (Decimal normalize): `100.0`→`"100"`, `1670.1`→`"1670.1"`, `0.0147`→`"0.0147"`.
  - `float_to_int_for_hashing(x)` = `round(x * 1e8)` as i64, error if rounding loss ≥ 1e-3: `1000`→`100000000000`, `0.00001231`→`1231`, `1.033`→`103300000`, `0.000012312312`→error.

> ⚠️ `v` mapping: alloy `Signature::v()` returns y-parity bool; eth `v = 27 + parity`. V2 mainnet v=27 (parity 0), testnet v=28 (parity 1) calibrate this.

> ⚠️ Builder-dex asset index: HyperOdd markets are NOT plain perps. The wire `a` field for a builder-dex asset = `100000 + perp_dex_index*10000 + index_in_dex_universe` (verify at runtime against `perpDexs` order + per-dex `meta.universe` order). The golden vectors above are plain perps (asset 1, 4) so unit tests don't cover the offset — the **live margin probe (Task 9) validates it** (wrong index → different error than insufficient-margin).

---

### Task 1: float_to_wire + float_to_int_for_hashing

**Files:**
- Create: `crates/volarb-venues/src/hyperliquid/signing.rs`
- Modify: `crates/volarb-venues/src/hyperliquid/mod.rs:3-4` (add `pub mod signing;`)

**Interfaces:**
- Produces: `pub(crate) fn float_to_wire(x: f64) -> Result<String, VenueError>`, `pub(crate) fn float_to_int_for_hashing(x: f64) -> Result<i64, VenueError>`

- [ ] **Step 1: Write the failing test**

In `signing.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_to_wire_matches_sdk() {
        assert_eq!(float_to_wire(100.0).unwrap(), "100");
        assert_eq!(float_to_wire(1670.1).unwrap(), "1670.1");
        assert_eq!(float_to_wire(0.0147).unwrap(), "0.0147");
        assert_eq!(float_to_wire(-0.0).unwrap(), "0");
    }

    #[test]
    fn float_to_int_for_hashing_matches_sdk() {
        assert_eq!(float_to_int_for_hashing(1000.0).unwrap(), 100_000_000_000);
        assert_eq!(float_to_int_for_hashing(0.00001231).unwrap(), 1231);
        assert_eq!(float_to_int_for_hashing(1.033).unwrap(), 103_300_000);
        assert!(float_to_int_for_hashing(0.000012312312).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-venues --lib signing::tests::float_to_wire_matches_sdk`
Expected: FAIL (function not defined / build error).

- [ ] **Step 3: Write minimal implementation**

At top of `signing.rs`:
```rust
//! Hyperliquid phantom-agent signing (msgpack action-hash + EIP-712).
//! Verified byte-exact against hyperliquid-python-sdk `signing.py` test vectors.

use crate::VenueError;

/// Mirror of SDK `float_to_wire`: 8dp fixed, error on rounding loss, strip trailing zeros.
pub(crate) fn float_to_wire(x: f64) -> Result<String, VenueError> {
    if !x.is_finite() {
        return Err(VenueError::VenueSpecific(format!("non-finite price/size: {x}")));
    }
    let rounded = format!("{x:.8}");
    let reparsed: f64 = rounded.parse().map_err(|_| {
        VenueError::VenueSpecific("float_to_wire parse failed".into())
    })?;
    if (reparsed - x).abs() >= 1e-12 {
        return Err(VenueError::VenueSpecific(format!("float_to_wire rounding: {x}")));
    }
    // strip trailing zeros and a trailing dot; normalize "-0".
    let mut s = rounded.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() || s == "-0" {
        s = "0".to_string();
    }
    Ok(s)
}

/// Mirror of SDK `float_to_int_for_hashing`: round(x*1e8), error on rounding loss >= 1e-3.
pub(crate) fn float_to_int_for_hashing(x: f64) -> Result<i64, VenueError> {
    let with_decimals = x * 1e8;
    if (with_decimals.round() - with_decimals).abs() >= 1e-3 {
        return Err(VenueError::VenueSpecific(format!("float_to_int rounding: {x}")));
    }
    Ok(with_decimals.round() as i64)
}
```
Add `pub mod signing;` to `mod.rs` after `pub mod ws;`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p volarb-venues --lib signing::tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/signing.rs crates/volarb-venues/src/hyperliquid/mod.rs
git commit -m "feat(venues): HL float_to_wire/float_to_int_for_hashing (SDK-vector-exact)"
```

---

### Task 2: Action wire types + msgpack action_hash (connectionId vector)

**Files:**
- Modify: `crates/volarb-venues/src/hyperliquid/signing.rs`
- Modify: `crates/volarb-venues/Cargo.toml` (add deps)

**Interfaces:**
- Consumes: `float_to_wire` (Task 1)
- Produces:
  - `#[derive(Serialize)] pub(crate) struct OrderWire { a, b, p, s, r, t }` (limit-only)
  - `#[derive(Serialize)] pub(crate) struct OrderAction { type, orders, grouping }`
  - `#[derive(Serialize)] pub(crate) struct CancelAction { type, cancels: Vec<CancelWire> }`, `CancelWire { a, o }`
  - `pub(crate) fn order_wire(asset: u32, is_buy: bool, px: f64, sz: f64, reduce_only: bool, tif: &str) -> Result<OrderWire, VenueError>`
  - `pub(crate) fn action_hash<T: Serialize>(action: &T, nonce: u64, vault: Option<[u8;20]>) -> Result<B256, VenueError>`

- [ ] **Step 1: Add dependencies**

Run (then verify resolved versions are recorded in Cargo.toml):
```bash
cargo add -p volarb-venues rmp-serde alloy-primitives alloy-sol-types alloy-signer alloy-signer-local hex
```
Expected: deps added. If a version conflict appears, check installed versions first (dev-rules) before pinning.

- [ ] **Step 2: Write the failing test**

In `signing.rs` tests module:
```rust
    #[test]
    fn connection_id_matches_sdk_vector() {
        // V1: ETH order asset 4, buy, sz 0.0147, px 1670.1, Ioc, nonce 1677777606040, no vault.
        let ow = order_wire(4, true, 1670.1, 0.0147, false, "Ioc").unwrap();
        let action = OrderAction {
            r#type: "order",
            orders: vec![ow],
            grouping: "na",
        };
        let hash = action_hash(&action, 1_677_777_606_040, None).unwrap();
        assert_eq!(
            hex::encode(hash),
            "0fcbeda5ae3c4950a548021552a4fea2226858c4453571bf3f24ba017eac2908"
        );
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p volarb-venues --lib signing::tests::connection_id_matches_sdk_vector`
Expected: FAIL (types not defined).

- [ ] **Step 4: Write minimal implementation**

Add to `signing.rs` (above tests):
```rust
use alloy_primitives::{keccak256, B256};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct LimitType<'a> {
    pub tif: &'a str,
}
#[derive(Serialize)]
pub(crate) struct OrderTypeWire<'a> {
    pub limit: LimitType<'a>,
}
#[derive(Serialize)]
pub(crate) struct OrderWire<'a> {
    pub a: u32,
    pub b: bool,
    pub p: String,
    pub s: String,
    pub r: bool,
    pub t: OrderTypeWire<'a>,
}
#[derive(Serialize)]
pub(crate) struct OrderAction<'a> {
    pub r#type: &'a str,
    pub orders: Vec<OrderWire<'a>>,
    pub grouping: &'a str,
}
#[derive(Serialize)]
pub(crate) struct CancelWire {
    pub a: u32,
    pub o: u64,
}
#[derive(Serialize)]
pub(crate) struct CancelAction<'a> {
    pub r#type: &'a str,
    pub cancels: Vec<CancelWire>,
}

pub(crate) fn order_wire(
    asset: u32,
    is_buy: bool,
    px: f64,
    sz: f64,
    reduce_only: bool,
    tif: &str,
) -> Result<OrderWire<'_>, VenueError> {
    Ok(OrderWire {
        a: asset,
        b: is_buy,
        p: float_to_wire(px)?,
        s: float_to_wire(sz)?,
        r: reduce_only,
        t: OrderTypeWire { limit: LimitType { tif } },
    })
}

/// SDK `action_hash`: msgpack(action) ++ nonce(8 BE) ++ vault byte ++ (no expires).
pub(crate) fn action_hash<T: Serialize>(
    action: &T,
    nonce: u64,
    vault: Option<[u8; 20]>,
) -> Result<B256, VenueError> {
    let mut data = rmp_serde::to_vec_named(action)
        .map_err(|e| VenueError::VenueSpecific(format!("msgpack encode: {e}")))?;
    data.extend_from_slice(&nonce.to_be_bytes());
    match vault {
        None => data.push(0x00),
        Some(addr) => {
            data.push(0x01);
            data.extend_from_slice(&addr);
        }
    }
    Ok(keccak256(&data))
}
```

> NOTE: `r#type` serializes as field name `type` (raw identifier strips the `r#`). If rmp-serde emits `r#type`, switch to `#[serde(rename = "type")] pub typ: &'a str`. The connectionId vector will catch it.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p volarb-venues --lib signing::tests::connection_id_matches_sdk_vector`
Expected: PASS. If FAIL, the bytes diverged — debug msgpack output vs python (most likely `type` field name or int marker width).

- [ ] **Step 6: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/signing.rs crates/volarb-venues/Cargo.toml Cargo.lock
git commit -m "feat(venues): HL action wire types + msgpack action_hash (connectionId vector exact)"
```

---

### Task 3: EIP-712 phantom-agent signing (sig vectors)

**Files:**
- Modify: `crates/volarb-venues/src/hyperliquid/signing.rs`

**Interfaces:**
- Consumes: `action_hash`, `OrderAction`, `order_wire`, `float_to_int_for_hashing` (Tasks 1-2)
- Produces:
  - `#[derive(Serialize)] pub(crate) struct Signature { r: String, s: String, v: u64 }`
  - `pub(crate) fn sign_l1_action<T: Serialize>(signer: &PrivateKeySigner, action: &T, nonce: u64, vault: Option<[u8;20]>, is_mainnet: bool) -> Result<Signature, VenueError>`

- [ ] **Step 1: Write the failing test**

In tests module:
```rust
    use alloy_signer_local::PrivateKeySigner;

    const TEST_KEY: &str = "0x0123456789012345678901234567890123456789012345678901234567890123";

    #[derive(serde::Serialize)]
    struct Dummy<'a> {
        r#type: &'a str,
        num: u64,
    }

    #[test]
    fn sign_dummy_matches_sdk_vector() {
        let signer: PrivateKeySigner = TEST_KEY.parse().unwrap();
        let action = Dummy { r#type: "dummy", num: float_to_int_for_hashing(1000.0).unwrap() as u64 };
        let m = sign_l1_action(&signer, &action, 0, None, true).unwrap();
        assert_eq!(m.r, "0x53749d5b30552aeb2fca34b530185976545bb22d0b3ce6f62e31be961a59298");
        assert_eq!(m.s, "0x755c40ba9bf05223521753995abb2f73ab3229be8ec921f350cb447e384d8ed8");
        assert_eq!(m.v, 27);
        let t = sign_l1_action(&signer, &action, 0, None, false).unwrap();
        assert_eq!(t.r, "0x542af61ef1f429707e3c76c5293c80d01f74ef853e34b76efffcb57e574f9510");
        assert_eq!(t.s, "0x17b8b32f086e8cdede991f1e2c529f5dd5297cbe8128500e00cbaf766204a613");
        assert_eq!(t.v, 28);
    }

    #[test]
    fn sign_order_matches_sdk_vector() {
        let signer: PrivateKeySigner = TEST_KEY.parse().unwrap();
        let ow = order_wire(1, true, 100.0, 100.0, false, "Gtc").unwrap();
        let action = OrderAction { r#type: "order", orders: vec![ow], grouping: "na" };
        let m = sign_l1_action(&signer, &action, 0, None, true).unwrap();
        assert_eq!(m.r, "0xd65369825a9df5d80099e513cce430311d7d26ddf477f5b3a33d2806b100d78e");
        assert_eq!(m.s, "0x2b54116ff64054968aa237c20ca9ff68000f977c93289157748a3162b6ea940e");
        assert_eq!(m.v, 28);
        let t = sign_l1_action(&signer, &action, 0, None, false).unwrap();
        assert_eq!(t.r, "0x82b2ba28e76b3d761093aaded1b1cdad4960b3af30212b343fb2e6cdfa4e3d54");
        assert_eq!(t.s, "0x6b53878fc99d26047f4d7e8c90eb98955a109f44209163f52d8dc4278cbbd9f5");
        assert_eq!(t.v, 27);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-venues --lib signing::tests::sign_dummy_matches_sdk_vector`
Expected: FAIL (sign_l1_action not defined).

- [ ] **Step 3: Write minimal implementation**

Add to `signing.rs`:
```rust
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{eip712_domain, sol, SolStruct};

sol! {
    #[derive(Serialize)]
    struct Agent {
        string source;
        bytes32 connectionId;
    }
}

#[derive(Serialize)]
pub(crate) struct Signature {
    pub r: String,
    pub s: String,
    pub v: u64,
}

/// SDK `sign_l1_action`: action_hash → phantom agent → EIP-712 (Exchange/1/1337) → {r,s,v}.
pub(crate) fn sign_l1_action<T: Serialize>(
    signer: &PrivateKeySigner,
    action: &T,
    nonce: u64,
    vault: Option<[u8; 20]>,
    is_mainnet: bool,
) -> Result<Signature, VenueError> {
    let connection_id = action_hash(action, nonce, vault)?;
    let agent = Agent {
        source: if is_mainnet { "a".to_string() } else { "b".to_string() },
        connectionId: connection_id,
    };
    let domain = eip712_domain! {
        name: "Exchange",
        version: "1",
        chain_id: 1337,
        verifying_contract: alloy_primitives::Address::ZERO,
    };
    let signing_hash = agent.eip712_signing_hash(&domain);
    let sig = signer
        .sign_hash_sync(&signing_hash)
        .map_err(|e| VenueError::VenueSpecific(format!("sign: {e}")))?;
    Ok(Signature {
        r: format!("0x{:x}", sig.r()),
        s: format!("0x{:x}", sig.s()),
        v: 27 + sig.v() as u64,
    })
}
```

> NOTE on `r`/`s` formatting: SDK uses `to_hex(int)` which strips leading zeros (variable length, e.g. V3-cloid `0x41ae...` is 63 hex chars). `format!("0x{:x}", sig.r())` on a `U256`/`Uint` prints minimal hex (no leading zeros) → matches. If alloy prints fixed 64-width, switch to trimming leading zeros to match the SDK vectors.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p volarb-venues --lib signing::tests`
Expected: PASS (all signing tests). If r/s mismatch on width, apply the leading-zero-trim note.

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/signing.rs
git commit -m "feat(venues): HL phantom-agent EIP-712 sign_l1_action (dummy+order sig vectors exact)"
```

---

### Task 4: Red-team unit tests — formatting/determinism guards

**Files:**
- Modify: `crates/volarb-venues/src/hyperliquid/signing.rs`

**Interfaces:**
- Consumes: all of Task 1-3.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn msgpack_is_deterministic() {
        let a1 = order_wire(1, true, 100.0, 100.0, false, "Gtc").unwrap();
        let a2 = order_wire(1, true, 100.0, 100.0, false, "Gtc").unwrap();
        let act1 = OrderAction { r#type: "order", orders: vec![a1], grouping: "na" };
        let act2 = OrderAction { r#type: "order", orders: vec![a2], grouping: "na" };
        assert_eq!(
            rmp_serde::to_vec_named(&act1).unwrap(),
            rmp_serde::to_vec_named(&act2).unwrap()
        );
    }

    #[test]
    fn non_finite_and_lossy_inputs_error_not_panic() {
        assert!(order_wire(1, true, f64::NAN, 1.0, false, "Gtc").is_err());
        assert!(order_wire(1, true, f64::INFINITY, 1.0, false, "Gtc").is_err());
        // sub-1e-8 size loses precision in float_to_wire → error.
        assert!(order_wire(1, true, 100.0, 0.000000001, false, "Gtc").is_err());
    }

    #[test]
    fn nonce_big_endian_changes_hash() {
        let ow = order_wire(1, true, 100.0, 100.0, false, "Gtc").unwrap();
        let act = OrderAction { r#type: "order", orders: vec![ow], grouping: "na" };
        let h0 = action_hash(&act, 0, None).unwrap();
        let h1 = action_hash(&act, 1, None).unwrap();
        assert_ne!(h0, h1);
    }
```

- [ ] **Step 2: Run tests to verify they fail or pass**

Run: `cargo test -p volarb-venues --lib signing::tests`
Expected: these PASS immediately (engine already correct) — they are regression guards encoding WHY (Rule 9): a reordered/non-deterministic msgpack or a silent NaN→"NaN" string would break signing against the live venue.

- [ ] **Step 3: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/signing.rs
git commit -m "test(venues): HL signing red-team guards (determinism, non-finite, nonce)"
```

---

### Task 5: Exchange client + asset resolution + place()

**Files:**
- Create: `crates/volarb-venues/src/hyperliquid/exchange.rs`
- Modify: `crates/volarb-venues/src/hyperliquid/info.rs` (add asset-index resolution helper)
- Modify: `crates/volarb-venues/src/hyperliquid/mod.rs` (add `pub mod exchange;`, signer field, `place` impl)

**Interfaces:**
- Consumes: `signing::{OrderAction, order_wire, sign_l1_action, Signature}`, `info::InfoClient`.
- Produces:
  - `info`: `pub(crate) async fn asset_index(&self, dex: &str, coin: &str) -> Result<u32, VenueError>` (resolves builder-dex offset).
  - `exchange`: `pub(crate) struct ExchangeClient { http, base_url }` with `pub(crate) async fn post_action(&self, action_json, signature, nonce) -> Result<serde_json::Value, VenueError>`.
  - `mod.rs`: `place` returns real `OrderReceipt`.

- [ ] **Step 1: Write the failing test (asset index offset)**

In `info.rs` tests:
```rust
    #[test]
    fn builder_asset_index_applies_offset() {
        // perp_dex_index 1 (after the base perp dex 0), coin index 2 in universe →
        // 100000 + 1*10000 + 2 = 110002.
        assert_eq!(super::builder_asset_index(1, 2), 110_002);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-venues --lib info::tests::builder_asset_index_applies_offset`
Expected: FAIL (function not defined).

- [ ] **Step 3: Implement asset index + exchange client + place**

In `info.rs`:
```rust
/// Builder-dex asset index per HL convention (verify against live `perpDexs` ordering).
pub(crate) fn builder_asset_index(perp_dex_index: u32, coin_index_in_dex: u32) -> u32 {
    100_000 + perp_dex_index * 10_000 + coin_index_in_dex
}
```
Add an `async fn asset_index(&self, dex, coin)` that fetches `perpDexs` (to find `perp_dex_index` for `dex`) and per-dex `meta.universe` (to find `coin_index_in_dex` matching the bare coin), then returns `builder_asset_index(...)`. Reuse the existing `InfoClient` POST helper. On unknown dex/coin → `VenueError::VenueSpecific`.

Create `exchange.rs`:
```rust
//! Hyperliquid /exchange POST client (signed write actions).
use crate::VenueError;
use serde::Serialize;

const TIMEOUT_SECS: u64 = 10;

#[derive(Serialize)]
struct ExchangeRequest<'a, A: Serialize> {
    action: &'a A,
    nonce: u64,
    signature: &'a crate::hyperliquid::signing::Signature,
    #[serde(rename = "vaultAddress")]
    vault_address: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExchangeClient {
    http: reqwest::Client,
    base_url: String,
}

impl ExchangeClient {
    pub(crate) fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .expect("reqwest client");
        Self { http, base_url }
    }

    pub(crate) async fn post_action<A: Serialize>(
        &self,
        action: &A,
        signature: &crate::hyperliquid::signing::Signature,
        nonce: u64,
    ) -> Result<serde_json::Value, VenueError> {
        let body = ExchangeRequest { action, nonce, signature, vault_address: None };
        let resp = self
            .http
            .post(format!("{}/exchange", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| VenueError::Network(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(VenueError::RateLimited);
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| VenueError::Network(e.to_string()))?;
        Ok(v)
    }
}
```

In `mod.rs`: add `pub mod exchange;`. Add fields to `HyperliquidAdapter`: `signer: Option<alloy_signer_local::PrivateKeySigner>` and `exchange: exchange::ExchangeClient`. Builder reads key from env:
```rust
// in builder build(): signer from env, never logged.
let signer = std::env::var("HL_TESTNET_PRIVATE_KEY")
    .ok()
    .and_then(|k| k.parse::<alloy_signer_local::PrivateKeySigner>().ok());
```
Implement `place`:
```rust
async fn place(&self, order: PlaceOrder) -> Result<OrderReceipt, VenueError> {
    let signer = self.signer.as_ref().ok_or(VenueError::Unauthorized)?;
    let coin = order.market.venue_market.rsplit(':').next().unwrap_or("");
    let asset = self.info.asset_index(&self.dex, coin).await?;
    let is_buy = matches!(order.side, volarb_core::Side::Up);
    let ow = signing::order_wire(asset, is_buy, order.price, order.size, false, "Gtc")?;
    let action = signing::OrderAction { r#type: "order", orders: vec![ow], grouping: "na" };
    let nonce = self.next_nonce();
    let sig = signing::sign_l1_action(signer, &action, nonce, None, !self.testnet)?;
    let resp = self.exchange.post_action(&action, &sig, nonce).await?;
    parse_order_receipt(&resp)
}
```
Add a `next_nonce(&self)` (Task 8 makes it monotonic; for now `get_timestamp_ms`) and `parse_order_receipt` that reads `response.data.statuses[0]` → `resting.oid` or `filled.{oid,totalSz}`, mapping HL error strings to `VenueError::VenueSpecific`.

> `Side::Up` → buy mapping: confirm against `volarb_core::Side` variants; HyperOdd "up" outcome = long. If `Side` lacks `Up`, use the actual variant names.

- [ ] **Step 4: Run tests + build**

Run: `cargo test -p volarb-venues --lib && cargo build -p volarb-venues`
Expected: PASS (asset-index unit test) + clean build. Replace the old `place` stub test in `mod.rs` `write_methods_fail_loud` — `place`/`cancel` no longer fail-loud; keep only `settle` in that assertion (see Task 6 note).

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/exchange.rs crates/volarb-venues/src/hyperliquid/info.rs crates/volarb-venues/src/hyperliquid/mod.rs
git commit -m "feat(venues): HL /exchange client + builder asset index + place()"
```

---

### Task 6: cancel()

**Files:**
- Modify: `crates/volarb-venues/src/hyperliquid/mod.rs`

**Interfaces:**
- Consumes: `signing::{CancelAction, CancelWire, sign_l1_action}`, `exchange::ExchangeClient`.

- [ ] **Step 1: Update the stub test**

In `mod.rs` tests, change `write_methods_fail_loud` to assert ONLY `settle` fails loud (place/cancel now require a signer → without env key they return `Unauthorized`, not `VenueSpecific`):
```rust
    #[tokio::test]
    async fn settle_fails_loud_place_cancel_need_signer() {
        let a = HyperliquidAdapter::builder().build(); // no env key
        assert!(matches!(a.settle(mref()).await, Err(VenueError::VenueSpecific(_))));
        assert!(matches!(
            a.place(PlaceOrder { market: mref(), side: Side::Up, price: 0.2, size: 1.0 }).await,
            Err(VenueError::Unauthorized)
        ));
        assert!(matches!(a.cancel(OrderId("123".into())).await, Err(VenueError::Unauthorized)));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-venues --lib hyperliquid::tests::settle_fails_loud_place_cancel_need_signer`
Expected: FAIL (cancel still returns `VenueSpecific`).

- [ ] **Step 3: Implement cancel**

`OrderId` carries the numeric oid as a string; cancel needs the asset too. Since `cancel(order_id)` lacks a market, encode `"<coin>:<oid>"` in `OrderId` at `place` time, OR (simpler, matches trait) parse oid and require the adapter's single configured dex+coin. Implement by parsing `OrderId` as `"<coin>:<oid>"`:
```rust
async fn cancel(&self, order_id: OrderId) -> Result<(), VenueError> {
    let signer = self.signer.as_ref().ok_or(VenueError::Unauthorized)?;
    let (coin, oid_str) = order_id.0.split_once(':')
        .ok_or_else(|| VenueError::VenueSpecific("OrderId must be '<coin>:<oid>'".into()))?;
    let oid: u64 = oid_str.parse()
        .map_err(|_| VenueError::VenueSpecific("OrderId oid not numeric".into()))?;
    let asset = self.info.asset_index(&self.dex, coin).await?;
    let action = signing::CancelAction {
        r#type: "cancel",
        cancels: vec![signing::CancelWire { a: asset, o: oid }],
    };
    let nonce = self.next_nonce();
    let sig = signing::sign_l1_action(signer, &action, nonce, None, !self.testnet)?;
    let resp = self.exchange.post_action(&action, &sig, nonce).await?;
    parse_cancel_response(&resp)
}
```
Make `place`'s `OrderReceipt.order_id` carry `"<coin>:<oid>"` so cancel round-trips. Add `parse_cancel_response` mapping `"success"` → `Ok(())`, HL error string → `VenueError::VenueSpecific`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-venues --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/mod.rs
git commit -m "feat(venues): HL cancel() via signed cancel action"
```

---

### Task 7: position() clearinghouseState parser + non-empty fixture

**Files:**
- Modify: `crates/volarb-venues/src/hyperliquid/info.rs`
- Modify: `crates/volarb-venues/src/hyperliquid/mod.rs` (`position` impl)
- Create: `crates/volarb-venues/tests/fixtures/clearinghouse_state_nonempty.json` (hand-built, no real key/PII)

**Interfaces:**
- Consumes: `ExtPosition`, `MarketRef` (from `lib.rs`).
- Produces: `info`: `pub(crate) fn parse_position(state: &serde_json::Value, market: &MarketRef) -> Result<Option<ExtPosition>, VenueError>`.

- [ ] **Step 1: Create the fixture**

`clearinghouse_state_nonempty.json` — minimal HL `clearinghouseState` shape with one `assetPositions[]` entry for coin `BTCHOURLY` (szi, entryPx, unrealizedPnl). Use synthetic values, no address.
```json
{
  "assetPositions": [
    { "position": { "coin": "ho:BTCHOURLY", "szi": "3.0", "entryPx": "0.21", "unrealizedPnl": "0.06" } }
  ],
  "marginSummary": { "accountValue": "100.0" }
}
```

- [ ] **Step 2: Write the failing test**

In `info.rs` tests:
```rust
    #[test]
    fn parses_nonempty_position() {
        let raw = include_str!("../../tests/fixtures/clearinghouse_state_nonempty.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let m = crate::MarketRef {
            venue_market: "ho:BTCHOURLY".into(),
            strike: volarb_core::Strike(64000.0),
            expiry: volarb_core::Expiry { unix_ms: 1 },
        };
        let p = super::parse_position(&v, &m).unwrap().unwrap();
        assert_eq!(p.size, 3.0);
        assert_eq!(p.entry_px, 0.21);
        assert_eq!(p.side, volarb_core::Side::Up); // szi > 0 → long/Up
    }

    #[test]
    fn flat_market_returns_none() {
        let v: serde_json::Value = serde_json::json!({ "assetPositions": [] });
        let m = crate::MarketRef {
            venue_market: "ho:BTCHOURLY".into(),
            strike: volarb_core::Strike(1.0),
            expiry: volarb_core::Expiry { unix_ms: 1 },
        };
        assert!(super::parse_position(&v, &m).unwrap().is_none());
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p volarb-venues --lib info::tests::parses_nonempty_position`
Expected: FAIL (parse_position not defined).

- [ ] **Step 4: Implement parse_position + wire into position()**

`parse_position` finds the `assetPositions[]` entry whose `position.coin == market.venue_market`; if absent → `Ok(None)` (genuinely flat — this is real data, not a fabricated stub, so `None` is honest here, unlike pt1). Parse `szi` (sign → `Side`), `entryPx`, `unrealizedPnl`; malformed numbers → `VenueError::VenueSpecific`. In `mod.rs` `position`, replace the loud stub: require `user`, fetch `clearinghouseState`, call `parse_position`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p volarb-venues --lib info::tests`
Expected: PASS (2 tests). Remove/adjust the pt1 `position_with_user_fails_loud_not_flat` test in `mod.rs` (no longer fails loud — it now parses).

- [ ] **Step 6: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/info.rs crates/volarb-venues/src/hyperliquid/mod.rs crates/volarb-venues/tests/fixtures/clearinghouse_state_nonempty.json
git commit -m "feat(venues): HL position() clearinghouseState parser + non-empty fixture"
```

---

### Task 8: Monotonic nonce source

**Files:**
- Modify: `crates/volarb-venues/src/hyperliquid/mod.rs`

**Interfaces:**
- Produces: `fn next_nonce(&self) -> u64` strictly increasing across calls.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn nonce_is_strictly_increasing() {
        let a = HyperliquidAdapter::builder().build();
        let mut last = 0u64;
        for _ in 0..1000 {
            let n = a.next_nonce();
            assert!(n > last, "nonce not increasing: {n} <= {last}");
            last = n;
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p volarb-venues --lib hyperliquid::tests::nonce_is_strictly_increasing`
Expected: FAIL (timestamp ms collides within a tight loop → not strictly increasing, or next_nonce missing).

- [ ] **Step 3: Implement monotonic nonce**

Add `last_nonce: std::sync::Arc<std::sync::atomic::AtomicU64>` to the adapter (init 0). `next_nonce` = `max(now_ms, last+1)` via a CAS loop on the atomic, so equal-ms calls bump by 1 (red-team #1: replay/collision).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p volarb-venues --lib hyperliquid::tests::nonce_is_strictly_increasing`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/volarb-venues/src/hyperliquid/mod.rs
git commit -m "feat(venues): HL strictly-monotonic nonce (collision guard)"
```

---

### Task 9: Live no-funds margin-error probe (Q3 #2)

**Files:**
- Modify: `crates/volarb-venues/tests/live_testnet.rs`

**Interfaces:**
- Consumes: full adapter. Env-gated on `HL_TESTNET_PRIVATE_KEY` + `HL_TESTNET_ACCOUNT_ADDRESS`.

- [ ] **Step 1: Write the live test**

```rust
// Live, no funds. Proves the SIGNATURE is accepted by the venue: a signed order on an
// unfunded account must fail with a MARGIN/funds error, NOT a signature/format error
// (HL verifies the signature before the margin check). Also exercises the builder
// asset-index path (a wrong index returns a different error).
#[tokio::test]
#[ignore = "live testnet; requires HL_TESTNET_PRIVATE_KEY"]
async fn live_signed_place_fails_on_margin_not_signature() {
    let Ok(_key) = std::env::var("HL_TESTNET_PRIVATE_KEY") else { return; };
    let user = std::env::var("HL_TESTNET_ACCOUNT_ADDRESS").expect("account addr");
    let a = HyperliquidAdapter::builder().user(user).build();
    let market = MarketRef {
        venue_market: "ho:BTCHOURLY".into(),
        strike: Strike(64000.0),
        expiry: Expiry { unix_ms: 1_781_348_700_000 },
    };
    let res = a.place(PlaceOrder { market, side: Side::Up, price: 0.05, size: 1.0 }).await;
    let err = res.expect_err("unfunded place must fail");
    let msg = format!("{err:?}").to_lowercase();
    assert!(
        !msg.contains("signature") && !msg.contains("does not exist") && !msg.contains("deserialize"),
        "signature/format rejected — signing is WRONG: {msg}"
    );
    // expected: margin/insufficient/funds-class error → signature was accepted.
    eprintln!("live place rejected as expected: {msg}");
}
```

- [ ] **Step 2: Run the live probe (manual, gated)**

Run: `cargo test -p volarb-venues --test live_testnet live_signed_place_fails_on_margin_not_signature -- --ignored --nocapture`
Expected: PASS — error message is margin-class, not signature-class. If it asserts-fail with "signature rejected", the signing engine diverges from live (revisit Tasks 2-3 against current `signing.py`). Without env key → returns early (no-op).

- [ ] **Step 3: Commit**

```bash
git add crates/volarb-venues/tests/live_testnet.rs
git commit -m "test(venues): HL live no-funds margin probe (signature acceptance)"
```

---

### Task 10: Full-workspace gate + notes

**Files:**
- Modify: `move/move-notes.md` is N/A; update `tasks/progress.md` is handled by /save-progress.

- [ ] **Step 1: Full gate**

Run:
```bash
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt --check
```
Expected: all green (live `#[ignore]` tests skipped).

- [ ] **Step 2: Monkey test pass (project rule test.md)**

Manually probe extremes against the signing engine: empty coin string, asset index for unknown dex, oid `u64::MAX`, size at the 1e-8 precision boundary. Confirm each returns a `VenueError`, never a panic. Fold any gap into a unit test in `signing.rs`/`info.rs`.

- [ ] **Step 3: Commit (if monkey tests added)**

```bash
git add crates/volarb-venues/src/hyperliquid/
git commit -m "test(venues): HL write-path monkey tests"
```

---

## Self-Review

**Spec coverage:** place (T5) ✓, cancel (T6) ✓, position completion (T7) ✓, settle stays stub (T6 test asserts) ✓, signing engine via vectors (T1-T3, Q3#1) ✓, live margin probe (T9, Q3#2) ✓, secrets handling (T5 env, no logging) ✓, red-team nonce/determinism/formatting (T4/T8) ✓, asset-index offset (T5) ✓.

**Out of scope (loud):** `settle`, `quote_stream` multi-market, funded real-fill capture — all explicitly deferred in the design doc.

**Open risks flagged for the implementer:**
1. `r#type` msgpack field name — verify it emits `type` not `r#type` (T2 note); connectionId vector catches it.
2. alloy `Signature` r/s hex width vs SDK leading-zero-stripped `to_hex` (T3 note); vectors catch it.
3. Builder asset-index formula is the suspected `100000 + dex*10000 + idx` — only the live probe (T9) confirms it against real HL; if the probe shows a non-margin error, re-derive the offset from `perpDexs` before claiming done (Rule 12).
4. `volarb_core::Side` variant names (`Up`/`Down`?) — confirm before using in T5/T7.

## Post-implementation

Run `/dual-review` (round 1 codex generic + round 2 project rules) on the diff per dev-rules — this is non-Move TS/Rust, generic reviewer is allowed. Then `/save-progress`.
