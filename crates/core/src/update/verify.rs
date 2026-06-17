//! The shared verification ladder (autoupdater §2) — the part that actually matters
//! for a scanner. Used by both the rules channel and the app channel.
//!
//! IPNS (and a release host) only ever provide a mutable "latest" pointer; they are
//! **not** the trust anchor (Decision D-AU-3). Trust comes from a detached Ed25519
//! signature over the artifact, verified against a public key **baked into the
//! binary**, plus a **monotonic epoch** (anti-rollback) and a **freshness window**.
//!
//! These functions are pure (no I/O) so the security invariants are exhaustively
//! unit-testable. The order matches the diagram in `AUTOUPDATER_PLAN.md §2`:
//!
//! 1. signature valid vs a baked pubkey?   else reject
//! 2. epoch > last_applied_epoch?           else reject (anti-rollback)
//! 3. cid matches manifest.cid & sha256?    else reject
//! 4. ts within freshness window?           else warn (stale publisher)
//! 5. yara-x compiles all rules?            else reject, keep LKG  (step 5 lives in
//!    `rules.rs` at activation time — it needs the decoded ruleset, not just bytes)

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::UpdateError;

/// The signed "latest" descriptor the publisher emits and the node verifies. Every
/// field except `sig` is covered by the signature (see [`Manifest::signing_bytes`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Monotonic anti-rollback counter. A replayed older-but-valid bundle is refused.
    pub epoch: u64,
    /// Human-readable ruleset version (e.g. a YARA Forge release date).
    pub version: String,
    /// Content id of the bundle the manifest points to (what the node fetches).
    pub cid: String,
    /// Lowercase hex SHA-256 of the bundle bytes — the cryptographic content check.
    pub sha256: String,
    /// Signed publish timestamp (unix seconds) — drives the freshness warning.
    pub ts: u64,
    /// The `yara-x` engine version the bundle was validated against (skew handling).
    pub engine: String,
    /// Base64 (standard, padded) Ed25519 signature over [`Manifest::signing_bytes`].
    pub sig: String,
}

/// The freshness outcome of step 4. Staleness is a *warning*, never a rejection —
/// rejecting on the clock would let a wrong local clock disable updates, and the
/// real anti-replay guarantee is the monotonic epoch, not the timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// The signed manifest is within the freshness window.
    Fresh,
    /// The newest signed manifest is older than the window — the publisher may be
    /// dark or compromised. Surface honestly; keep serving the (verified) rules.
    Stale { age_secs: u64 },
}

impl Manifest {
    /// The exact bytes the signature covers. A fixed domain-separation prefix and a
    /// newline-delimited field order make this deterministic and unambiguous — no
    /// JSON canonicalization pitfalls. The CI signer MUST reproduce this byte-for-byte.
    pub fn signing_bytes(&self) -> Vec<u8> {
        format!(
            "ucp-rules-manifest-v1\n{}\n{}\n{}\n{}\n{}\n{}",
            self.epoch, self.version, self.cid, self.sha256, self.ts, self.engine
        )
        .into_bytes()
    }

