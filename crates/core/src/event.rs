//! Layer 2 — Plugin Host API: the host-neutral event interface.
//!
//! This is the public contract. A harness in any language (Python, TS, Rust)
//! produces these events; harness adapters in Layer 3 are responsible for
//! translating their local event shapes into this one. The host side never
//! needs to understand DAG-CBOR, CAR, tombstones, or IPLD links.
//!
//! On the wire this is JSONL: one [`Envelope`] per line. See `ADAPTER_CONTRACT.md`.

use serde::{Deserialize, Serialize};

/// One event as it appears on the wire: a routing envelope plus the event body.
///
/// `host_id`, `session_id`, and the optional `project_id` keep names
/// collision-free across concurrent harnesses (see the plan's Names section).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Identifies the harness/host emitting the event.
    pub host_id: String,
    /// Stable id for the session this event belongs to.
    pub session_id: String,
    /// Optional project scope, when the host knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Optional stable event id. Phase 2 uses it as the preferred dedupe key;
    /// if absent, the ingest path falls back to a deterministic fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// RFC 3339 timestamp.
    pub ts: String,
    /// Optional backfill provenance for imported records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<ImportedFrom>,
    /// Optional model reasoning — the "why" behind this step. Rides along with
    /// the step so it lands in the *same* record/CID, not a separate node.
    /// Absent when the harness does not expose thinking (fidelity ladder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
    /// The event itself, tagged by `type`.
    #[serde(flatten)]
    pub event: Event,
}

/// An **outbound** suggestion from the node's Librarian to the host (Phase 8 §2).
///
/// Unlike every other event here (host → node capture), this flows node → host.
/// It is produced **only** under the opt-in proactive-injection gate *and* a
/// trusted-authority grant — never by default. The host treats these CIDs as
/// trusted **per the named `authority`**, attributed as such; it must never
/// silently fold them into context as instructions (threat-model L1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextSuggested {
    /// The suggested context CIDs, highest-relevance first.
    pub cids: Vec<String>,
    /// Why these were suggested (the look-ahead query / rationale).
    pub reason: String,
    /// The trusted authority under which the host may treat these as trusted.
    /// Without a grant there is no authority and no suggestion is ever produced.
    pub authority: String,
}

/// Provenance attached to backfilled events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImportedFrom {
    pub source_system: String,
    pub original_id: String,
    pub original_ts: String,
}

/// The model's reasoning ("why") behind a step, captured inline with it.
///
/// Carried on the [`Envelope`] so it lands in the same IPLD record as the step
/// it explains — one CID, never a separate "thinking" node. It travels with the
/// step on share (no strip): the reasoning is the highest-signal text for
/// retrieval, so withholding it would degrade a recipient's graph as much as the
/// holder's. Privacy is the step's own capability boundary, nothing more.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reasoning {
    /// The reasoning text.
    pub text: String,
    /// Where the reasoning came from — kept honest so a record never claims
    /// thinking the model did not actually produce.
    pub source: ReasoningSource,
}

/// Provenance of captured [`Reasoning`], so fidelity stays honest.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSource {
    /// Genuine model reasoning/thinking tokens (e.g. extended-thinking blocks).
    Thinking,
    /// Rationale the model wrote inline before acting (preamble text), not
    /// dedicated thinking tokens.
    InlinePreamble,
}

