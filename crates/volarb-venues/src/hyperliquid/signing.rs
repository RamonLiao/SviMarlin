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
}
