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
