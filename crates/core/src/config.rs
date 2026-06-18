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
    pub update: UpdateConfig,
    pub brain: BrainConfig,
    pub network: NetworkConfig,
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
            embedding_model: "nomic-embed-text-v1.5".to_string(),
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

/// Autoupdater settings (AUTOUPDATER_PLAN.md §3, §6). The defaults encode the plan's
/// guardrails: rules auto-refresh on (the one scoped silent-egress exception), a ~6h
/// jittered poll, and a 14-day freshness window before the "rules may be stale" notice.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UpdateConfig {
    /// The publisher's rules IPNS name (`k51…`). Empty = use the baked default.
    pub rules_ipns: String,
    /// Poll interval for the rules channel, seconds. Default ~6h.
    pub poll_interval_secs: u64,
    /// Freshness window before the GUI shows "rules may be stale", days.
    pub freshness_days: u64,
    /// Master switch for automatic rules updates (the kill switch's persisted form).
    pub auto_rules: bool,
    /// `owner/repo` the app channel polls for binary releases.
    pub app_repo: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            rules_ipns: String::new(),
            poll_interval_secs: 6 * 60 * 60,
            freshness_days: 14,
            auto_rules: true,
            app_repo: "gekinthegame/Universal_Concierge_Plugin".to_string(),
        }
    }
}

impl UpdateConfig {
    /// The freshness window in seconds.
    pub fn freshness_secs(&self) -> u64 {
        self.freshness_days.saturating_mul(86_400)
    }
}

/// The "Brain" tab (brain-tab.md): the connected sovereign LLM Concierge talks to.
/// The engine itself is user-run and external; this only records which local endpoint
/// to probe and which model to route to.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BrainConfig {
    /// Base URL of the local OpenAI-compatible engine (oMLX default `:8000`).
    pub engine_base_url: String,
    /// The model the Brain tab routes requests to (the OpenAI `model` field). Empty =
    /// none selected yet.
    pub active_model: Option<String>,
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self {
            engine_base_url: "http://localhost:8000".to_string(),
            active_model: None,
        }
    }
}

/// Peer-discovery / rendezvous wiring (Network tab). The always-on "original"
/// Concierge can act as the shared **rendezvous point**, and every other node
/// registers + discovers against it so peers find each other beyond the LAN
/// (mDNS only reaches the same subnet). Driven from config so a GUI-launched app
/// — especially on Windows, where a double-clicked app barely inherits env vars —
/// keeps working across restarts. The matching env vars (`CONCIERGE_RENDEZVOUS_SERVER`,
/// `CONCIERGE_LISTEN_PORT`, `CONCIERGE_RENDEZVOUS`) still override these when set.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Be the shared rendezvous point. Requires a STABLE, publicly dialable address
    /// (`listen_port` pinned + a router port-forward), so others can register here.
    pub rendezvous_server: bool,
    /// Pin the libp2p listen port (TCP + QUIC/UDP). `0` = ephemeral (fine for an
    /// ordinary node). A rendezvous point MUST pin this so its address is stable and
    /// matches the forwarded port on the router.
    pub listen_port: u16,
    /// Multiaddr of the rendezvous point to register + discover at — e.g.
    /// `/ip4/<public-ip>/tcp/48011/p2p/<peer-id>`. Empty disables rendezvous (the
    /// node still uses mDNS + the public DHT).
    pub rendezvous: String,
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
        assert!(cfg.update.auto_rules);
        assert_eq!(cfg.update.poll_interval_secs, 6 * 60 * 60);
        assert_eq!(cfg.update.freshness_days, 14);
        assert_eq!(
            cfg.update.app_repo,
            "gekinthegame/Universal_Concierge_Plugin"
        );
    }

    #[test]
    fn network_defaults_are_inert_and_roundtrip() {
        // An unconfigured node is never a rendezvous server, uses an ephemeral port,
        // and registers against no one — discovery falls back to mDNS + the DHT.
        let cfg = Config::default();
        assert!(!cfg.network.rendezvous_server);
        assert_eq!(cfg.network.listen_port, 0);
        assert!(cfg.network.rendezvous.is_empty());

        let dir = TempDir::new().unwrap();
        let mut configured = Config::default();
        configured.network.rendezvous_server = true;
        configured.network.listen_port = 48011;
        configured.network.rendezvous =
            "/ip4/67.247.166.2/tcp/48011/p2p/12D3KooWL5cDq7wqi77eyBNtxPT6vqazKtd2mRFe3c29txfBi1Ky"
                .to_string();
        configured.save_to_project_root(dir.path()).unwrap();
        let reloaded = Config::load_from_project_root(dir.path()).unwrap();
        assert!(reloaded.network.rendezvous_server);
        assert_eq!(reloaded.network.listen_port, 48011);
        assert!(reloaded.network.rendezvous.contains("/tcp/48011/p2p/"));
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
