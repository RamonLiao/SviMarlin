//! WHY: executor crash-resume (spec §3.5) and the indexer persist these types as JSON.
//! If any public type fails to round-trip, resumed state or indexed rows corrupt silently.

use volarb_core::{
    Expiry, Position, Quote, SVIParams, SVISurface, Side, Smile, Strike, UsdcAmount, VolPoints,
};

fn roundtrip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("serialize");
    let back: T = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(&back, value, "round-trip mismatch for {json}");
}

#[test]
fn all_public_types_json_roundtrip() {
    let strike = Strike(64_250.5);
    let expiry = Expiry {
        unix_ms: 1_700_000_000_123,
    };
    let amt = UsdcAmount(123_456_789);

    roundtrip(&strike);
    roundtrip(&expiry);
    roundtrip(&amt);
    roundtrip(&VolPoints(72.5));
    roundtrip(&Quote {
        bid: 0.51,
        ask: 0.53,
        strike,
        expiry,
        ts_ms: 1_700_000_000_000,
    });
    roundtrip(&Position {
        side: Side::Up,
        size: amt,
        entry_iv: VolPoints(72.5),
        strike,
        expiry,
    });

    let mut surface = SVISurface {
        as_of_ms: 1_700_000_000_000,
        ..Default::default()
    };
    surface.per_expiry.insert(
        expiry.unix_ms,
        Smile {
            params: SVIParams {
                a: 0.04,
                b: 0.4,
                rho: -0.3,
                m: 0.0,
                sigma: 0.1,
            },
            forward: 64_000.0,
        },
    );
    roundtrip(&surface);
}