/// The host-neutral event types (Layer 2).
///
/// Serialized with an internal `type` tag, e.g.
/// `{"type":"user_prompt","text":"…"}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A new session began.
    SessionStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// A prompt from the user.
    UserPrompt { text: String },
    /// A response from the model.
    ModelResponse { text: String },
    /// A tool call has started.
    ToolCallStarted {
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args_json: Option<String>,
    },
    /// A tool call has finished.
    ToolCallFinished {
        tool: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_json: Option<String>,
    },
    /// A file was read by the host.
    FileRead { path: String },
    /// A file was written by the host.
    FileWritten { path: String },
    /// The host recorded a decision worth persisting.
    DecisionRecorded { text: String },
    /// A note or memory worth persisting — e.g. a backfilled markdown note.
    /// Maps to a `memory` node (kind `reference` for imported notes).
    MemoryRecorded { text: String },
    /// The host explicitly asked for a checkpoint.
    CheckpointRequested {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// The session ended.
    SessionEnded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_user_prompt_line() {
        let line = r#"{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:00Z","type":"user_prompt","text":"hi"}"#;
        let env = serde_json::from_str::<Envelope>(line).expect("valid line should parse");
        assert_eq!(env.host_id, "hermes");
        assert_eq!(env.session_id, "s1");
        assert!(matches!(env.event, Event::UserPrompt { text } if text == "hi"));
    }

    #[test]
    fn rejects_malformed_line() {
        assert!(serde_json::from_str::<Envelope>("{not json").is_err());
    }

    #[test]
    fn roundtrips_every_event_variant() {
        let envelopes = vec![
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: Some("proj".to_string()),
                event_id: None,
                ts: "2026-06-02T00:00:00Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::SessionStarted {
                    cwd: Some("/work/app".to_string()),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:01Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::UserPrompt {
                    text: "hi".to_string(),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:02Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::ModelResponse {
                    text: "hello".to_string(),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:03Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::ToolCallStarted {
                    tool: "write_file".to_string(),
                    args_json: Some(json!({"path": "src/lib.rs"}).to_string()),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:04Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::ToolCallFinished {
                    tool: "write_file".to_string(),
                    ok: true,
                    result_json: Some(json!({"bytes": 12}).to_string()),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:05Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::FileRead {
                    path: "README.md".to_string(),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:06Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::FileWritten {
                    path: "src/main.rs".to_string(),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:07Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::DecisionRecorded {
                    text: "use Rust core".to_string(),
                },
            },
            Envelope {
                host_id: "import".to_string(),
                session_id: "notes.md".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:07Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::MemoryRecorded {
                    text: "the river mentality".to_string(),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:08Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::CheckpointRequested {
                    label: Some("after-write".to_string()),
                },
            },
            Envelope {
                host_id: "hermes".to_string(),
                session_id: "s1".to_string(),
                project_id: None,
                event_id: None,
                ts: "2026-06-02T00:00:09Z".to_string(),
                imported_from: None,
                reasoning: None,
                event: Event::SessionEnded,
            },
        ];

        for envelope in envelopes {
            let encoded = serde_json::to_string(&envelope).expect("envelope should serialize");
            let decoded: Envelope =
                serde_json::from_str(&encoded).expect("envelope should deserialize");

            assert_eq!(decoded.host_id, envelope.host_id);
            assert_eq!(decoded.session_id, envelope.session_id);
            assert_eq!(decoded.project_id, envelope.project_id);
            assert_eq!(decoded.ts, envelope.ts);
            assert_eq!(
                serde_json::to_value(decoded.event).expect("event should serialize"),
                serde_json::to_value(envelope.event).expect("event should serialize")
            );
        }
    }

    #[test]
    fn roundtrips_optional_envelope_metadata() {
        let envelope = Envelope {
            host_id: "hermes".to_string(),
            session_id: "s1".to_string(),
            project_id: Some("proj".to_string()),
            event_id: Some("evt-1".to_string()),
            ts: "2026-06-02T00:00:00Z".to_string(),
            imported_from: Some(ImportedFrom {
                source_system: "jsonl".to_string(),
                original_id: "orig-1".to_string(),
                original_ts: "2026-06-01T23:59:59Z".to_string(),
            }),
            reasoning: None,
            event: Event::SessionEnded,
        };

        let encoded = serde_json::to_string(&envelope).expect("envelope should serialize");
        let decoded: Envelope =
            serde_json::from_str(&encoded).expect("envelope should deserialize");

        assert_eq!(decoded.event_id.as_deref(), Some("evt-1"));
        assert_eq!(decoded.imported_from, envelope.imported_from);
    }

    #[test]
    fn reasoning_rides_along_with_a_step() {
        // A tool call carries the "why" inline; it must round-trip on the same
        // envelope (same record/CID), not as a separate node.
        let envelope = Envelope {
            host_id: "claude-code".to_string(),
            session_id: "s1".to_string(),
            project_id: None,
            event_id: None,
            ts: "2026-06-09T00:00:00Z".to_string(),
            imported_from: None,
            reasoning: Some(Reasoning {
                text: "Read the schema first so the field name matches the contract.".to_string(),
                source: ReasoningSource::Thinking,
            }),
            event: Event::ToolCallStarted {
                tool: "read_file".to_string(),
                args_json: Some(json!({"path": "event.rs"}).to_string()),
            },
        };

        let encoded = serde_json::to_string(&envelope).expect("envelope should serialize");
        let decoded: Envelope =
            serde_json::from_str(&encoded).expect("envelope should deserialize");

        assert_eq!(decoded.reasoning, envelope.reasoning);
        assert!(matches!(
            decoded.reasoning.unwrap().source,
            ReasoningSource::Thinking
        ));
    }

    #[test]
    fn reasoning_absent_is_omitted_from_the_wire() {
        // Backward-compatibility: a line with no reasoning parses, and a record
        // without reasoning never serializes the field.
        let line = r#"{"host_id":"hermes","session_id":"s1","ts":"2026-06-02T00:00:00Z","type":"user_prompt","text":"hi"}"#;
        let env = serde_json::from_str::<Envelope>(line).expect("line without reasoning parses");
        assert!(env.reasoning.is_none());

        let encoded = serde_json::to_string(&env).expect("serialize");
        assert!(!encoded.contains("reasoning"));
    }
}
