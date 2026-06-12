//! TOML config: where the store lives, the model endpoint, and trace verbosity.
//! Loaded once from `.concierge/config.toml` (or all-defaults if absent), then
//! threaded where needed. Backend secrets (tokens) come from the environment,
//! matching the `requires_env` manifest convention.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Fixed location of the config file, relative to the working directory. The
/// file is optional; absence means "use defaults".
pub const CONFIG_PATH: &str = ".concierge/config.toml";

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub store: StoreConfig,
    pub models: ModelsConfig,
    pub trace: TraceConfig,
    pub backend: BackendConfig,
    pub checkpoint: CheckpointConfig,
    pub verify: VerifierConfig,
}

/// Which registered network backend `mem share` uses (e.g. "pinata"). `None`
/// until set; sharing errors clearly when unset.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BackendConfig {
    pub name: Option<String>,
}

/// Auto-checkpoint policy — write-side only; recall stays manual. Defaults on:
/// the agent snapshots itself as it works (every turn) and on clean exit,
/// rebinding the well-known `name` pointer (`latest`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CheckpointConfig {
    pub auto: bool,
    pub every_turns: u32,
    pub on_exit: bool,
    pub name: String,
    /// GC retention: how many newest auto-checkpoints `mem gc` keeps before
    /// trimming the rest of the `parent` chain (Phase 5.3).
    pub keep_checkpoints: u32,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            auto: true,
            every_turns: 1,
            on_exit: true,
            name: "latest".to_string(),
            keep_checkpoints: 10,
        }
    }
}

/// Sandboxed real-tool verification for `/work`. Commands are detected from
/// project manifests and run from a temporary workspace copy; model output never
/// supplies a shell command.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct VerifierConfig {
    /// Run build/test verification after deterministic audit passes.
    pub enabled: bool,
    /// For JavaScript projects, run `npm install --ignore-scripts` inside the
    /// sandbox before build/test. Lifecycle scripts stay disabled.
    pub install: bool,
    /// Run test commands when the stack has a detectable test surface.
    pub test: bool,
    /// Hard timeout per verifier command.
    pub timeout_seconds: u64,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            install: true,
            test: true,
            timeout_seconds: 120,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    /// Root directory for blocks + the name index.
    pub root: PathBuf,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from(".concierge"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub provider: String,
    pub host: String,
    pub name: String,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "ollama".to_string(),
            host: "http://localhost:11434".to_string(),
            name: "llama3.2".to_string(),
        }
    }
}

/// Named model roles. `concierge` is always available (the conversational
/// front); extra roles (e.g. `worker`) are optional. A single-model setup is
/// just one `[models.concierge]` block. Providers are pluggable behind the
/// `Model` trait — `provider` selects which one builds a role.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct ModelsConfig(std::collections::BTreeMap<String, ModelConfig>);

impl Default for ModelsConfig {
    fn default() -> Self {
        let mut roles = std::collections::BTreeMap::new();
        roles.insert("concierge".to_string(), ModelConfig::default());
        Self(roles)
    }
}

impl ModelsConfig {
    /// The always-present conversational front. Falls back to defaults if the
    /// config names other roles but omits `concierge`.
    pub fn concierge(&self) -> ModelConfig {
        self.0.get("concierge").cloned().unwrap_or_default()
    }

    /// A named role, if configured.
    pub fn role(&self, name: &str) -> Option<&ModelConfig> {
        self.0.get(name)
    }

    /// All configured roles (for `mem model list`).
    pub fn roles(&self) -> impl Iterator<Item = (&str, &ModelConfig)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TraceConfig {
    /// 0 = silent, 1 = normal, 2 = verbose.
    pub verbosity: u8,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self { verbosity: 1 }
    }
}

impl Config {
    /// Load from `.concierge/config.toml`, or all-defaults if it doesn't exist.
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(Path::new(CONFIG_PATH))
    }

    /// Load from an explicit path (defaults if absent). Used by tests.
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        if path.try_exists()? {
            let text = std::fs::read_to_string(path)?;
            toml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("config parse ({}): {e}", path.display()))
        } else {
            Ok(Self::default())
        }
    }

    /// Directory holding content-addressed blocks.
    pub fn blocks_dir(&self) -> PathBuf {
        self.store.root.join("blocks")
    }

    /// Path to the mutable name index.
    pub fn names_path(&self) -> PathBuf {
        self.store.root.join("names.json")
    }

    /// Path to the tombstone ledger (GC death certificates). Sits beside the
    /// name index and the blocks, never inside the DAG.
    pub fn tombstones_path(&self) -> PathBuf {
        self.store.root.join("tombstones.json")
    }

    /// Fetch a required secret/config value from the environment (e.g. a
    /// backend token like `PINATA_JWT`), erroring clearly if it's unset.
    pub fn require(&self, key: &str) -> anyhow::Result<String> {
        std::env::var(key)
            .map_err(|_| anyhow::anyhow!("missing required environment variable: {key}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert_eq!(cfg.store.root, PathBuf::from(".concierge"));
        assert_eq!(cfg.blocks_dir(), PathBuf::from(".concierge/blocks"));
        assert_eq!(cfg.names_path(), PathBuf::from(".concierge/names.json"));
        assert_eq!(cfg.models.concierge().host, "http://localhost:11434");
        assert_eq!(cfg.models.concierge().provider, "ollama");
        assert_eq!(cfg.trace.verbosity, 1);
        assert!(cfg.checkpoint.auto);
        assert_eq!(cfg.checkpoint.every_turns, 1);
        assert!(cfg.checkpoint.on_exit);
        assert_eq!(cfg.checkpoint.name, "latest");
        assert_eq!(cfg.checkpoint.keep_checkpoints, 10);
        assert!(cfg.verify.enabled);
        assert!(cfg.verify.install);
        assert!(cfg.verify.test);
        assert_eq!(cfg.verify.timeout_seconds, 120);
        assert_eq!(
            cfg.tombstones_path(),
            PathBuf::from(".concierge/tombstones.json")
        );
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = TempDir::new().unwrap();
        let cfg = Config::load_from(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(cfg.store.root, PathBuf::from(".concierge"));
    }

    #[test]
    fn partial_toml_overrides_only_named_fields() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            "[store]\nroot = \"/data/mem\"\n\n[models.concierge]\nname = \"qwen2.5\"\n"
        )
        .unwrap();

        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.store.root, PathBuf::from("/data/mem"));
        assert_eq!(cfg.models.concierge().name, "qwen2.5");
        // untouched fields keep their defaults
        assert_eq!(cfg.models.concierge().host, "http://localhost:11434");
        assert_eq!(cfg.models.concierge().provider, "ollama");
        assert_eq!(cfg.trace.verbosity, 1);
        assert!(cfg.verify.enabled);
        assert!(cfg.verify.install);
        assert!(cfg.verify.test);
    }

    #[test]
    fn require_errors_when_env_var_is_unset() {
        let cfg = Config::default();
        assert!(cfg.require("MEM_DEFINITELY_UNSET_VAR_XYZ").is_err());
    }
}
