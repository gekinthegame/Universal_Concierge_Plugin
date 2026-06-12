//! Sender-side store-and-forward outbox for direct messages. When a DM can't be
//! delivered immediately (the recipient is offline / unreachable), it is queued
//! here and re-sent on a schedule until the recipient acknowledges receipt —
//! delivery succeeds whenever both peers are online together within the TTL.
//! Without a relay/mailbox server this is the realistic offline path for a
//! public-DHT, zero-infrastructure network. Each entry is keyed by the transport
//! content id of the sent bytes, so the delivery ack clears exactly the message
//! that landed (receiver-side de-dupe by signature makes re-sends harmless).

use std::collections::BTreeMap;
use std::path::Path;

/// How long an undelivered message keeps being retried before it is dropped
/// (gives up on a recipient who never comes online within the window).
pub const OUTBOX_TTL_SECS: i64 = 60 * 60 * 24;

/// The queued direct messages awaiting delivery, keyed by transport content id.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct DmOutbox {
    #[serde(default)]
    pub pending: BTreeMap<String, OutboundDm>,
}

/// One undelivered direct message.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutboundDm {
    /// Recipient username (hex AgentID).
    pub recipient: String,
    /// The signed message-envelope JSON to (re)send verbatim.
    pub envelope: String,
    /// Unix seconds when first queued, for TTL expiry.
    pub queued_at: i64,
}

impl DmOutbox {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("parse dm outbox: {e}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("read dm outbox: {error}")),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("dm outbox dir: {e}"))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("write dm outbox: {e}"))
    }

    /// Drop entries older than the TTL.
    pub fn prune(&mut self, now: i64) {
        self.pending
            .retain(|_, dm| now - dm.queued_at < OUTBOX_TTL_SECS);
    }
}
