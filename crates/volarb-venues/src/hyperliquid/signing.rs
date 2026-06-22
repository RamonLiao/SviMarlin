//! Hyperliquid phantom-agent signing (msgpack action-hash + EIP-712).
//! Verified byte-exact against hyperliquid-python-sdk `signing.py` test vectors.

use crate::VenueError;

/// Mirror of SDK `float_to_wire`: 8dp fixed, error on rounding loss, strip trailing zeros.
#[allow(dead_code)]
pub(crate) fn float_to_wire(x: f64) -> Result<String, VenueError> {
    if !x.is_finite() {
        return Err(VenueError::VenueSpecific(format!(
            "non-finite price/size: {x}"
        )));
    }
    let rounded = format!("{x:.8}");
    let reparsed: f64 = rounded
        .parse()
        .map_err(|_| VenueError::VenueSpecific("float_to_wire parse failed".into()))?;
    if (reparsed - x).abs() >= 1e-12 {
        return Err(VenueError::VenueSpecific(format!(
            "float_to_wire rounding: {x}"
        )));
    }
    // strip trailing zeros and a trailing dot; normalize "-0".
    let mut s = rounded
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string();
    if s.is_empty() || s == "-0" {
        s = "0".to_string();
    }
    Ok(s)
}

/// Mirror of SDK `float_to_int_for_hashing`: round(x*1e8), error on rounding loss >= 1e-3.
#[allow(dead_code)]
pub(crate) fn float_to_int_for_hashing(x: f64) -> Result<i64, VenueError> {
    let with_decimals = x * 1e8;
    if (with_decimals.round() - with_decimals).abs() >= 1e-3 {
        return Err(VenueError::VenueSpecific(format!(
            "float_to_int rounding: {x}"
        )));
    }
    Ok(with_decimals.round() as i64)
}

use alloy_primitives::{B256, keccak256};
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{SolStruct, eip712_domain, sol};
use serde::Serialize;

sol! {
    #[derive(Serialize)]
    struct Agent {
        string source;
        bytes32 connectionId;
    }
}

#[derive(Serialize)]
#[allow(dead_code)]
pub(crate) struct Signature {
    pub r: String,
    pub s: String,
    pub v: u64,
}

/// SDK `sign_l1_action`: action_hash → phantom agent → EIP-712 (Exchange/1/1337) → {r,s,v}.
#[allow(dead_code)]
pub(crate) fn sign_l1_action<T: Serialize>(
    signer: &PrivateKeySigner,
    action: &T,
    nonce: u64,
    vault: Option<[u8; 20]>,
    is_mainnet: bool,
) -> Result<Signature, VenueError> {
    let connection_id = action_hash(action, nonce, vault)?;
    let agent = Agent {
        source: if is_mainnet { "a" } else { "b" }.to_string(),
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
#[allow(dead_code)]
pub(crate) struct OrderAction<'a> {
    pub r#type: &'a str,
    pub orders: Vec<OrderWire<'a>>,
    pub grouping: &'a str,
}
#[derive(Serialize)]
#[allow(dead_code)]
pub(crate) struct CancelWire {
    pub a: u32,
    pub o: u64,
}
#[derive(Serialize)]
#[allow(dead_code)]
pub(crate) struct CancelAction<'a> {
    pub r#type: &'a str,
    pub cancels: Vec<CancelWire>,
}

#[allow(dead_code)]
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
        t: OrderTypeWire {
            limit: LimitType { tif },
        },
    })
}

/// SDK `action_hash`: msgpack(action) ++ nonce(8 BE) ++ vault byte ++ (no expires).
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let action = Dummy {
            r#type: "dummy",
            num: float_to_int_for_hashing(1000.0).unwrap() as u64,
        };
        let m = sign_l1_action(&signer, &action, 0, None, true).unwrap();
        assert_eq!(
            m.r,
            "0x53749d5b30552aeb2fca34b530185976545bb22d0b3ce6f62e31be961a59298"
        );
        assert_eq!(
            m.s,
            "0x755c40ba9bf05223521753995abb2f73ab3229be8ec921f350cb447e384d8ed8"
        );
        assert_eq!(m.v, 27);
        let t = sign_l1_action(&signer, &action, 0, None, false).unwrap();
        assert_eq!(
            t.r,
            "0x542af61ef1f429707e3c76c5293c80d01f74ef853e34b76efffcb57e574f9510"
        );
        assert_eq!(
            t.s,
            "0x17b8b32f086e8cdede991f1e2c529f5dd5297cbe8128500e00cbaf766204a613"
        );
        assert_eq!(t.v, 28);
    }

    #[test]
    fn sign_order_matches_sdk_vector() {
        let signer: PrivateKeySigner = TEST_KEY.parse().unwrap();
        let ow = order_wire(1, true, 100.0, 100.0, false, "Gtc").unwrap();
        let action = OrderAction {
            r#type: "order",
            orders: vec![ow],
            grouping: "na",
        };
        let m = sign_l1_action(&signer, &action, 0, None, true).unwrap();
        assert_eq!(
            m.r,
            "0xd65369825a9df5d80099e513cce430311d7d26ddf477f5b3a33d2806b100d78e"
        );
        assert_eq!(
            m.s,
            "0x2b54116ff64054968aa237c20ca9ff68000f977c93289157748a3162b6ea940e"
        );
        assert_eq!(m.v, 28);
        let t = sign_l1_action(&signer, &action, 0, None, false).unwrap();
        assert_eq!(
            t.r,
            "0x82b2ba28e76b3d761093aaded1b1cdad4960b3af30212b343fb2e6cdfa4e3d54"
        );
        assert_eq!(
            t.s,
            "0x6b53878fc99d26047f4d7e8c90eb98955a109f44209163f52d8dc4278cbbd9f5"
        );
        assert_eq!(t.v, 27);
    }

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

    #[test]
    fn msgpack_is_deterministic() {
        let a1 = order_wire(1, true, 100.0, 100.0, false, "Gtc").unwrap();
        let a2 = order_wire(1, true, 100.0, 100.0, false, "Gtc").unwrap();
        let act1 = OrderAction {
            r#type: "order",
            orders: vec![a1],
            grouping: "na",
        };
        let act2 = OrderAction {
            r#type: "order",
            orders: vec![a2],
            grouping: "na",
        };
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
        let act = OrderAction {
            r#type: "order",
            orders: vec![ow],
            grouping: "na",
        };
        let h0 = action_hash(&act, 0, None).unwrap();
        let h1 = action_hash(&act, 1, None).unwrap();
        assert_ne!(h0, h1);
    }
}
