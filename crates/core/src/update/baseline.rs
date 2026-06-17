//! The baked floor (Decision D-AU-4) and the baked trust anchors (Decision D-AU-3).
//!
//! Everything in this file is compiled *into the binary* so the autoupdater's trust
//! does not depend on IPNS, DNS, or GitHub being honest:
//!
//! - [`BAKED_RULES`] — a last-known-good YARA ruleset, so the scanner works on a
//!   fresh, node-less, offline install. IPNS only ever supersedes it.
//! - [`PUBLISHER_PUBKEYS_HEX`] — the Ed25519 public key(s) the rules-manifest
//!   signature is verified against. A stolen IPNS key cannot ship poison because
//!   the bundle must also carry a signature from one of these keys.
//! - [`APP_PUBKEYS_HEX`] — the key(s) the *binary* release signature is verified
//!   against (same offline-key discipline; SHASUMS alone proves nothing against a
//!   host compromise).
//! - [`DEFAULT_RULES_IPNS`] — the publisher's "latest" pointer, used when the user
//!   has not pinned their own.

use ed25519_dalek::VerifyingKey;

use super::UpdateError;

/// The last-known-good ruleset bundled at build time. UnixFS/CAR packing is what
/// rides over IPNS; the *baked* copy is just the raw rule text — the smallest thing
/// that makes the scanner work offline from the first byte.
pub const BAKED_RULES: &[u8] = include_bytes!("baseline/rules.yar");

/// The epoch the baked baseline represents. The first IPNS bundle must carry an
/// epoch strictly greater than this to supersede it (anti-rollback, Decision D-AU-3).
pub const BAKED_EPOCH: u64 = 1;

/// Human-readable version of the baked baseline.
pub const BAKED_VERSION: &str = "baseline-2026.06";

/// The `yara-x` engine version the baked baseline was validated against. Recorded so
/// a node can tolerate engine skew gracefully rather than rejecting a whole bundle
/// (open risk in the plan §9).
pub const BAKED_ENGINE: &str = "yara-x";

/// Placeholder Ed25519 public key(s) used only for local/dev builds that did not
/// bake release-time keys. Their private halves were intentionally discarded and
/// are not useful for shipping updates. Real release builds should inject
/// `UCP_RULES_PUBKEYS_HEX` and `UCP_APP_PUBKEYS_HEX`.
pub const PUBLISHER_PUBKEYS_HEX: &[&str] =
    &["0901e6ac503835e62f9a83f02eabb79ea9941c0d11dd6e3430a85345b8508308"];

pub const APP_PUBKEYS_HEX: &[&str] =
    &["f3ca8179a00248c6af8e12278afbdbdba3f4d6e75191d6d84e0226ad5b1923a0"];

/// Optional release-time baked rules IPNS name (`k51…`). Local/dev builds can leave
/// this unset and configure `config.update.rules_ipns` instead.
pub const DEFAULT_RULES_IPNS: &str = match option_env!("UCP_RULES_IPNS") {
    Some(value) => value,
    None => "",
};

/// The GitHub `owner/repo` the app channel polls for releases.
pub const APP_REPO: &str = "gekinthegame/Universal_Concierge_Plugin";

fn parse_keys(hexes: impl IntoIterator<Item = &'static str>) -> Vec<VerifyingKey> {
    hexes
        .into_iter()
        .filter_map(|h| {
            let bytes = hex_to_array32(h)?;
            VerifyingKey::from_bytes(&bytes).ok()
        })
        .collect()
}

fn configured_hexes(
    env: Option<&'static str>,
    fallback: &'static [&'static str],
) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = env
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .collect();
    if out.is_empty() {
        out.extend_from_slice(fallback);
    }
    out
}

/// The trusted rules-publisher verifying keys, parsed from [`PUBLISHER_PUBKEYS_HEX`].
pub fn publisher_pubkeys() -> Vec<VerifyingKey> {
    parse_keys(configured_hexes(
        option_env!("UCP_RULES_PUBKEYS_HEX"),
        PUBLISHER_PUBKEYS_HEX,
    ))
}

/// The trusted app-release verifying keys, parsed from [`APP_PUBKEYS_HEX`].
pub fn app_pubkeys() -> Vec<VerifyingKey> {
    parse_keys(configured_hexes(
        option_env!("UCP_APP_PUBKEYS_HEX"),
        APP_PUBKEYS_HEX,
    ))
}

pub fn default_rules_ipns() -> Option<&'static str> {
    let trimmed = DEFAULT_RULES_IPNS.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Parse a hex-encoded 32-byte Ed25519 key into a [`VerifyingKey`]. Surfaces a typed
/// error so a `concierge rules pin <key>` with a malformed key fails clearly.
pub fn parse_pubkey_hex(h: &str) -> Result<VerifyingKey, UpdateError> {
    let bytes = hex_to_array32(h.trim())
        .ok_or_else(|| UpdateError::BadKey(format!("not 32 hex-encoded bytes: {h}")))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|e| UpdateError::BadKey(format!("invalid ed25519 public key: {e}")))
}

fn hex_to_array32(h: &str) -> Option<[u8; 32]> {
    let h = h.trim();
    if h.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(h.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_pubkeys_parse() {
        // A baked key that does not parse would silently disable verification, so
        // assert the build always ships at least one usable trust anchor.
        assert!(
            !publisher_pubkeys().is_empty(),
            "no usable publisher pubkey"
        );
        assert!(!app_pubkeys().is_empty(), "no usable app pubkey");
    }

    #[test]
    fn baked_rules_are_nonempty() {
        assert!(BAKED_RULES.len() > 100);
        assert!(BAKED_RULES.windows(4).any(|w| w == b"rule"));
    }

    #[test]
    fn pin_rejects_malformed_keys() {
        assert!(parse_pubkey_hex("nope").is_err());
        assert!(parse_pubkey_hex("ab").is_err());
        assert!(parse_pubkey_hex(PUBLISHER_PUBKEYS_HEX[0]).is_ok());
    }
}
