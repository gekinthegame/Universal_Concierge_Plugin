//! The published-site registry (the Planet Pattern). Each site has a stable IPNS
//! address (`k51…`) generated once; publishing re-`add`s the folder as UnixFS and
//! re-points the IPNS name at the new CID, so the public URL never changes while
//! the content updates. This is runtime state (the last CID changes every publish),
//! so it lives in its own `sites.json` rather than the user's `config.toml`.

use std::collections::BTreeMap;
use std::path::Path;

/// The persisted registry: site name → its record.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Sites {
    #[serde(default)]
    pub sites: BTreeMap<String, SiteRecord>,
}

/// One published (or stage-registered) site.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SiteRecord {
    /// Human name, also the IPNS key name in the public Kubo keystore.
    pub name: String,
    /// The site's stable IPNS address (`k51…`); the public URL is `/ipns/<ipns>`.
    pub ipns: String,
    /// The local source folder last published for this site.
    pub dir: String,
    /// The UnixFS directory CID of the most recent publish (None until first publish).
    #[serde(default)]
    pub last_cid: Option<String>,
    /// Unix seconds of the last successful publish (0 if never).
    #[serde(default)]
    pub published_at: i64,
}

impl Sites {
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| format!("parse sites: {e}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("read sites: {error}")),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        crate::state::save_json(path, self).map_err(|e| e.to_string())
    }
}
