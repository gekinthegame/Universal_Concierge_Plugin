//! CID construction. The multiformats contract, fixed for gateway portability:
//! every block is **CIDv1 / dag-cbor (0x71) / sha2-256 (0x12)**.
//!
//! This module re-exports the multiformats `Cid` so the rest of the crate
//! refers to `crate::cid::Cid` and never touches the extern crate directly.

use multihash::Multihash;
use sha2::{Digest, Sha256};

pub use ::cid::Cid;

/// IPLD dag-cbor codec code.
pub const DAG_CBOR: u64 = 0x71;
/// sha2-256 multihash code.
pub const SHA2_256: u64 = 0x12;

/// Compute the CID for already-DAG-CBOR-encoded bytes.
///
/// This is the single source of CIDs in the system. Backends must store/return
/// blocks under the CID this produces — never re-derive it (see
/// `ADDING_A_BACKEND.md`, "preserve the CID").
pub fn compute(bytes: &[u8]) -> Cid {
    let digest = Sha256::digest(bytes);
    // sha2-256 is always 32 bytes, which fits a Multihash<64>; wrap only fails
    // when the digest exceeds the allocation, so this is infallible here.
    let mh = Multihash::<64>::wrap(SHA2_256, digest.as_slice())
        .expect("sha2-256 digest is 32 bytes; fits Multihash<64>");
    Cid::new_v1(DAG_CBOR, mh)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical DAG-CBOR for `{ "hello": "world" }`.
    ///
    /// CBOR breakdown:
    /// - `a1`: map with one pair
    /// - `65 68656c6c6f`: text key "hello"
    /// - `65 776f726c64`: text value "world"
    const DAG_CBOR_HELLO_WORLD: &[u8] = &[
        0xa1, 0x65, b'h', b'e', b'l', b'l', b'o', 0x65, b'w', b'o', b'r', b'l', b'd',
    ];

    /// Golden CID for `DAG_CBOR_HELLO_WORLD`, generated with the IPFS
    /// multiformats contract:
    ///
    /// `ipfs dag put --input-codec=dag-json --store-codec=dag-cbor --hash=sha2-256 fixture.json`
    ///
    /// where `fixture.json` is `{"hello":"world"}`.
    const DAG_CBOR_HELLO_WORLD_CID: &str =
        "bafyreidykglsfhoixmivffc5uwhcgshx4j465xwqntbmu43nb2dzqwfvae";

    #[test]
    fn compute_matches_known_dagcbor_golden_cid() {
        let cid = compute(DAG_CBOR_HELLO_WORLD);

        assert_eq!(
            cid.to_string(),
            DAG_CBOR_HELLO_WORLD_CID,
            "CID must match the external dag-cbor/sha2-256 golden value"
        );
    }

    #[test]
    fn compute_builds_cidv1_dagcbor_sha256() {
        let cid = compute(DAG_CBOR_HELLO_WORLD);

        assert_eq!(cid.version(), ::cid::Version::V1, "must be CIDv1");
        assert_eq!(cid.codec(), DAG_CBOR, "codec must be dag-cbor");

        let mh = cid.hash();
        assert_eq!(mh.code(), SHA2_256, "multihash must be sha2-256");
        assert_eq!(mh.size(), 32, "sha2-256 digest is 32 bytes");
    }

    #[test]
    fn cid_is_deterministic() {
        assert_eq!(compute(DAG_CBOR_HELLO_WORLD), compute(DAG_CBOR_HELLO_WORLD));
        assert_ne!(compute(DAG_CBOR_HELLO_WORLD), compute(b"hello worle"));
    }

    #[test]
    fn cid_string_roundtrips_through_base32_multibase() {
        let cid = compute(DAG_CBOR_HELLO_WORLD);
        let s = cid.to_string();
        assert!(
            s.starts_with('b'),
            "CIDv1 string is base32 multibase ('b…')"
        );
        let parsed: Cid = s.parse().expect("round-trip parse");
        assert_eq!(parsed, cid);
    }
}
