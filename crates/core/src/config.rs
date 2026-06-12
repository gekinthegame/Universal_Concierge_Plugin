//! Local configuration for the core binding and CLI bootstrap.
//!
//! Phase 1 only needs the store/host/checkpoint surfaces. The rest of the
//! project-level config lives in `mem` itself; this crate keeps the minimum
//! shape needed to load the store root and checkpoint policy without hand-parsing
//! TOML at the call sites.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The config file the plugin reads and writes under the project root.
pub const CONFIG_PATH: &str = ".concierge/config.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub store: StoreConfig,
    pub host: HostConfig,
    pub checkpoint: CheckpointConfig,
    pub publishing: PublishingConfig,
    pub identity: IdentityConfig,
    pub injection: InjectionConfig,
    pub librarian: LibrarianConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct StoreConfig {
    pub root: PathBuf,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from(".concierge"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HostConfig {
    pub id: String,
    pub adapter: String,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            adapter: "jsonl".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CheckpointConfig {
    pub auto: bool,
    pub every_turns: u32,
    pub on_exit: bool,
    pub keep_checkpoints: u32,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            auto: true,
            every_turns: 1,
            on_exit: true,
            keep_checkpoints: 10,
        }
    }
}

/// Phase 8 §1 — Librarian embedder selection. The embedding model is **not baked
/// in**: it is chosen here so it can be swapped as models age, with no recompile.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LibrarianConfig {
    /// Which embedder backend: `"auto"` (semantic model if the build supports it
    /// and it loads, else lexical), `"lexical"` (zero-dep fallback), `"fastembed"`
    /// (in-process ONNX, `embedding_model`), or `"http"` (any model server at
    /// `embedding_url` — Ollama-style; works without the `semantic-embed` feature).
    pub embedder: String,
    /// The model to load for the `fastembed` backend — matched by name against
    /// fastembed's supported models (e.g. `bge-small-en-v1.5`, `nomic-embed-text-v1.5`,
    /// `mxbai-embed-large-v1`). Swap this to adopt a newer model.
    pub embedding_model: String,
    /// The embeddings endpoint for the `http` backend (e.g.
    /// `http://127.0.0.1:11434/api/embeddings`). Empty disables it.
    pub embedding_url: String,
}

impl Default for LibrarianConfig {
    fn default() -> Self {
        Self {
            embedder: "auto".to_string(),
            embedding_model: "bge-small-en-v1.5".to_string(),
            embedding_url: String::new(),
        }
    }
}

/// Phase 8 §2 — proactive context injection (the "librarian-as-agent" path).
/// **Off by default** (Decision 0022): recall is a tool the host calls, never a
/// push. Turning `proactive` on is necessary but *not sufficient* — emission also
/// requires a trusted-authority grant at request time (threat-model L1: injected
/// memory the agent is not told to trust gets ignored or causes drift).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct InjectionConfig {
    /// Master switch for proactive injection. Default `false` (tool-only recall).
    pub proactive: bool,
    /// Which captured event types wake a look-ahead (explicit, not "every event").
    pub wake_on: Vec<String>,
    /// Minimum top-hit score before a suggestion is worth pushing.
    pub confidence: f32,
    /// Cap on how many CIDs a single suggestion carries.
    pub max_suggestions: usize,
    /// Token budget for the background look-ahead retrieval.
    pub budget_tokens: usize,
}

impl Default for InjectionConfig {
    fn default() -> Self {
        Self {
            proactive: false,
            wake_on: vec!["user_prompt".to_string()],
            confidence: 0.3,
            max_suggestions: 5,
            budget_tokens: 1000,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PublishingConfig {
    /// Selected publishing backend. `ipfs` is the free local default.
    pub backend: String,
    /// IPFS API endpoint used by the local Kubo backend.
    pub ipfs_api: String,
}

impl Default for PublishingConfig {
    fn default() -> Self {
        Self {
            backend: "ipfs".to_string(),
            ipfs_api: "http://127.0.0.1:5001/api/v0".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct IdentityConfig {
    /// Where the persisted Ed25519 AgentID keypair lives (outside the DAG).
    /// Generated once at `init`, reused on every start so the AgentID is stable.
    pub key_path: PathBuf,
    /// Whether this install participates as a `"human"` or an `"ai"`. Drives the
    /// AI-send lever (Phase 5.7): a Human-only room refuses sends from `ai`.
    pub kind: String,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            key_path: PathBuf::from(".concierge/identity.key"),
            kind: "human".to_string(),
        }
    }
}

impl Config {
    pub fn load_from_project_root(project_root: &Path) -> std::result::Result<Self, String> {
        let path = project_root.join(CONFIG_PATH);
        let exists = path
            .try_exists()
            .map_err(|e| format!("config existence check ({}): {e}", path.display()))?;
        if !exists {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("config read ({}): {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("config parse ({}): {e}", path.display()))
    }

    pub fn config_path(project_root: &Path) -> PathBuf {
        project_root.join(CONFIG_PATH)
    }

    pub fn save_to_project_root(&self, project_root: &Path) -> std::result::Result<(), String> {
        let path = Self::config_path(project_root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("config dir create ({}): {e}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| format!("config serialize: {e}"))?;
        std::fs::write(&path, text).map_err(|e| format!("config write ({}): {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn defaults_match_the_plan() {
        let cfg = Config::default();
        assert_eq!(cfg.store.root, PathBuf::from(".concierge"));
        assert_eq!(cfg.host.id, "default");
        assert_eq!(cfg.host.adapter, "jsonl");
        assert!(cfg.checkpoint.auto);
        assert_eq!(cfg.checkpoint.every_turns, 1);
        assert!(cfg.checkpoint.on_exit);
        assert_eq!(cfg.checkpoint.keep_checkpoints, 10);
        assert_eq!(cfg.publishing.backend, "ipfs");
        assert_eq!(cfg.publishing.ipfs_api, "http://127.0.0.1:5001/api/v0");
    }

    #[test]
    fn missing_config_yields_defaults() {
        let dir = TempDir::new().unwrap();
        let cfg = Config::load_from_project_root(dir.path()).unwrap();
        assert_eq!(cfg.store.root, PathBuf::from(".concierge"));
    }

    #[test]
    fn written_config_roundtrips() {
        let dir = TempDir::new().unwrap();
        let path = Config::config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[store]\nroot = \".concierge\"\n\n[host]\nid = \"hermes\"\nadapter = \"jsonl\"\n\n[checkpoint]\nauto = true\nevery_turns = 2\non_exit = false\nkeep_checkpoints = 7\n\n[publishing]\nbackend = \"ipfs\"\nipfs_api = \"http://127.0.0.1:5001/api/v0\"\n",
        )
        .unwrap();
        let cfg = Config::load_from_project_root(dir.path()).unwrap();
        assert_eq!(cfg.host.id, "hermes");
        assert_eq!(cfg.checkpoint.keep_checkpoints, 7);
        assert_eq!(cfg.publishing.backend, "ipfs");
    }
}
