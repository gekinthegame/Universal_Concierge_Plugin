//! The private node: launching the user's Kubo (IPFS) node and the on-node
//! embedding model together, as one **"Enable Sidekick"** act.
//!
//! Product framing (not "deploy Kubo"): the user enables the **Sidekick** — the
//! small on-node embedding model (the Librarian, Phase 8 §1). The Sidekick needs
//! a running Kubo node, and the node only runs as part of the Sidekick — they
//! come up and go down **together** (the node-stack-as-a-unit, Decisions 0022/0028).
//!
//! The node is **private**: a dedicated IPFS repo under the store (`<store>/ipfs`,
//! its own `IPFS_PATH`), separate from any general IPFS the user runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rand_core::{OsRng, RngCore};

use crate::binding::MemCli;
use crate::error::{Error, Result};

/// Shown before enabling — makes the coupling explicit.
pub const SIDEKICK_DISCLAIMER: &str = "Enabling the Sidekick launches your private \
Kubo node. The Sidekick is the on-node embedding model that powers retrieval.";

/// A snapshot of Sidekick/node state for the UI and CLI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SidekickStatus {
    /// Is the `ipfs` (Kubo) binary installed and runnable?
    pub kubo_installed: bool,
    /// Is the node's API reachable right now?
    pub node_running: bool,
    /// Has the user enabled the Sidekick (persisted consent)?
    pub enabled: bool,
    /// Operational = enabled **and** the node is actually up.
    pub operational: bool,
    /// The disclaimer text the UI shows before enabling.
    pub disclaimer: &'static str,
}

/// Resolve the Kubo (`ipfs`) binary the Sidekick should run. Preference order:
/// 1. `CONCIERGE_IPFS_BIN` — explicit override (tests / unusual layouts).
/// 2. A **bundled** binary shipped *with the app* — next to the running executable
///    (`<exe_dir>/ipfs`) or, on a macOS `.app`, the sibling `Resources/ipfs`. This is
///    what makes the Sidekick seamless: the one-click installer brings Kubo along,
///    so the user never installs IPFS separately (Decision: bundle Kubo).
/// 3. A system `ipfs` on `PATH` — the developer / "already have IPFS" fallback.
pub fn kubo_binary() -> PathBuf {
    const BIN: &str = if cfg!(windows) { "ipfs.exe" } else { "ipfs" };
    if let Some(path) = std::env::var_os("CONCIERGE_IPFS_BIN") {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [dir.join(BIN), dir.join("..").join("Resources").join(BIN)] {
                if candidate.exists() {
                    return candidate;
                }
            }
        }
    }
    PathBuf::from(BIN)
}

