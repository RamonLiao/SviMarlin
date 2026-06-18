# On-chain Pricing Decompile Findings (Plan B prerequisite)

> Spike date: 2026-05-31
> Package: `0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138` (testnet `predict-testnet-4-16`, Immutable)
> Method: fetched module bytecode via `sui_getObject {showBcs}`, base64-decoded each module → `.mv`, disassembled with `sui move disassemble`.
> Goal: extract SCALE + exact `compute_price` formula + `i64` semantics so the L0 parity layer can be ported **placeholder-free, bit-exact**.

## TL;DR (the load-bearing facts)

1. **SCALE = 1e9** (`1_000_000_000`), everywhere. Same as DeepBook `constants::FLOAT_SCALING`. `max_u64 = 18446744073709551615`.
2. **`compute_price(oracle, K) -> u64` returns `N(d2)` in 1e9 fixed-point** (a probability ∈ [0,1] scaled by 1e9). This is the UP (cash-or-nothing digital, r=0) price.
3. **`w` on-chain is RAW SVI total variance** — `a, b, σ, ρ, m` already bake in time-to-expiry. `compute_price` does **NOT** scale by `T`, has **NO** `√T`, and does **NOT** annualize. ⚠️ Our L1 `volarb-core::svi::sigma_at` *annualizes* (total variance / T → σ). **L0 must port the raw-`w` path and must NOT reuse the annualized-σ representation**, or the basis we measure is fictional.
4. **Predict reuses two DeepBook modules**: `oracle`/`compute_nd2` calls `math::mul` / `math::div` from DeepBook (`pkg fb28c4cb…6982`, aliased `math`), and `0math::{ln, sqrt, normal_cdf}` from Predict's own `math`. `i64` uses DeepBook `constants::max_u64`.
5. **`i64` is sign-magnitude** `{magnitude: u64, is_negative: bool}`, with `−0` normalized to `+0`. A Rust two's-complement port will diverge on rounding/zero boundaries — port the sign-magnitude semantics literally.

## `i64` (sign-magnitude, SCALE=1e9)

```
struct I64 { magnitude: u64, is_negative: bool }
zero()                 = {0, false}
from_u64(x)            = {x, false}
from_parts(m, neg)     = m==0 ? zero() : {m, neg}        // -0 → +0
neg(x)                 = x.mag==0 ? zero() : {x.mag, !x.neg}
add(a,b)               // standard sign-magnitude; abort(0) on magnitude overflow (a.mag > max_u64 - b.mag when same sign)
sub(a,b)               = add(a, neg(b))
mul_scaled(a,b)        = { (a.mag as u128 * b.mag as u128) / 1e9, sign = a.neg != b.neg }  // abort(0) if result > max_u64; from_parts → -0 normalized
div_scaled(a,b)        = { (a.mag as u128 * 1e9) / b.mag, sign = a.neg != b.neg }           // abort(1) if b.mag == 0; abort(0) on overflow
square_scaled(a)       = mul_scaled(a,a).magnitude  // u64, always non-negative
```

All `/` are integer floor division (u128 domain to avoid intermediate overflow).

## DeepBook `math` (SCALE=1e9) — only `mul`/`div` used by oracle

```
mul(a,b)      = floor(a*b / 1e9)              // round DOWN (plain)
div(a,b)      = floor(a*1e9 / b)              // round DOWN (plain)
mul_round_up  = mul + (a*b % 1e9 != 0 ? 1:0)  // NOT used in compute_nd2
div_round_up  = div + (a*1e9 % b != 0 ? 1:0)  // NOT used in compute_nd2
```
`compute_nd2` uses only the **round-down** `mul`/`div`. (computed in u128 then cast back to u64.)

## Predict `math` (SCALE=1e9)

