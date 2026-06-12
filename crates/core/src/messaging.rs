//! Phase 5.7 — content-addressed messaging (the conversation plane).
//!
//! A message is a **signed IPLD node linking its parent's CID** — the orbitdb
//! `Entry` shape (Decision 0008/0010) adapted to `mem`'s schema: it is stored as
//! the `text` of a `memory` node, so a thread is a Merkle-DAG of CIDs and the
//! "messenger" is just a *view* over these nodes. The CID is the message id; the
//! signature proves *who*; the parent CID links the thread.
//!
//! This module is the **local** substance: the message model, signing/verifying,
//! thread assembly, the `(Lamport time, then AgentID)` total order, and the
//! **AI-send lever** (the room participation policy). The network transport that
//! moves these signed nodes between installs — libp2p gossipsub rooms + direct
//! streams + store-and-forward — is the deferred half of Phase 5.7 (the
//! "embed rust-libp2p" decision); it carries *these same* nodes.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A message envelope (orbitdb `Entry` shape): signed, parent-linked, clocked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEnvelope {
    /// The room / thread id this message belongs to.
    #[serde(alias = "room")]
    pub id: String,
    /// The message body.
    pub payload: String,
    /// Parent message CIDs (the previous messages in the thread).
    /// `parent` is accepted for backward compatibility with the earlier linear
    /// shape; it deserializes to a single-entry `next`.
    #[serde(default, alias = "parent", deserialize_with = "deserialize_next")]
    pub next: Vec<String>,
    /// Skip-links for faster traversal / later room views.
    #[serde(default)]
    pub refs: Vec<String>,
    /// Lamport clock — `parent.clock + 1` (Decision 0010).
    #[serde(default)]
    pub clock: u64,
    /// Author AgentID (the public key).
    #[serde(alias = "author")]
    pub key: String,
    /// Hex Ed25519 signature over the canonical, signature-less content.
    pub sig: String,
}

impl MessageEnvelope {
    /// The deterministic bytes that get signed — every field *except* the
    /// signature, in fixed order. Recomputed on verify, so a tampered payload,
    /// parent set, clock, or author key breaks the signature.
    pub fn signing_bytes(&self) -> Vec<u8> {
        serde_json::json!({
            "id": self.id,
            "payload": self.payload,
            "next": self.next,
            "refs": self.refs,
            "clock": self.clock,
            "key": self.key,
        })
        .to_string()
        .into_bytes()
    }

    /// Backward-compatible accessor for the room id.
    pub fn room(&self) -> &str {
        &self.id
    }

    /// Backward-compatible accessor for the author AgentID.
    pub fn author(&self) -> &str {
        &self.key
    }

    /// Backward-compatible accessor for the single-chain parent, if present.
    pub fn parent(&self) -> Option<&str> {
        self.next.first().map(String::as_str)
    }
}

/// Total order for concurrent messages: **Lamport time, then AgentID** (orbitdb's
/// rule, Decision 0010). Deterministic across every client, never a tie unless
/// it's the same message.
pub fn message_order(a: &MessageEnvelope, b: &MessageEnvelope) -> Ordering {
    a.clock.cmp(&b.clock).then_with(|| a.key.cmp(&b.key))
}

/// Per-room participation policy — the **AI-send lever** plus the mute list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomPolicy {
    /// `on` (open brainstorm), `off` (Human-only), or `on_mention`.
    pub ai_send: String,
    /// Muted AgentIDs — a *receiver-side* filter. Muted messages still land in
    /// the DAG and are still traversed (**mute ≠ deafen**); they're just hidden.
    #[serde(default)]
    pub muted: BTreeSet<String>,
}

impl Default for RoomPolicy {
    fn default() -> Self {
        Self {
            ai_send: "on".to_string(),
            muted: BTreeSet::new(),
        }
    }
}

impl RoomPolicy {
    /// May an author of the given kind (`"human"` / `"ai"`) post to this room?
    /// The send-side half of the lever: a Human-only room refuses AI sends, and
    /// `on_mention` requires an explicit `@` mention in the payload.
    pub fn may_send(&self, author_kind: &str, payload: &str) -> bool {
        match self.ai_send.as_str() {
            "off" => author_kind != "ai",
            "on_mention" => author_kind != "ai" || payload.contains('@'),
            _ => true,
        }
    }
}

