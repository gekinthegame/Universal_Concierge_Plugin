//! Phase 8 §2 — the Context Compiler (proactive injection), the opt-in
//! "librarian-as-agent" path. **Off by default** (Decision 0022): recall is a
//! tool the host calls; the compiler only *pushes* context when explicitly
//! invited and trusted.
//!
//! Two gates, both required before a single [`ContextSuggested`] is produced:
//! 1. **Config opt-in** — `injection.proactive` must be `true` (default `false`).
//! 2. **Trusted-authority grant** — the host must present a [`TrustedAuthority`]
//!    at request time (threat-model L1 / the MemoryOS "Ground Truth" lesson:
//!    injected memory the agent is not told to trust gets ignored or drifts).
//!
//! With either gate closed the compiler returns `None` and the node stays
//! tool-only. This module is the *decision* logic; wiring it onto a live wake
//! trigger and shipping the suggestion back to the harness is the adapter's
//! write-back seam (deferred — the loop closes there).

use crate::config::InjectionConfig;
use crate::event::ContextSuggested;
use crate::retrieval::{Depth, Embedder, Librarian};

/// A harness-specific trust grant the host presents to authorize proactive
/// injection. Its `id` names the authority the host will attribute suggestions
/// to. Absent a grant, no suggestion is ever emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedAuthority {
    pub id: String,
}

impl TrustedAuthority {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

/// The proactive context compiler (Phase 8 §2). Stateless: every decision is a
/// pure function of the config, the presented grant, the index, and the query.
pub struct ContextCompiler;

impl ContextCompiler {
    /// Whether a captured `event_type` should wake a look-ahead, per config.
    /// `false` whenever proactive injection is off — so the default node never
    /// wakes at all.
    pub fn should_wake(config: &InjectionConfig, event_type: &str) -> bool {
        config.proactive && config.wake_on.iter().any(|w| w == event_type)
    }

    /// Produce a context suggestion — or `None`. Returns `None` unless **both**
    /// gates pass (proactive on *and* a trusted-authority grant present) and a
    /// look-ahead retrieval clears the confidence threshold. The default config
    /// (`proactive = false`) therefore emits nothing, ever.
    pub fn suggest<E: Embedder>(
        config: &InjectionConfig,
        authority: Option<&TrustedAuthority>,
        librarian: &Librarian<E>,
        query: &str,
    ) -> Option<ContextSuggested> {
        // Gate 1: opt-in (default off).
        if !config.proactive {
            return None;
        }
        // Gate 2: a trusted-authority grant is mandatory.
        let authority = authority?;

        let result = librarian.retrieve(query, config.budget_tokens, &[], Depth::Brief);
        // Confidence: the best hit must clear the threshold to be worth pushing.
        let top_score = result.items.first().map(|hit| hit.score).unwrap_or(0.0);
        if top_score < config.confidence {
            return None;
        }
        let cids: Vec<String> = result
            .items
            .iter()
            .take(config.max_suggestions)
            .map(|hit| hit.cid.clone())
            .collect();
        if cids.is_empty() {
            return None;
        }
        Some(ContextSuggested {
            cids,
            reason: format!("relevant prior context for: {query}"),
            authority: authority.id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::LexicalEmbedder;

    // A tiny in-memory index over a couple of texts, via the test constructor.
    fn library() -> Librarian<LexicalEmbedder> {
        // Reuse the retrieval crate's store-free path through a real MemCli would
        // be heavier; instead index a small temp store.
        let dir = tempfile::tempdir().unwrap();
        let mem = crate::binding::MemCli::new(dir.path());
        use crate::binding::{CoreBinding, Node};
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"the egress lock fences data from leaving the device","kind":"reference"}"#.to_string(),
            })
            .unwrap();
        mem.bind("latest", &cid).unwrap();
        // Keep the temp dir alive for the store's lifetime via leak (test-only).
        std::mem::forget(dir);
        Librarian::index_all(&mem, LexicalEmbedder::default()).unwrap()
    }

    fn on() -> InjectionConfig {
        InjectionConfig {
            proactive: true,
            confidence: 0.0,
            ..Default::default()
        }
    }

    #[test]
    fn proactive_injection_is_off_by_default_and_emits_nothing() {
        let lib = library();
        let config = InjectionConfig::default();
        assert!(!config.proactive, "default is tool-only");
        // Even with a grant and a strong match, the default config emits nothing.
        let grant = TrustedAuthority::new("user");
        assert!(
            ContextCompiler::suggest(&config, Some(&grant), &lib, "egress lock device").is_none()
        );
        assert!(
            !ContextCompiler::should_wake(&config, "user_prompt"),
            "default never wakes"
        );
    }

    #[test]
    fn injection_requires_a_trusted_authority_grant() {
        let lib = library();
        let config = on();
        // Proactive on but NO grant → refused.
        assert!(
            ContextCompiler::suggest(&config, None, &lib, "egress lock device").is_none(),
            "no trusted-authority grant → no suggestion"
        );
        // Proactive on WITH a grant → a suggestion, attributed to that authority.
        let grant = TrustedAuthority::new("claude-code");
        let suggestion =
            ContextCompiler::suggest(&config, Some(&grant), &lib, "egress lock device")
                .expect("opt-in + grant produces a suggestion");
        assert!(!suggestion.cids.is_empty());
        assert_eq!(
            suggestion.authority, "claude-code",
            "attributed to the granting authority"
        );
    }

    #[test]
    fn a_suggestion_respects_the_confidence_threshold() {
        let lib = library();
        let grant = TrustedAuthority::new("user");
        // An impossibly high threshold suppresses even a real match.
        let strict = InjectionConfig {
            proactive: true,
            confidence: 100.0,
            ..Default::default()
        };
        assert!(
            ContextCompiler::suggest(&strict, Some(&grant), &lib, "egress lock device").is_none()
        );
    }

    #[test]
    fn wake_policy_fires_only_on_configured_triggers() {
        let config = on();
        assert!(ContextCompiler::should_wake(&config, "user_prompt"));
        assert!(
            !ContextCompiler::should_wake(&config, "tool_call_started"),
            "not a default wake trigger"
        );
    }
}
