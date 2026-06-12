//! Contact consent for direct messages (the "only an approved concierge can
//! reach me" gate). On the public network anyone can *attempt* delivery, so an
//! inbound message is only let into a thread when its **author** is an approved
//! contact. A message from an unknown author is held as a **request** the user
//! explicitly accepts or declines — having the recipient's (public) username is
//! never enough to land a message. See `concierge-only-message-acceptance`.
//!
//! This is the application-layer allowlist; signature verification (authorship)
//! and the `/concierge/` protocol peer-gate are separate, complementary layers.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Persisted consent state: approved sender usernames, plus per-sender queues of
/// held inbound message envelopes (raw signed JSON) awaiting an accept/decline.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Contacts {
    /// Usernames (hex AgentIDs) whose messages are accepted into threads.
    #[serde(default)]
    pub approved: BTreeSet<String>,
    /// Held messages from not-yet-approved senders: username → queued envelope JSON.
    #[serde(default)]
    pub requests: BTreeMap<String, Vec<String>>,
}

impl Contacts {
    /// Load from `path`, or a fresh empty set if the file does not exist yet.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("parse contacts: {e}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("read contacts: {error}")),
        }
    }

    /// Persist with locked atomic replacement.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        crate::state::save_json(path, self).map_err(|e| e.to_string())
    }
}
