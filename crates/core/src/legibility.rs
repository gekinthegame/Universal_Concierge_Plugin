//! Phase N · Phase I — Social Legibility (Decision 0024).
//!
//! The final, post-core layer: it makes the user's **own** trust and **own** graph
//! legible to them. It is **strictly local and personal**. There is permanently
//! **no global reputation, karma, vote, popularity, proof-of-contribution, or token
//! economy** here — that is excluded by Decisions 0012 + 0022 + 0024, not deferred.
//! Reopening it would be a separate decision against those, not a feature.
//!
//! Three honest signals, all computed from material the user already holds:
//! 1. **Trust tier** ([`TrustTier`]) — the authentication discipline a message
//!    actually crossed. **Never claims a tier whose crypto is not shipped.**
//! 2. **Structural importance** ([`structural_importance`]) — how many things a
//!    message ties together (the Decision 0022 graph-gravity intuition), framed as
//!    *importance*, never "hottest" or "reputation".
//! 3. **Personal social-gravity** ([`social_gravity_factor`]) — a relevance lens
//!    computed over the user's **own** follow graph; it brightens nodes from people
//!    *you* follow. It is never a global score that ranks people.

use std::collections::BTreeSet;

use crate::identity::{verify as verify_sig, AgentId};
use crate::messaging::MessageEnvelope;

/// The authentication tier a message crossed (the Decision 0024 tiered regime).
///
/// **Tier B (Enclave-bound symmetric MAC) is deliberately absent** — it is not
/// shipped, so the thermometer must never display it. We classify only what we can
/// honestly prove today: Tier A (local capability/possession) and Tier C (Ed25519
/// signature). Post-quantum Tier C is likewise future work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    /// You authored it on this device — trusted by capability/possession (Tier A).
    Local,
    /// From another identity and **Ed25519-verified** — it crossed a host boundary
    /// and authenticated (Tier C).
    GlobalSigned,
    /// From another identity but the signature is missing, malformed, or does not
    /// verify. Shown honestly as *not* authenticated — never silently trusted.
    Unverified,
}

impl TrustTier {
    /// The human label for the badge.
    pub fn label(self) -> &'static str {
        match self {
            TrustTier::Local => "Local",
            TrustTier::GlobalSigned => "Global Signed",
            TrustTier::Unverified => "Unverified",
        }
    }
}

/// Classify a message's trust tier from `this_agent`'s perspective. Your own
/// messages are `Local`; another identity's signature-verified messages are
/// `GlobalSigned`; anything that fails to verify is `Unverified`. The result always
/// matches the authentication actually used — and is never `Device`, because that
/// tier's crypto is not shipped.
pub fn message_trust_tier(envelope: &MessageEnvelope, this_agent: &str) -> TrustTier {
    if envelope.author() == this_agent {
        return TrustTier::Local;
    }
    let verified = verify_sig(
        &AgentId(envelope.author().to_string()),
        &envelope.signing_bytes(),
        &envelope.sig,
    )
    .unwrap_or(false);
    if verified {
        TrustTier::GlobalSigned
    } else {
        TrustTier::Unverified
    }
}

/// How many things a message **ties together** — the count of CIDs it references
/// (decisions, files, prior context). This is the structural-importance signal
/// (Decision 0022 gravity), framed as load-bearing-ness. It is a pure function of
/// the message's own links — never a popularity, vote, or engagement count.
pub fn structural_importance(envelope: &MessageEnvelope) -> usize {
    envelope.refs.len()
}

/// A personal relevance multiplier for a node authored by `author`, given the
/// user's **own** `follows`. `> 1.0` brightens/clusters nodes from people the user
/// follows toward the center of their lens. It reads only the user's local follow
/// graph (Decision 0007) and **emits/stores no global score about anyone** — it is
/// a lens, not a ranking of people.
pub fn social_gravity_factor(author: &str, follows: &BTreeSet<String>) -> f32 {
    if follows.contains(author) {
        1.5
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn signed_message(author: &Identity, payload: &str, refs: Vec<String>) -> MessageEnvelope {
        let mut env = MessageEnvelope {
            id: "room".to_string(),
            payload: payload.to_string(),
            next: vec![],
            refs,
            clock: 1,
            key: author.agent_id().0,
            sig: String::new(),
        };
        env.sig = author.sign(&env.signing_bytes());
        env
    }

    #[test]
    fn your_own_message_is_local_and_anothers_verified_message_is_global_signed() {
        let me = Identity::generate();
        let peer = Identity::generate();
        let mine = signed_message(&me, "hi", vec![]);
        let theirs = signed_message(&peer, "hello", vec![]);

        assert_eq!(
            message_trust_tier(&mine, &me.agent_id().0),
            TrustTier::Local
        );
        assert_eq!(
            message_trust_tier(&theirs, &me.agent_id().0),
            TrustTier::GlobalSigned
        );
    }

    #[test]
    fn a_tampered_message_is_unverified_not_silently_trusted() {
        let me = Identity::generate();
        let peer = Identity::generate();
        let mut theirs = signed_message(&peer, "original", vec![]);
        theirs.payload = "tampered".to_string(); // mutate after signing
        assert_eq!(
            message_trust_tier(&theirs, &me.agent_id().0),
            TrustTier::Unverified
        );
    }

    #[test]
    fn the_thermometer_never_claims_an_unshipped_tier() {
        // Honesty invariant: there is no `Device` (Tier B) tier, so the classifier
        // can only ever return tiers whose crypto actually ships.
        let me = Identity::generate();
        let peer = Identity::generate();
        for env in [
            signed_message(&me, "a", vec![]),
            signed_message(&peer, "b", vec![]),
        ] {
            let tier = message_trust_tier(&env, &me.agent_id().0);
            assert!(matches!(
                tier,
                TrustTier::Local | TrustTier::GlobalSigned | TrustTier::Unverified
            ));
        }
    }

    #[test]
    fn structural_importance_counts_what_a_message_ties_together() {
        let me = Identity::generate();
        let orphan = signed_message(&me, "aside", vec![]);
        let hub = signed_message(
            &me,
            "decision recap",
            vec!["bafyDecision".into(), "bafyFile".into()],
        );
        assert_eq!(structural_importance(&orphan), 0);
        assert_eq!(structural_importance(&hub), 2, "two things tied together");
        // Importance is structural (links), not engagement — re-reading changes nothing.
        assert_eq!(structural_importance(&hub), 2);
    }

    #[test]
    fn the_social_lens_brightens_followed_authors_and_is_purely_local() {
        let mut follows = BTreeSet::new();
        follows.insert("agent-friend".to_string());
        assert!(
            social_gravity_factor("agent-friend", &follows) > 1.0,
            "a followed author brightens"
        );
        assert_eq!(
            social_gravity_factor("agent-stranger", &follows),
            1.0,
            "others are neutral, not penalized"
        );
        // No global score is produced — the factor is a pure function of the user's
        // own follow set, computed on demand, never stored about anyone.
    }
}
