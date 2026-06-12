//! The network backend contract (push-based). Adding a backend is a plugin
//! (see `ADDING_A_BACKEND.md`): implement `Backend` in `backends/<name>.rs`, add
//! one registry line, gate behind a Cargo feature. Core never grows.

use crate::blockstore::Blockstore;
use crate::cid::Cid;
use crate::config::Config;
use std::collections::BTreeMap;

/// A network backend owns its *push strategy* — different networks ingest very
/// differently (Pinata takes a whole CAR in one HTTP call; another might take
/// blocks one at a time). So the contract is push-based, not per-block.
pub trait Backend {
    fn manifest() -> BackendManifest
    where
        Self: Sized;

    fn from_config(cfg: &Config) -> anyhow::Result<Self>
    where
        Self: Sized;

    /// Publish the DAG rooted at `root` (and pin it). Read the reachable blocks
    /// from `local` (via `dag::reachable_from`) and ship them, preserving CIDs.
    fn push(&self, local: &dyn Blockstore, root: &Cid) -> anyhow::Result<()>;

    /// Fetch one block by CID, verifying the returned bytes hash back to `cid`.
    fn get_block(&self, cid: &Cid) -> anyhow::Result<Vec<u8>>;
}

/// Declarative config, mirroring Hermes' plugin.yaml `requires_env`. Drives the
/// prompts in `mem backend add <name>` without hardcoding anything in core.
#[derive(Debug, Clone)]
pub struct BackendManifest {
    pub name: &'static str,
    pub label: &'static str,
    pub requires: Vec<EnvKey>,
}

#[derive(Debug, Clone)]
pub struct EnvKey {
    pub key: &'static str,
    pub prompt: &'static str,
    pub url: Option<&'static str>,
    pub secret: bool,
}

/// Build a configured backend as a trait object.
pub type BackendFactory = fn(&Config) -> anyhow::Result<Box<dyn Backend>>;

/// A registry entry: the manifest (so `backend list` can show requirements
/// without constructing anything) plus the factory (to build when sharing).
#[derive(Clone)]
pub struct BackendEntry {
    pub manifest: BackendManifest,
    pub factory: BackendFactory,
}

/// A registry entry for a concrete backend type.
#[allow(dead_code)] // referenced only by feature-gated registry lines
fn entry<B: Backend + 'static>() -> BackendEntry {
    BackendEntry {
        manifest: B::manifest(),
        factory: |cfg| Ok(Box::new(B::from_config(cfg)?) as Box<dyn Backend>),
    }
}

/// name -> entry. Each feature-gated backend adds one line. Empty when no
/// backend features are enabled — local disk is the only always-on store.
pub fn registry() -> BTreeMap<&'static str, BackendEntry> {
    #[allow(unused_mut)]
    let mut m: BTreeMap<&'static str, BackendEntry> = BTreeMap::new();
    #[cfg(feature = "pinata")]
    {
        let e = entry::<crate::backends::pinata::Pinata>();
        m.insert(e.manifest.name, e);
    }
    #[cfg(feature = "ipfs")]
    {
        let e = entry::<crate::backends::ipfs::Ipfs>();
        m.insert(e.manifest.name, e);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBackend;
    impl Backend for MockBackend {
        fn manifest() -> BackendManifest {
            BackendManifest {
                name: "mock",
                label: "Mock backend",
                requires: vec![EnvKey {
                    key: "MOCK_TOKEN",
                    prompt: "Mock token",
                    url: None,
                    secret: true,
                }],
            }
        }
        fn from_config(_cfg: &Config) -> anyhow::Result<Self> {
            Ok(MockBackend)
        }
        fn push(&self, _local: &dyn Blockstore, _root: &Cid) -> anyhow::Result<()> {
            Ok(())
        }
        fn get_block(&self, _cid: &Cid) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("mock has no blocks")
        }
    }

    #[test]
    fn entry_exposes_the_backends_manifest() {
        let e = entry::<MockBackend>();
        assert_eq!(e.manifest.name, "mock");
        assert_eq!(e.manifest.requires[0].key, "MOCK_TOKEN");
        assert!(e.manifest.requires[0].secret);
    }

    #[test]
    fn factory_builds_a_trait_object() {
        let e = entry::<MockBackend>();
        let built = (e.factory)(&Config::default());
        assert!(built.is_ok());
    }

    #[cfg(not(any(feature = "pinata", feature = "ipfs")))]
    #[test]
    fn registry_is_empty_without_backend_features() {
        assert!(
            registry().is_empty(),
            "no network backends are compiled in by default"
        );
    }
}
