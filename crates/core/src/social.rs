//! Phase 5.5 — the local social book: petnames and a follow list.
//!
//! Global identity is the AgentID (a public key); *locally* you give other
//! AgentIDs human-friendly **petnames** and choose who to **follow** — no central
//! name registry required (Decision 0007). Stored as plain JSON beside the store
//! (`.concierge/social.json`), outside the DAG.
//!
//! The follow list is also the **authorization seam**: it's the natural allowlist
//! for whose shares/messages you accept once the messaging plane (Phase 5.7)
//! lands.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Local, per-install social state: nicknames for AgentIDs and who you follow.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SocialBook {
    /// AgentID → human-friendly petname.
    #[serde(default)]
    pub nicknames: BTreeMap<String, String>,
    /// AgentIDs you follow (the allowlist for inbound shares/messages).
    #[serde(default)]
    pub following: BTreeSet<String>,
}

impl SocialBook {
    /// Load the book, or an empty one if it doesn't exist yet.
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("read social book: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("parse social book: {e}"))
    }

    /// Persist the book (creating the parent dir if needed).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("social book dir: {e}"))?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize social: {e}"))?;
        std::fs::write(path, text).map_err(|e| format!("write social book: {e}"))
    }

    pub fn follow(&mut self, agent_id: &str) {
        self.following.insert(agent_id.to_string());
    }

    pub fn unfollow(&mut self, agent_id: &str) {
        self.following.remove(agent_id);
    }

    pub fn set_nickname(&mut self, agent_id: &str, nickname: &str) {
        self.nicknames
            .insert(agent_id.to_string(), nickname.to_string());
    }

    pub fn nickname_of(&self, agent_id: &str) -> Option<&String> {
        self.nicknames.get(agent_id)
    }

    pub fn is_following(&self, agent_id: &str) -> bool {
        self.following.contains(agent_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_and_nickname_persist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("social.json");

        let mut book = SocialBook::load(&path).expect("empty load");
        book.follow("agent-abc");
        book.set_nickname("agent-abc", "Gabriel");
        book.save(&path).expect("save");

        let reloaded = SocialBook::load(&path).expect("reload");
        assert!(reloaded.is_following("agent-abc"));
        assert_eq!(
            reloaded.nickname_of("agent-abc"),
            Some(&"Gabriel".to_string())
        );
        assert!(!reloaded.is_following("someone-else"));
    }
}
