//! Phase 5 publishing backends.
//!
//! This module keeps backend selection and requirements separate from the core
//! storage binding. The local IPFS backend is the free default; optional
//! pin-service backends can be compiled in later behind feature flags.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::binding::{MemCli, PublishReceipt};
use crate::config::Config;
use crate::egress::EgressPlan;
use crate::error::{Error, Result};

/// A user-facing backend requirement item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRequirement {
    pub key: String,
    pub value: String,
}

/// A published backend entry for `backend list` / requirements display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    pub name: String,
    pub blurb: String,
    pub requirements: Vec<BackendRequirement>,
}

impl BackendInfo {
    pub fn requirements_summary(&self) -> String {
        self.requirements
            .iter()
            .map(|r| format!("{}={}", r.key, r.value))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Backend interface: a share operation plus requirements metadata.
trait PublishingBackend {
    fn info(&self) -> BackendInfo;
    fn share(&self, mem: &MemCli, approved: &EgressPlan) -> Result<PublishReceipt>;
}

/// The free local Kubo backend.
#[derive(Debug, Clone)]
struct IpfsBackend {
    api_url: String,
}

impl IpfsBackend {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            api_url: cfg.publishing.ipfs_api.clone(),
        }
    }

    fn node_url(&self) -> &str {
        &self.api_url
    }

    /// A quick, short-timeout TCP probe of the node's API port. Publishing is
    /// opt-in (Phase B), so an unreachable node is the normal "not set up yet"
    /// state — never a startup error. Connecting to a closed local port returns
    /// immediately (ECONNREFUSED); only a firewalled host hits the full timeout.
    fn reachable(&self) -> bool {
        let Ok((host, port, _)) = parse_http_url(self.node_url()) else {
            return false;
        };
        let Ok(addrs) = (host.as_str(), port).to_socket_addrs() else {
            return false;
        };
        addrs
            .into_iter()
            .any(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok())
    }

    fn gateway_url(&self, root: &crate::binding::Cid) -> String {
        format!("https://ipfs.io/ipfs/{}", root.0)
    }

    fn post_car(&self, car: &[u8]) -> Result<()> {
        let (host, port, path) = parse_http_url(self.node_url())?;
        let mut stream = TcpStream::connect((host.as_str(), port))
            .map_err(|e| Error::BackendDown(format!("is your node running? {e}")))?;
        let request = format!(
            "POST {path}/dag/import?pin-roots=true HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/vnd.ipld.car\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            car.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| Error::BackendDown(format!("is your node running? {e}")))?;
        stream
            .write_all(car)
            .map_err(|e| Error::BackendDown(format!("is your node running? {e}")))?;

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|e| Error::BackendDown(format!("is your node running? {e}")))?;
        let status = response.lines().next().ok_or_else(|| {
            Error::BackendDown("is your node running? empty response".to_string())
        })?;
        if !status.contains(" 2") {
            return Err(Error::BackendDown(format!(
                "ipfs node rejected CAR: {status}"
            )));
        }
        Ok(())
    }
}

impl PublishingBackend for IpfsBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: "ipfs".to_string(),
            blurb: "free local Kubo/IPFS node, CID-preserving".to_string(),
            requirements: vec![BackendRequirement {
                key: "IPFS_API".to_string(),
                value: self.api_url.clone(),
            }],
        }
    }

    fn share(&self, mem: &MemCli, approved: &EgressPlan) -> Result<PublishReceipt> {
        let car = mem.export_car(&approved.root)?;
        self.post_car(&car)?;
        Ok(PublishReceipt {
            root: approved.root.0.clone(),
            backend: "ipfs".to_string(),
            unix_time: now_secs(),
            gateway_url: self.gateway_url(&approved.root),
            // The MemCli `share` wrapper signs the root and fills these in.
            agent_id: String::new(),
            signature: String::new(),
            ipns_name: None,
            site_name: None,
        })
    }
}

/// The optional pin-service backend. Feature-gated and intentionally separate.
#[cfg(feature = "pinata")]
#[derive(Debug, Clone)]
pub struct PinataBackend {
    jwt_env: String,
}

#[cfg(feature = "pinata")]
impl Default for PinataBackend {
    fn default() -> Self {
        Self {
            jwt_env: "PINATA_JWT".to_string(),
        }
    }
}

