# Design — Hyperliquid HIP-4 Venue Adapter (write path) — TODO #6 Part 2

> Date: 2026-06-22
> Status: design (pre-implementation)
> Scope: `place` + `cancel` + `position` parse completion, in `volarb-venues/hyperliquid`.
> Out of scope (next round): `settle` (HyperOdd expiry-settlement semantics unverified → loud stub kept).
> Depends on read path landed pt1 (merge tip `933213e`).

## Context

pt1 landed the read path (quote/quote_stream/position-stub/health) against live testnet. The
`VenueAdapter` trait is ADR-003-locked; write-side satellite types (`PlaceOrder`/`OrderReceipt`/
`OrderId`/`SettleReceipt`) already exist with placeholder fields marked "finalized in the signing
round". **This round needs no trait-signature change** — it replaces three `VenueSpecific` stubs
(`place`/`cancel`) + the `position` parse stub with real implementations. `settle` stays a loud stub.

## Brainstorming decisions (locked — do not relitigate)

- **Q1 scope = A**: place + cancel + position completion. `settle` deferred (HyperOdd expiry/redeem
  semantics not yet pinned; do not fold uncertainty into the signing round).
- **Q2 signing crate = A**: `alloy-signer-local` + `alloy-primitives` (keccak256, B256, recoverable
  secp256k1 sign, address derivation) + `rmp-serde` (msgpack). HL is a **phantom-agent** scheme, not
  a standard EIP-712 dApp domain → we assemble the action-hash / connectionId ourselves, but hand the
  error-prone primitives (keccak, recoverable sign, address recovery) to the audited alloy lib.
  No hand-rolled k256, no `ethers` (maintenance mode).
- **Q3 verification = 1+2 stacked**:
  1. Offline unit test: byte-exact cross-check against Hyperliquid python SDK `signing.py` published
     **test vectors** (action → action-hash → signature). Deterministic, CI-safe, no funds, directly
     proves the signing engine is correct.
  2. Live `#[ignore]` no-funds **margin-error probe**: a fully-signed `place` hits testnet and is
     expected to return an `insufficient-margin`-class error, **not** a signature error — because HL
     verifies the signature *before* the margin check. Proves the wire path end-to-end.
  3. Real fill (funds + non-empty position fixture) → **not this round** (funds in flight via CCTP).

## HL signing scheme (phantom-agent) — to be pinned byte-exact against `signing.py`

> ⚠️ **Real schema first (lessons 2026-06-13).** The exact msgpack field order, connectionId
> assembly, and EIP-712 domain constants are load-bearing and easy to get wrong from memory. The
> implementation TDD pulls the canonical steps + test vectors from the hyperliquid-python-sdk
> `hyperliquid/utils/signing.py` and asserts byte-exact equality. The sketch below is the *shape*
> the tests will lock down, not a substitute for reading the source.

Action signing pipeline (per `signing.py` `sign_l1_action`):

1. Build the **action** object (order / cancel) with HL's exact field names + ordering.
2. `action_hash(action, vault_address, nonce)`:
   - `data = msgpack.packb(action)` (deterministic field order — load-bearing)
   - append `nonce` as 8-byte big-endian
   - append `0x00` if `vault_address is None` else `0x01 || vault_address_bytes`
   - `connection_id = keccak256(data)`
3. **phantom agent** = `{ "source": "a" (mainnet) | "b" (testnet), "connectionId": connection_id }`
4. EIP-712 sign the phantom agent under domain
   `{ name: "Exchange", version: "1", chainId: 1337, verifyingContract: 0x00..0 }`
   with type `Agent { source: string, connectionId: bytes32 }`.
   - ⚠️ chainId is the **fixed L1 signing chainId (1337)**, independent of testnet/mainnet; testnet vs
     mainnet is encoded in `source` ("b"/"a"). This is the classic footgun — the test vector pins it.
5. Recoverable signature → `{ r, s, v }`. Exchange request body =
   `{ "action": action, "nonce": nonce, "signature": {r,s,v}, "vaultAddress": null }`.

### `order` action shape (to verify against source)

```
action = {
  "type": "order",
  "orders": [{
    "a": asset_index,          // u32 — builder-dex asset index (see asset-index mapping below)
    "b": is_buy,               // bool
    "p": price_str,            // string, HL price formatting rules
    "s": size_str,             // string, szDecimals from meta
    "r": reduce_only,          // bool
    "t": { "limit": { "tif": "Gtc" | "Ioc" | "Alo" } }
  }],
  "grouping": "na"
}
```