- **`ln(x: u64) -> I64`** (x is 1e9-FP, x>0 asserted, abort(0) if x==0):
  - `x == 1e9` → `0`
  - `x < 1e9`  → `-ln(1e18 / x)`  (i.e. `neg(ln(1e9*1e9/x))`)
  - else: `(mantissa, shift) = normalize(x)`; `ln_u128(mantissa, shift)`.
  - `normalize(x)`: repeatedly `>>` by {32,16,8,4,2,1} accumulating `shift` until mantissa < 2·threshold... returns mantissa∈[1e9, 2e9) and integer `shift` (number of halvings).
  - `ln_u128(m, shift)`: atanh series. `y = (m - 1e9) / (m + 1e9)` (×1e9 scaled); `y2 = mul_scaled(y,y)`; Horner poly with coeffs `1/3,1/5,1/7,1/9,1/11,1/13` = `333333333, 200000000, 142857143, 111111111, 90909091, 76923077`; result = `2·y·(1 + poly·y2) + shift·ln2`. `ln2 = 693147180`.
- **`exp(x: &I64) -> u64`** (1e9-FP):
  - `x.mag == 0` → `1e9`
  - if positive (`!is_neg`): assert `mag <= 23638153699` (~23.638) else abort(1) [overflow guard].
  - `k = mag / ln2`; `r = mag - k·ln2`; `base = exp_series(r)`; then scale by `2^k`: positive → `base << shifts` (bit-by-bit, early-return 0 if it zeroes), negative → reciprocal `1e18/base` then `>> shifts`.
  - `exp_series(r)`: Taylor `Σ_{n=0..12} r^n/n!` in FP (term = `term·r/(n·1e9)`, accumulate while term>0).
- **`normal_cdf(x: &I64) -> u64`** (1e9-FP probability). |x| = `mag`:
  - `mag > 8e9` → `is_neg ? 0 : 1e9`
  - regime A `mag < 0.66291` (`662910000`): polynomial. `z = mag²/1e9`; West-style rational `P(z)/Q(z)` (coeffs consts 7–15, see below); `val = mag·P/Q /1e9`; `result = is_neg ? 0.5 - val : 0.5 + val` (0.5 = 1e9/2).
  - regime B `0.66291 ≤ mag < 5.656854249` (`5656854249`): rational `R(mag)` (consts 16–33) × `exp(-mag²/2)`; `tail = R·exp(...) /1e9`; `result = is_neg ? tail : 1e9 - tail`.
  - else (`mag ≥ 5.656854249`): `is_neg ? 0 : 1e9`.
  - **Constants (1e9-FP, from `math` Constants table)** — port verbatim, op order matters for floor-truncation parity:
    ```
    ln2 = 693147180
    regime A break = 662910000 ; regime B break = 5656854249 ; hard clamp = 8e9
    A coeffs (consts 7-15):  2235252035, 161028231069, 1067689485460, 18154981253344,
                             65682338, 47202581905, 976098551738, 10260932208619, 45507789335027
    B coeffs (consts 16-33): break=5656854249; 398941512, 8883149794, 93506656132, 597270276395,
                             2494537585290, 6848190450536, 11602651437647, 9842714838384,
                             11, 22266688044, 235387901782, 1519377599408, 6485558298267,
                             18615571640885, 34900952721146, 38912003286093, 19685429676860
    ln series coeffs (consts 34-39): 333333333, 200000000, 142857143, 111111111, 90909091, 76923077
    ```
    (Exact pairing of each const to its Horner step is in `/tmp/dis_math.txt` `normal_cdf_u128` / `ln_u128` block ranges — re-disassemble to regenerate; see "Reproduce" below.)
- **`sqrt(a: u64, b: u64) -> u64`** (FP sqrt of `a/b`-ish): asserts `0 < b ≤ 1e9` else abort(2). `inv = 1e9/b`; `result = sqrt_u128(a·inv·1e9) / inv`. **In `compute_nd2` always called with `b = 1e9`** → `inv = 1` → `result = sqrt_u128(a·1e9)` = floor(√(a·1e9)) = the standard FP sqrt of `a`.
  - `sqrt_u128(a)`: bit-length initial guess, **7 Newton iterations** `x = (x + a/x)/2`, then `if x·x > a: x -= 1`.

## `oracle::compute_price(oracle, K) -> u64` (the target)