#[cfg(feature = "pinata")]
impl PublishingBackend for PinataBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo {
            name: "pinata".to_string(),
            blurb: "optional paid persistence backend".to_string(),
            requirements: vec![BackendRequirement {
                key: self.jwt_env.clone(),
                value: "JWT token (paid plan required)".to_string(),
            }],
        }
    }

    fn share(&self, _mem: &MemCli, _approved: &EgressPlan) -> Result<PublishReceipt> {
        Err(Error::BackendDown(
            "pinata backend is feature-gated in this build".to_string(),
        ))
    }
}

/// Whether the *selected* publishing backend is reachable right now (Phase B).
/// Publishing is opt-in: a `false` here means "publishing is not set up on this
/// machine," not an error — capture, ingest, the store, and the GUI all work
/// offline regardless. This never runs on the startup path; callers poll it
/// lazily (e.g. the background stats refresh).
pub fn selected_backend_reachable(cfg: &Config) -> bool {
    match cfg.publishing.backend.as_str() {
        "ipfs" => IpfsBackend::from_config(cfg).reachable(),
        // Pin services are reachable in the sense that they need credentials, not
        // a local node; treat them as "configured" rather than probing here.
        #[cfg(feature = "pinata")]
        "pinata" => true,
        _ => false,
    }
}

/// Registry / selection helpers.
pub fn available_backends(cfg: &Config) -> Vec<BackendInfo> {
    let backends = vec![IpfsBackend::from_config(cfg).info()];
    #[cfg(feature = "pinata")]
    {
        backends.push(PinataBackend::default().info());
    }
    backends
}

pub(crate) fn share_via_selected_backend(
    mem: &MemCli,
    approved: &EgressPlan,
    cfg: &Config,
) -> Result<PublishReceipt> {
    if approved.backend != cfg.publishing.backend {
        return Err(Error::EgressPlanChanged(format!(
            "reviewed backend `{}` changed to `{}`",
            approved.backend, cfg.publishing.backend
        )));
    }
    if approved.backend_target != cfg.publishing.ipfs_api {
        return Err(Error::EgressPlanChanged(format!(
            "reviewed backend target `{}` changed to `{}`",
            approved.backend_target, cfg.publishing.ipfs_api
        )));
    }
    match cfg.publishing.backend.as_str() {
        "ipfs" => IpfsBackend::from_config(cfg).share(mem, approved),
        #[cfg(feature = "pinata")]
        "pinata" => PinataBackend::default().share(mem, approved),
        other => Err(Error::BackendDown(format!(
            "backend `{other}` is not configured"
        ))),
    }
}

pub fn backend_exists(name: &str) -> bool {
    matches!(name, "ipfs") || {
        #[cfg(feature = "pinata")]
        {
            name == "pinata"
        }
        #[cfg(not(feature = "pinata"))]
        {
            false
        }
    }
}

fn parse_http_url(url: &str) -> Result<(String, u16, String)> {
    let url = url.strip_prefix("http://").ok_or_else(|| {
        Error::Io("IPFS_API must use an http:// URL for the local node".to_string())
    })?;
    let (host_port, path) = match url.split_once('/') {
        Some((host_port, rest)) => (host_port, format!("/{rest}")),
        None => (url, String::from("/")),
    };
    let (host, port) = match host_port.split_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|e| Error::Io(format!("invalid IPFS_API port: {e}")))?;
            (host.to_string(), port)
        }
        None => (host_port.to_string(), 80),
    };
    Ok((host, port, path))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn cfg_with_ipfs(api: &str) -> Config {
        let mut cfg = Config::default();
        cfg.publishing.backend = "ipfs".to_string();
        cfg.publishing.ipfs_api = api.to_string();
        cfg
    }

    #[test]
    fn an_unreachable_node_reads_as_not_set_up_not_an_error() {
        // Bind then drop to get a port that is guaranteed not listening.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let cfg = cfg_with_ipfs(&format!("http://127.0.0.1:{port}/api/v0"));
        // Publishing is opt-in: an absent node is simply "not reachable", never a panic.
        assert!(!selected_backend_reachable(&cfg));
    }

    #[test]
    fn a_listening_node_reads_as_reachable() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let cfg = cfg_with_ipfs(&format!("http://127.0.0.1:{port}/api/v0"));
        assert!(selected_backend_reachable(&cfg));
        drop(listener);
    }

    #[test]
    fn an_unknown_backend_is_never_reachable() {
        let mut cfg = Config::default();
        cfg.publishing.backend = "does-not-exist".to_string();
        assert!(!selected_backend_reachable(&cfg));
    }
}