    /// Parse + validate a manifest from JSON bytes (the IPNS payload).
    pub fn from_json(bytes: &[u8]) -> Result<Self, UpdateError> {
        let m: Manifest = serde_json::from_slice(bytes)
            .map_err(|e| UpdateError::Manifest(format!("invalid manifest json: {e}")))?;
        if m.version.trim().is_empty()
            || m.cid.trim().is_empty()
            || m.engine.trim().is_empty()
            || m.sha256.len() != 64
            || !m.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || m.sig.trim().is_empty()
        {
            return Err(UpdateError::Manifest(
                "manifest missing version/cid/sha256/engine/sig or sha256 is not hex".to_string(),
            ));
        }
        Ok(m)
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Step 1 — verify the detached signature against *any* baked key. Uses
/// `verify_strict` (rejects non-canonical/small-order keys) and returns the index of
/// the key that matched, so the caller can surface which anchor was used.
pub fn verify_signature(m: &Manifest, trusted: &[VerifyingKey]) -> Result<usize, UpdateError> {
    let sig_bytes = base64_decode(&m.sig)
        .ok_or_else(|| UpdateError::BadSignature("signature is not valid base64".to_string()))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| UpdateError::BadSignature("signature is not 64 bytes".to_string()))?;
    let sig = Signature::from_bytes(&sig_arr);
    let msg = m.signing_bytes();
    for (i, key) in trusted.iter().enumerate() {
        if key.verify_strict(&msg, &sig).is_ok() {
            return Ok(i);
        }
    }
    Err(UpdateError::BadSignature(
        "no baked publisher key verifies this manifest".to_string(),
    ))
}

/// The full ladder, steps 1–4, over an already-fetched bundle. `fetched_cid` is the
/// CID the bytes were actually fetched under (IPFS content-addresses the fetch, so
/// this must equal `manifest.cid`). Returns the freshness outcome on success.
///
/// Step 5 (yara-x compile) is enforced by the caller at activation — it needs the
/// decoded ruleset and the live engine, which this pure function deliberately avoids.
pub fn verify_bundle(
    m: &Manifest,
    bundle: &[u8],
    fetched_cid: &str,
    trusted: &[VerifyingKey],
    last_epoch: u64,
    now: u64,
    freshness_secs: u64,
) -> Result<Freshness, UpdateError> {
    // 1. signature
    verify_signature(m, trusted)?;
    // 2. anti-rollback — strictly newer than what we already trust.
    if m.epoch <= last_epoch {
        return Err(UpdateError::Rollback {
            manifest_epoch: m.epoch,
            last_epoch,
        });
    }
    verify_content_and_freshness(m, bundle, fetched_cid, now, freshness_secs)
}

/// Verify an already-current manifest. Equal epochs are a no-op, but the channel
/// is still unhealthy if the publisher signature or content hash no longer verify.
pub fn verify_current_bundle(
    m: &Manifest,
    bundle: &[u8],
    fetched_cid: &str,
    trusted: &[VerifyingKey],
    now: u64,
    freshness_secs: u64,
) -> Result<Freshness, UpdateError> {
    verify_signature(m, trusted)?;
    verify_content_and_freshness(m, bundle, fetched_cid, now, freshness_secs)
}

fn verify_content_and_freshness(
    m: &Manifest,
    bundle: &[u8],
    fetched_cid: &str,
    now: u64,
    freshness_secs: u64,
) -> Result<Freshness, UpdateError> {
    // The CID guards which object we asked IPFS for; the sha256 over the raw bytes
    // is the cryptographic content equality the signature commits to. Both must hold.
    if fetched_cid != m.cid {
        return Err(UpdateError::CidMismatch {
            expected: m.cid.clone(),
            got: fetched_cid.to_string(),
        });
    }
    let actual = sha256_hex(bundle);
    if !actual.eq_ignore_ascii_case(&m.sha256) {
        return Err(UpdateError::HashMismatch {
            expected: m.sha256.clone(),
            got: actual,
        });
    }
    // 4. freshness — warn, never reject.
    let age = now.saturating_sub(m.ts);
    if age > freshness_secs {
        Ok(Freshness::Stale { age_secs: age })
    } else {
        Ok(Freshness::Fresh)
    }
}

/// Standard, padded base64 decode without pulling a new dependency surface into the
/// security path — the `base64` crate is already a core dependency, but keeping the
/// decode here documents exactly what the signature path accepts.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed(epoch: u64, bundle: &[u8], ts: u64, sk: &SigningKey) -> Manifest {
        let mut m = Manifest {
            epoch,
            version: "v-test".into(),
            cid: "bafytestcid".into(),
            sha256: sha256_hex(bundle),
            ts,
            engine: "yara-x".into(),
            sig: String::new(),
        };
        use base64::Engine;
        let sig = sk.sign(&m.signing_bytes());
        m.sig = base64::engine::general_purpose::STANDARD.encode(sig.to_bytes());
        m
    }

    fn keypair() -> (SigningKey, Vec<VerifyingKey>) {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let vk = sk.verifying_key();
        (sk, vec![vk])
    }

    #[test]
    fn good_bundle_verifies_fresh() {
        let (sk, trusted) = keypair();
        let bundle = b"rule a { condition: true }";
        let m = signed(2, bundle, 1000, &sk);
        let f = verify_bundle(&m, bundle, "bafytestcid", &trusted, 1, 1000, 100).unwrap();
        assert_eq!(f, Freshness::Fresh);
    }

    #[test]
    fn tampered_bundle_is_rejected() {
        let (sk, trusted) = keypair();
        let m = signed(2, b"original", 1000, &sk);
        let err =
            verify_bundle(&m, b"TAMPERED", "bafytestcid", &trusted, 1, 1000, 100).unwrap_err();
        assert!(matches!(err, UpdateError::HashMismatch { .. }));
    }

    #[test]
    fn rollback_is_refused() {
        let (sk, trusted) = keypair();
        let bundle = b"x";
        // Equal epoch is also refused by the pure ladder (the no-op "already current"
        // path is handled before calling verify in rules.rs).
        let m = signed(5, bundle, 1000, &sk);
        let err = verify_bundle(&m, bundle, "bafytestcid", &trusted, 5, 1000, 100).unwrap_err();
        assert!(matches!(err, UpdateError::Rollback { .. }));
        let older = signed(3, bundle, 1000, &sk);
        let err2 =
            verify_bundle(&older, bundle, "bafytestcid", &trusted, 5, 1000, 100).unwrap_err();
        assert!(matches!(err2, UpdateError::Rollback { .. }));
    }

    #[test]
    fn wrong_key_is_rejected() {
        let (sk, _) = keypair();
        let other = vec![SigningKey::from_bytes(&[9u8; 32]).verifying_key()];
        let bundle = b"x";
        let m = signed(2, bundle, 1000, &sk);
        let err = verify_bundle(&m, bundle, "bafytestcid", &other, 1, 1000, 100).unwrap_err();
        assert!(matches!(err, UpdateError::BadSignature(_)));
    }

    #[test]
    fn flipped_signature_bit_is_rejected() {
        let (sk, trusted) = keypair();
        let bundle = b"x";
        let mut m = signed(2, bundle, 1000, &sk);
        // Corrupt one signature byte; verify_strict must fail.
        let mut raw = base64_decode(&m.sig).unwrap();
        raw[0] ^= 0x01;
        use base64::Engine;
        m.sig = base64::engine::general_purpose::STANDARD.encode(raw);
        assert!(matches!(
            verify_signature(&m, &trusted),
            Err(UpdateError::BadSignature(_))
        ));
    }

    #[test]
    fn cid_mismatch_is_rejected() {
        let (sk, trusted) = keypair();
        let bundle = b"x";
        let m = signed(2, bundle, 1000, &sk);
        let err = verify_bundle(&m, bundle, "different-cid", &trusted, 1, 1000, 100).unwrap_err();
        assert!(matches!(err, UpdateError::CidMismatch { .. }));
    }

    #[test]
    fn stale_publisher_warns_not_rejects() {
        let (sk, trusted) = keypair();
        let bundle = b"x";
        let m = signed(2, bundle, 1000, &sk);
        // now is far past ts+window → Stale, but still Ok (served, with a warning).
        let f = verify_bundle(&m, bundle, "bafytestcid", &trusted, 1, 10_000, 100).unwrap();
        assert!(matches!(f, Freshness::Stale { .. }));
    }

    #[test]
    fn signing_bytes_are_stable() {
        // Lock the on-the-wire signing payload so a refactor can't silently break
        // interop with the CI signer.
        let m = Manifest {
            epoch: 42,
            version: "2026.06.17".into(),
            cid: "bafyabc".into(),
            sha256: "a".repeat(64),
            ts: 1_750_000_000,
            engine: "yara-x".into(),
            sig: "ignored".into(),
        };
        let expected = format!(
            "ucp-rules-manifest-v1\n42\n2026.06.17\nbafyabc\n{}\n1750000000\nyara-x",
            "a".repeat(64)
        );
        assert_eq!(m.signing_bytes(), expected.into_bytes());
    }

    #[test]
    fn manifest_json_rejects_non_hex_sha() {
        let bytes = br#"{
            "epoch": 2,
            "version": "v2",
            "cid": "bafy",
            "sha256": "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "ts": 1000,
            "engine": "yara-x",
            "sig": "abcd"
        }"#;
        assert!(matches!(
            Manifest::from_json(bytes),
            Err(UpdateError::Manifest(_))
        ));
    }
}