/// Whether a Kubo (`ipfs`) binary — bundled or on `PATH` — is installed and runnable.
pub fn kubo_installed() -> bool {
    Command::new(kubo_binary())
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// The private IPFS repo path for this store's node.
fn ipfs_repo(store_dir: &Path) -> PathBuf {
    store_dir.join("ipfs")
}

/// Whether this store's dedicated private Kubo daemon is reachable. The repo's
/// runtime `api` file identifies the exact daemon endpoint, so an unrelated
/// system or public-publishing node cannot make Sidekick status look healthy.
pub fn private_node_running(store_dir: &Path) -> bool {
    let repo = ipfs_repo(store_dir);
    let Ok(api) = std::fs::read_to_string(repo.join("api")) else {
        return false;
    };
    Command::new(kubo_binary())
        .env("IPFS_PATH", &repo)
        .args(["--api", api.trim(), "id"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn stop_private_node(store_dir: &Path) -> Result<()> {
    if !private_node_running(store_dir) {
        return Ok(());
    }
    let repo = ipfs_repo(store_dir);
    let status = Command::new(kubo_binary())
        .env("IPFS_PATH", &repo)
        .arg("shutdown")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| Error::Io(format!("stop private ipfs daemon: {e}")))?;
    if !status.success() {
        return Err(Error::BackendDown(
            "private ipfs daemon did not accept shutdown".to_string(),
        ));
    }
    for _ in 0..20 {
        if !private_node_running(store_dir) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(Error::BackendDown(
        "private ipfs daemon is still running after shutdown".to_string(),
    ))
}

/// Make the node a **private swarm**, not just a private repo: write a swarm key
/// (PSK) so only nodes holding the same key can connect, and drop the public
/// bootstrap peers so the node doesn't dial public IPFS. Combined with
/// `LIBP2P_FORCE_PNET=1` on the daemon, the node refuses any non-private peer.
/// (This is necessary — *not* sufficient — for "the graph on the node is secure":
/// locked content must still be stored as capability-encrypted inert ciphertext,
/// see `NETWORK_DEFENSE_PLAN.md`. A default Kubo node otherwise serves any block
/// by CID over the public DHT, which would bypass the egress lock.)
fn provision_private_swarm(repo: &Path) -> Result<()> {
    let key_path = repo.join("swarm.key");
    if !key_path.exists() {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(
            &key_path,
            format!("/key/swarm/psk/1.0.0/\n/base16/\n{hex}\n"),
        )
        .map_err(|e| Error::Io(format!("write swarm key: {e}")))?;
    }
    // Drop public bootstrap peers so the private node never dials public IPFS.
    let _ = Command::new(kubo_binary())
        .env("IPFS_PATH", repo)
        .args(["bootstrap", "rm", "--all"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    // Kubo ≥ 0.42 ships AutoConf, which **refuses to start on a private swarm**
    // while pointed at the default mainnet config URL ("AutoConf cannot use the
    // default mainnet URL on a private network"). Disable it so the private daemon
    // comes up; a private swarm has no use for mainnet auto-configuration anyway.
    let _ = Command::new(kubo_binary())
        .env("IPFS_PATH", repo)
        .args(["config", "--json", "AutoConf.Enabled", "false"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

/// Launch the private Kubo node: initialise the dedicated repo if needed, then
/// spawn `ipfs daemon` detached against that repo. Best-effort and honest — if
/// Kubo is not installed, returns a clear, actionable error.
pub fn launch_private_node(store_dir: &Path) -> Result<()> {
    if !kubo_installed() {
        return Err(Error::BackendDown(
            "Kubo (ipfs) is not installed. Install IPFS/Kubo to enable the Sidekick — https://docs.ipfs.tech/install/".to_string(),
        ));
    }
    let repo = ipfs_repo(store_dir);
    // Initialise the private repo on first run.
    if !repo.join("config").exists() {
        std::fs::create_dir_all(&repo)
            .map_err(|e| Error::Io(format!("create private ipfs repo: {e}")))?;
        let init = Command::new(kubo_binary())
            .env("IPFS_PATH", &repo)
            .arg("init")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| Error::Io(format!("ipfs init: {e}")))?;
        if !init.success() {
            return Err(Error::BackendDown(
                "ipfs init failed for the private node".to_string(),
            ));
        }
    }
    // Make it a private swarm (key + no public bootstrap), not just a private repo.
    provision_private_swarm(&repo)?;
    // Spawn the daemon detached, forced into private-network mode so it refuses
    // any peer that does not hold the swarm key.
    Command::new(kubo_binary())
        .env("IPFS_PATH", &repo)
        .env("LIBP2P_FORCE_PNET", "1")
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::Io(format!("launch ipfs daemon: {e}")))?;
    Ok(())
}

// ── Public publishing node (the Planet Pattern) ─────────────────────────────
//
// Web publishing needs a node on the PUBLIC IPFS network — the Sidekick node is a
// private swarm (PNET, no public bootstrap) and physically cannot serve content to
// the public web. So publishing runs against a *separate* public Kubo repo
// (`<store>/ipfs-public`) on its own ports, launched on demand. All `key gen` /
// `add` / `name publish` run against this node. Websites must be real **UnixFS**
// directories (`ipfs add -r`) — a gateway will not serve our DAG-CBOR graph as a
// site (proven: Planet / filecoin-pin).

/// Ports for the public publishing node, distinct from the private node's
/// defaults (5001 API / 8080 gateway / 4001 swarm) so both can run at once.
pub const PUBLIC_API_PORT: u16 = 5011;
const PUBLIC_GATEWAY_PORT: u16 = 8090;
const PUBLIC_SWARM_PORT: u16 = 4011;

/// The public publishing repo for this store.
fn ipfs_public_repo(store_dir: &Path) -> PathBuf {
    store_dir.join("ipfs-public")
}

fn ipfs(repo: &Path) -> Command {
    let mut cmd = Command::new(kubo_binary());
    cmd.env("IPFS_PATH", repo);
    cmd
}

/// Is the public node's API accepting connections right now?
pub fn public_node_running() -> bool {
    std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], PUBLIC_API_PORT).into(),
        std::time::Duration::from_millis(400),
    )
    .is_ok()
}

/// Launch the public publishing node (idempotent): init a public repo on its own
/// ports (kept on the public network — no swarm key, default bootstrap) and spawn
/// the daemon if it isn't already up. This is what makes a published site reachable
/// on the global IPFS network.
pub fn launch_public_node(store_dir: &Path) -> Result<()> {
    if !kubo_installed() {
        return Err(Error::BackendDown(
            "Kubo (ipfs) is not installed. Install IPFS/Kubo to publish — https://docs.ipfs.tech/install/".to_string(),
        ));
    }
    let repo = ipfs_public_repo(store_dir);
    if !repo.join("config").exists() {
        std::fs::create_dir_all(&repo)
            .map_err(|e| Error::Io(format!("create public ipfs repo: {e}")))?;
        let init = ipfs(&repo)
            .arg("init")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| Error::Io(format!("ipfs init (public): {e}")))?;
        if !init.success() {
            return Err(Error::BackendDown(
                "ipfs init failed for the public node".to_string(),
            ));
        }
        // Move off the private node's default ports so both daemons coexist.
        ipfs_set(&repo, &["config", "Addresses.API", &format!("/ip4/127.0.0.1/tcp/{PUBLIC_API_PORT}")]);
        ipfs_set(&repo, &["config", "Addresses.Gateway", &format!("/ip4/127.0.0.1/tcp/{PUBLIC_GATEWAY_PORT}")]);
    }
    if !public_node_running() {
        // Apply reachability config every (re)start so existing repos get upgraded —
        // it only takes effect when the daemon starts, so a running node needs a
        // restart to pick it up.
        ensure_public_reachability(&repo);
        // No LIBP2P_FORCE_PNET here — this node MUST reach the public network.
        ipfs(&repo)
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Io(format!("launch public ipfs daemon: {e}")))?;
    }
    Ok(())
}

fn ipfs_set(repo: &Path, args: &[&str]) {
    let _ = ipfs(repo)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Configure the public node for the best shot at being reachable from outside the
/// LAN: listen on **WebTransport** (so browser/service-worker gateways like
/// `inbrowser.link` can fetch directly), and enable **relay client + hole-punching**
/// so a NAT'd home node can still be dialed. Idempotent; takes effect on daemon
/// start. (None of this beats NAT outright — port-forwarding `PUBLIC_SWARM_PORT`
/// TCP+UDP, or pinning to a reachable peer, is the guaranteed path.)
fn ensure_public_reachability(repo: &Path) {
    ipfs_set(
        repo,
        &[
            "config",
            "--json",
            "Addresses.Swarm",
            &format!(
                "[\"/ip4/0.0.0.0/tcp/{P}\",\"/ip4/0.0.0.0/udp/{P}/quic-v1\",\"/ip4/0.0.0.0/udp/{P}/quic-v1/webtransport\"]",
                P = PUBLIC_SWARM_PORT
            ),
        ],
    );
    ipfs_set(repo, &["config", "--json", "Swarm.RelayClient.Enabled", "true"]);
    ipfs_set(repo, &["config", "--json", "Swarm.EnableHolePunching", "true"]);
    // Announce content to the public DHT so providers can be found.
    ipfs_set(repo, &["config", "Routing.Type", "auto"]);
}

/// The IPNS key id (`k51…`) for `site`, if a key by that name already exists.
fn ipns_key_id(repo: &Path, site: &str) -> Result<Option<String>> {
    let out = ipfs(repo)
        .args(["key", "list", "-l"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Io(format!("ipfs key list: {e}")))?;
    if !out.status.success() {
        return Ok(None);
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut cols = line.split_whitespace();
        let id = cols.next();
        let name = cols.next();
        if name == Some(site) {
            if let Some(id) = id {
                return Ok(Some(id.to_string()));
            }
        }
    }
    Ok(None)
}

/// Generate (or return the existing) per-site IPNS keypair; returns the `k51…`
/// IPNS name that is the site's stable public address.
pub fn ipns_key_gen(repo: &Path, site: &str) -> Result<String> {
    if let Some(id) = ipns_key_id(repo, site)? {
        return Ok(id);
    }
    let out = ipfs(repo)
        .args(["key", "gen", "--type=ed25519", site])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Io(format!("ipfs key gen: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "ipfs key gen failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Export a site's IPNS private key to `out_path` (key portability/backup).
pub fn ipns_key_export(repo: &Path, site: &str, out_path: &Path) -> Result<()> {
    let status = ipfs(repo)
        .args(["key", "export", "-o", &out_path.to_string_lossy(), site])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| Error::Io(format!("ipfs key export: {e}")))?;
    if !status.success() {
        return Err(Error::Io(format!("ipfs key export failed for {site}")));
    }
    Ok(())
}

/// Add a folder to IPFS as a real **UnixFS** directory (a gateway serves it as a
/// website, `index.html` at the root). Returns the directory's CIDv1.
pub fn unixfs_add_dir(repo: &Path, folder: &Path) -> Result<String> {
    let out = ipfs(repo)
        .args([
            "add",
            "-r",
            "-Q",
            "--cid-version=1",
            &folder.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Io(format!("ipfs add: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "ipfs add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let cid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if cid.is_empty() {
        return Err(Error::Io("ipfs add returned no CID".to_string()));
    }
    Ok(cid)
}

/// Publish a directory CID to the site's IPNS name. Returns the `k51…` name it was
/// published to (resolvable at `/ipns/<name>`). Requires the public daemon to be up.
pub fn ipns_publish(repo: &Path, cid: &str, site: &str) -> Result<String> {
    let out = ipfs(repo)
        .args([
            "name",
            "publish",
            &format!("--key={site}"),
            &format!("/ipfs/{cid}"),
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Io(format!("ipfs name publish: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "ipfs name publish failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // Output: "Published to k51…: /ipfs/<cid>"
    let text = String::from_utf8_lossy(&out.stdout);
    let name = text
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .trim_end_matches(':')
        .to_string();
    if name.is_empty() {
        // Fall back to the key's known id.
        return ipns_key_id(repo, site)?
            .ok_or_else(|| Error::Io("could not determine published IPNS name".to_string()));
    }
    Ok(name)
}

/// The public publishing repo path for this store (used by `MemCli`).
pub fn public_repo_for(store_dir: &Path) -> PathBuf {
    ipfs_public_repo(store_dir)
}

impl MemCli {
    /// The Sidekick-enabled sentinel (persisted consent), under the store.
    fn sidekick_flag_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("sidekick-enabled"))
    }

    fn sidekick_enabled(&self) -> bool {
        self.sidekick_flag_path()
            .map(|p| p.exists())
            .unwrap_or(false)
    }

    /// Current Sidekick/node status for the UI and CLI.
    pub fn sidekick_status(&self) -> SidekickStatus {
        let node_running = self
            .store_dir()
            .map(|store| private_node_running(&store))
            .unwrap_or(false);
        let enabled = self.sidekick_enabled();
        SidekickStatus {
            kubo_installed: kubo_installed(),
            node_running,
            enabled,
            operational: enabled && node_running,
            disclaimer: SIDEKICK_DISCLAIMER,
        }
    }

    /// Enable the Sidekick: launch the private node and persist consent. The
    /// embedding-model Sidekick becomes operational once the node is up.
    pub fn enable_sidekick(&self) -> Result<SidekickStatus> {
        let store = self.store_dir()?;
        // Launch only if the node isn't already reachable.
        if !self.sidekick_status().node_running {
            launch_private_node(&store)?;
        }
        let flag = self.sidekick_flag_path()?;
        if let Some(parent) = flag.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Io(format!("create store dir: {e}")))?;
        }
        std::fs::write(&flag, b"enabled")
            .map_err(|e| Error::Io(format!("persist sidekick flag: {e}")))?;
        Ok(self.sidekick_status())
    }

    /// Whether the host AI may use the MCP **write** tools (put_node/put_blob/bind/
    /// write_site). Off by default (Decision 0028: write-enabled is opt-in). The MCP
    /// server reads this dynamically, so the GUI toggle takes effect on the AI's next
    /// call — no re-registration. A persistent sentinel under the store.
    pub fn mcp_write_enabled(&self) -> bool {
        self.store_dir()
            .map(|dir| dir.join("mcp-write-enabled").exists())
            .unwrap_or(false)
    }

    /// Enable/disable the MCP write tools (the GUI toggle).
    pub fn set_mcp_write_enabled(&self, enabled: bool) -> Result<()> {
        let path = self.store_dir()?.join("mcp-write-enabled");
        if enabled {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| Error::Io(e.to_string()))?;
            }
            std::fs::write(&path, b"enabled")
                .map_err(|e| Error::Io(format!("set mcp write: {e}")))?;
        } else if path.exists() {
            std::fs::remove_file(&path).map_err(|e| Error::Io(format!("clear mcp write: {e}")))?;
        }
        Ok(())
    }

    /// Disable the Sidekick and stop its dedicated private node.
    pub fn disable_sidekick(&self) -> Result<SidekickStatus> {
        stop_private_node(&self.store_dir()?)?;
        let flag = self.sidekick_flag_path()?;
        if flag.exists() {
            std::fs::remove_file(&flag)
                .map_err(|e| Error::Io(format!("clear sidekick flag: {e}")))?;
        }
        Ok(self.sidekick_status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reports_disclaimer_and_enable_disable_toggles_consent() {
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());

        let status = mem.sidekick_status();
        assert!(!status.enabled, "off until enabled");
        assert!(!status.operational, "not operational while disabled");
        assert!(status.disclaimer.contains("private Kubo node"));
        assert!(status.disclaimer.contains("embedding model"));

        // disable is idempotent and safe when never enabled.
        assert!(!mem.disable_sidekick().unwrap().enabled);
    }

    #[test]
    fn enabling_without_kubo_is_an_honest_error_not_a_silent_pass() {
        // On a machine without `ipfs`, enable surfaces an actionable error and
        // does NOT mark the Sidekick enabled. (When Kubo *is* installed this test
        // still holds: the node either was already running or launches.)
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        if kubo_installed() {
            return; // can't assert the not-installed path on a Kubo machine
        }
        let result = mem.enable_sidekick();
        assert!(result.is_err(), "no Kubo → enable fails");
        assert!(!mem.sidekick_status().enabled, "stays disabled on failure");
    }

    #[test]
    fn operational_requires_both_enabled_and_node_running() {
        // Pure coupling check: operational == enabled && node_running.
        let dir = tempfile::tempdir().unwrap();
        let mem = MemCli::new(dir.path());
        let s = mem.sidekick_status();
        assert_eq!(s.operational, s.enabled && s.node_running);
    }
}