fn deserialize_next<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NextCompat {
        Single(String),
        Many(Vec<String>),
        Empty,
    }

    match Option::<NextCompat>::deserialize(deserializer)? {
        None | Some(NextCompat::Empty) => Ok(Vec::new()),
        Some(NextCompat::Single(parent)) => Ok(vec![parent]),
        Some(NextCompat::Many(next)) => Ok(next),
    }
}

/// All room policies, persisted beside the store (`.concierge/rooms.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoomBook {
    #[serde(default)]
    pub rooms: BTreeMap<String, RoomPolicy>,
}

impl RoomBook {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("read room book: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("parse room book: {e}"))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        crate::state::save_json(path, self).map_err(|e| e.to_string())
    }

    pub fn policy(&self, room: &str) -> RoomPolicy {
        self.rooms.get(room).cloned().unwrap_or_default()
    }

    pub fn set_ai_send(&mut self, room: &str, value: &str) {
        self.rooms.entry(room.to_string()).or_default().ai_send = value.to_string();
    }

    pub fn mute(&mut self, room: &str, agent_id: &str) {
        self.rooms
            .entry(room.to_string())
            .or_default()
            .muted
            .insert(agent_id.to_string());
    }

    pub fn is_muted(&self, room: &str, agent_id: &str) -> bool {
        self.rooms
            .get(room)
            .map(|p| p.muted.contains(agent_id))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(clock: u64, author: &str, payload: &str) -> MessageEnvelope {
        MessageEnvelope {
            id: "r".to_string(),
            payload: payload.to_string(),
            next: Vec::new(),
            refs: Vec::new(),
            clock,
            key: author.to_string(),
            sig: String::new(),
        }
    }

    #[test]
    fn total_order_is_lamport_then_agent_then_never_ties() {
        let mut msgs = [
            env(2, "bbb", "later"),
            env(1, "zzz", "first by clock"),
            env(2, "aaa", "tie on clock, wins on agent"),
        ];
        msgs.sort_by(message_order);
        assert_eq!(msgs[0].payload, "first by clock");
        assert_eq!(msgs[1].key, "aaa", "lower AgentID breaks a clock tie");
        assert_eq!(msgs[2].key, "bbb");
    }

    #[test]
    fn signing_bytes_exclude_the_signature() {
        let mut e = env(1, "x", "hi");
        let before = e.signing_bytes();
        e.sig = "deadbeef".to_string();
        assert_eq!(
            before,
            e.signing_bytes(),
            "the signature is not part of what's signed"
        );
        // ...but changing the payload does change the signed bytes.
        e.payload = "tampered".to_string();
        assert_ne!(before, e.signing_bytes());
    }

    #[test]
    fn legacy_room_parent_author_shape_still_parses() {
        let json = r#"{
            "room":"r",
            "payload":"hi",
            "parent":"cid-parent",
            "clock":7,
            "author":"agent-xyz",
            "sig":"abc123"
        }"#;
        let env: MessageEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.id, "r");
        assert_eq!(env.next, vec!["cid-parent".to_string()]);
        assert_eq!(env.key, "agent-xyz");
        assert_eq!(env.parent(), Some("cid-parent"));
    }

    #[test]
    fn humans_only_room_refuses_ai_sends() {
        let policy = RoomPolicy {
            ai_send: "off".to_string(),
            muted: BTreeSet::new(),
        };
        assert!(policy.may_send("human", "hello"));
        assert!(
            !policy.may_send("ai", "hello"),
            "a Human-only room mutes AI sends"
        );
    }

    #[test]
    fn mention_only_room_requires_an_at_symbol_for_ai() {
        let policy = RoomPolicy {
            ai_send: "on_mention".to_string(),
            muted: BTreeSet::new(),
        };
        assert!(policy.may_send("human", "hello"));
        assert!(!policy.may_send("ai", "hello"));
        assert!(policy.may_send("ai", "ping @room"));
    }

    #[test]
    fn room_book_persists_policy_and_mute() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rooms.json");
        let mut book = RoomBook::load(&path).unwrap();
        book.set_ai_send("conservation", "off");
        book.mute("conservation", "agent-spammer");
        book.save(&path).unwrap();

        let reloaded = RoomBook::load(&path).unwrap();
        assert_eq!(reloaded.policy("conservation").ai_send, "off");
        assert!(reloaded.is_muted("conservation", "agent-spammer"));
        assert!(!reloaded.is_muted("conservation", "someone-else"));
    }
}