### `cancel` action shape

```
action = { "type": "cancel", "cancels": [{ "a": asset_index, "o": oid }] }
```

`OrderId` is the opaque string wrapper; for HL it carries the numeric `oid` returned by `place`
(`response.data.statuses[].resting.oid` / `filled.oid`). cloid path deferred unless needed.

### Asset-index mapping (builder dex) — verify, do not guess

pt1 TODO note: builder-dex asset index is **not** the bare universe index — HL offsets builder-dex
assets (suspected `100000 + dex_idx*10000 + asset_idx` form). The actual index must be pulled at
runtime: `meta` per-dex `universe[]` order + the builder-dex offset rule confirmed from `signing.py`
/ docs, then asserted by the live margin-probe (a wrong asset index returns a *different* error than
insufficient-margin, so the probe doubles as an index check).

## Module structure (delta over pt1)

```
src/hyperliquid/
  mod.rs        place/cancel → real impl (was loud stub); wires signer + exchange client
  signing.rs    NEW: action-hash (msgpack + nonce + vault byte), phantom-agent EIP-712 sign → {r,s,v}
  exchange.rs   NEW: POST /exchange client; build order/cancel action, send signed body, parse response
  info.rs       extend: clearinghouseState parser → ExtPosition (position() completion)
  market.rs     asset-index resolution (name → builder-dex asset index)
```

`settle` stays in `mod.rs` as a loud `VenueSpecific` unimplemented error.

### Secrets handling (locked 2026-06-14)

Private key from `.env` (`HL_TESTNET_PRIVATE_KEY`, git-ignored) via `std::env::var`. Key never
enters git / `.rs` / fixtures / logs / error messages. Signing fixtures store only the public
action + signature, never the key. Live tests gate on `#[ignore]` + env presence (skip if absent).

## Error mapping

- Signature/format rejected by `/exchange` → `VenueError::VenueSpecific` with HL's error string
  (loud — this is the failure the margin-probe asserts must *not* happen).
- Insufficient margin / no funds → distinct `VenueSpecific` (the *expected* probe outcome).
  (Not a hard `Unauthorized`; it is the success signal for the no-funds probe.)
- Network/timeout → `VenueError::Network` (reuse pt1 reqwest timeout config).
- Rate limited → `VenueError::RateLimited`.

## Red-team (write path = money path, per CLAUDE.md Red Team Protocol)

1. **Nonce replay / collision** → HL nonce must be unique monotonic (ms timestamp). Two orders same
   ms → second rejected. Mitigation: nonce source strictly increasing (track last, bump if equal).
2. **Wrong chainId in domain** (1337 vs 998) → signature verifies but for wrong agent → rejected.
   Mitigation: constant pinned by test vector (Q3 #1).
3. **msgpack field reorder** (serde map non-determinism) → wrong connectionId → silent wrong hash.
   Mitigation: explicit ordered struct (not `HashMap`), byte-exact vector assert.
4. **Price/size formatting** (float → HL string rules: sig-figs, szDecimals) → rejected or wrong
   order. Mitigation: format per meta szDecimals; covered by vector + probe.
5. **Key leak via error/log** → never format key into errors; signer holds key, errors carry only
   HL response strings.

## Verification plan (Q3 = 1+2)

- **Unit (offline, CI)**: `signing.rs` action-hash + signature byte-exact vs `signing.py` published
  test vectors (hard-coded expected bytes in a fixture). Covers order + cancel.
- **Unit (offline)**: msgpack determinism (same action → same bytes), nonce big-endian encoding,
  price/size string formatting per szDecimals, monkey: NaN/negative/oversize → error not panic.
- **Live `#[ignore]` (env-gated, no funds)**: signed `place` → assert error is margin-class, **not**
  signature-class. Signed `cancel` of a bogus oid → assert "order not found"-class, not sig error.
- **Deferred (funded)**: real fill + non-empty `clearinghouseState` fixture → `position()` real-data
  test. This round ships `position()` parser + a hand-built non-empty fixture test (parse logic
  verified offline); live funded capture lands when CCTP funds arrive.

## Out of scope (loud)

- `settle` (HyperOdd redeem/expiry semantics unverified).
- `quote_stream` multi-market + name→(strike,expiry) reverse-derivation (pt1 known limitation).
- Real funded fill / live position capture (funds in flight).
```