```
public(friend) compute_price(oracle, K):
    if oracle.settlement_price is Some(s):
        return s > K ? 1e9 : 0          // STRICT >; equal → 0 (DOWN wins ties)
    else:
        return compute_nd2(oracle, K)

compute_nd2(oracle, K):   // all 1e9-FP
    F = oracle.forward_price();  assert F > 0  (abort 3)
    (a, b, rho, m, sigma) = oracle.svi          // a,b,sigma: u64 FP ; rho,m: I64 FP
    k        = math::ln( db_math::div(K, F) )    // = ln(K·1e9/F) = log-moneyness ln(K/F), I64
    diff     = k - m                              // I64  (i64::sub)
    inner    = i64::square_scaled(diff) + db_math::mul(sigma, sigma)   // [(k-m)² + σ²] in FP
    sqrt_t   = math::sqrt(inner, 1e9)             // √((k-m)²+σ²), I64 from_u64
    rho_term = i64::mul_scaled(rho, diff)         // ρ(k-m), I64
    bracket  = rho_term + sqrt_t                  // I64;  assert !is_negative (abort 4)
    w        = a + db_math::mul(b, bracket.magnitude)   // a + b·bracket = RAW total variance; assert w > 0 (abort 5)
    sqrt_w   = math::sqrt(w, 1e9)                  // √w, I64
    half_w   = i64::from_u64(w / 2)               // integer div
    numer    = k + half_w                          // ln(K/F) + w/2, I64
    d        = i64::div_scaled(numer, sqrt_w)      // (ln(K/F)+w/2)/√w, I64
    d2       = i64::neg(d)                          // = (ln(F/K) - w/2)/√w   [standard d2]
    return math::normal_cdf(d2)                     // N(d2) in 1e9-FP

binary_price_pair(oracle, K, clock) -> (up, down):
    up = compute_price(oracle, K);  down = 1e9 - up
```

### SVI total variance identity (raw, no time scaling)
```
w(k) = a + b·( ρ·(k − m) + √( (k − m)² + σ² ) ),   k = ln(K/F)
```
This is Gatheral raw-SVI **total** variance. The on-chain `OracleSVI` is **one object per expiry**; `a,b,σ,ρ,m` are the fitted raw params for that expiry, with `T` already absorbed. There is no `T` parameter to `compute_price`.

## Rust L0 port plan (Plan B implementation, next step)

- New module (proposed `volarb-pricing/src/onchain.rs` or a `volarb-core` fixed-point submodule — decide at impl time per DAG: it depends only on integer math, no external crates).
- Port, in order: `i64` (sign-magnitude over `u64`/`u128`) → DeepBook `mul`/`div` (round-down) → Predict `ln`/`exp`/`sqrt`/`normal_cdf` (verbatim constants + op order) → `compute_price`/`compute_nd2`.
- **Parity test (L3 harness)**: feed real on-chain `OracleSVI` (a,b,ρ,m,σ,forward,settlement) + a strike grid; assert Rust `compute_price` == on-chain `compute_price` **to the unit** (1e9-FP exact), via `sui_devInspect`/`dryRun` calling the on-chain function, or against `OracleSVIUpdated` event snapshots. Tick-exact, not float-tolerance.
- **Basis measurement**: L0 (executable, on-chain integer truncation) vs L1 (our float fair value). The *difference* = model basis = the edge gate input. Signal goes price-space (L0); SVI L1 fitter/surface stays for cross-strike interpolation + viz only.
- **No two's-complement shortcut**: replicate `i64` sign-magnitude incl. `−0→+0`, abort codes, and floor-division truncation. Two's-complement `i128` diverges at boundaries.

## Reproduce / regenerate
```bash
PKG=0xf5ea2b3749c65d6e56507cc35388719aadb28f9cab873696a2f8687f5c785138
curl -s https://fullnode.testnet.sui.io:443 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"sui_getObject","params":["'$PKG'",{"showBcs":true}]}' > pkg.json
python3 - <<'PY'
import json,base64; d=json.load(open('pkg.json')); mm=d['result']['data']['bcs']['moduleMap']
for m in ['math','oracle','i64']: open(f'{m}.mv','wb').write(base64.b64decode(mm[m]))
PY
for m in i64 math oracle; do sui move disassemble $m.mv > dis_$m.txt; done
# DeepBook math/constants live in pkg 0xfb28c4cbc6865bd1c897d26aecbe1f8792d1509a20ffec692c800660cbec6982
```
Disassembly is bytecode (`B0:`/opcode form), not source — formula was reconstructed by tracing the stack machine. Re-verify against this doc before trusting the port.
