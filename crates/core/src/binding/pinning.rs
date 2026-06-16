use super::*;

/// Receipt for a successful pin: the site's root CID + its stable IPNS name (so the
/// `/ipns/<k51>` link resolves), plus which service now holds the content.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PinReceipt {
    pub cid: String,
    pub ipns_name: String,
    pub service: String,
    pub status: String,
    pub request_id: String,
    pub endpoint: String,
}

impl MemCli {
    /// On-device pinning-credentials vault (`<store>/security/pin.json`, 0600). Tokens
    /// live here and go only to their own service's API.
    pub(super) fn pin_credentials_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("pin.json"))
    }

    /// Load stored pinning credentials (empty if none configured yet).
    pub fn pin_credentials(&self) -> Result<crate::pinning::PinCredentials> {
        let path = self.pin_credentials_path()?;
        if path
            .try_exists()
            .map_err(|e| Error::Io(format!("inspect pin credentials: {e}")))?
        {
            crate::egress::validate_private_file(&path)?;
        }
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Io(format!("parse pin credentials: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::pinning::PinCredentials::default())
            }
            Err(e) => Err(Error::Io(format!("read pin credentials: {e}"))),
        }
    }

    /// Set (merge) one pinning service's `{endpoint, token}` from JSON, written 0600.
    /// An explicit JSON `null` clears it.
    pub fn set_pin_credentials(&self, service: &str, fields_json: &str) -> Result<()> {
        let mut creds = self.pin_credentials()?;
        let value: serde_json::Value = serde_json::from_str(fields_json)
            .map_err(|e| Error::Io(format!("parse credential fields: {e}")))?;
        let parsed: Option<crate::pinning::PinService> = if value.is_null() {
            None
        } else {
            Some(
                serde_json::from_value(value)
                    .map_err(|e| Error::Io(format!("invalid {service} credentials: {e}")))?,
            )
        };
        if let Some(svc) = &parsed {
            if svc.token.trim().is_empty() || svc.token.chars().any(char::is_control) {
                return Err(Error::SecurityPolicy(
                    "pinning token must be non-empty and free of control characters".to_string(),
                ));
            }
            if !svc.endpoint.starts_with("https://") || svc.endpoint.chars().any(char::is_control) {
                return Err(Error::SecurityPolicy(
                    "pinning endpoint must be an https:// URL with no control characters"
                        .to_string(),
                ));
            }
        }
        match service {
            "filebase" => creds.filebase = parsed,
            "pinata" => creds.pinata = parsed,
            "foureverland" => creds.foureverland = parsed,
            "ipfs" => creds.ipfs = parsed,
            other => return Err(Error::Io(format!("unknown pinning service: {other}"))),
        }
        self.ensure_security_dir()?;
        let bytes = serde_json::to_vec_pretty(&creds)
            .map_err(|e| Error::Io(format!("serialize pin credentials: {e}")))?;
        crate::egress::atomic_private_write(&self.pin_credentials_path()?, &bytes)
    }

    /// Non-secret status: which services are configured + their endpoints (never tokens).
    pub fn pin_status(&self) -> Result<serde_json::Value> {
        let creds = self.pin_credentials()?;
        let one = |service: &Option<crate::pinning::PinService>| {
            service
                .as_ref()
                .map(|svc| serde_json::json!({ "endpoint": svc.endpoint }))
        };
        Ok(serde_json::json!({
            "filebase": one(&creds.filebase),
            "pinata": one(&creds.pinata),
            "foureverland": one(&creds.foureverland),
            "ipfs": one(&creds.ipfs),
        }))
    }

    /// Verify a service's credentials live against its API (the "Test connection" step).
    /// Tests unsaved `fields_json` if given, else the stored credentials.
    pub fn verify_pin_credentials(
        &self,
        service: &str,
        fields_json: Option<&str>,
    ) -> Result<String> {
        let svc = match fields_json {
            Some(json) if !json.trim().is_empty() && json.trim() != "null" => {
                serde_json::from_str::<crate::pinning::PinService>(json)
                    .map_err(|e| Error::Io(format!("invalid {service} credentials: {e}")))?
            }
            _ => self
                .pin_credentials()?
                .get(service)
                .cloned()
                .ok_or_else(|| Error::Io(format!("no {service} credentials yet")))?,
        };
        crate::pinning::verify(service, &svc).map_err(Error::Io)
    }

    /// Publish the site to IPFS (so its `/ipns/<k51>` name resolves) AND pin its root
    /// CID to an always-on pinning service, so the site stays reachable even when this
    /// node is offline. Password-gated: pinning is explicit, public egress.
    pub fn pin_site(
        &self,
        name: &str,
        folder: &str,
        service: &str,
        password: &str,
    ) -> Result<PinReceipt> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        if !std::path::Path::new(folder).join("index.html").is_file() {
            return Err(Error::Io(
                "only a website (with an index.html) can be pinned".to_string(),
            ));
        }
        let svc = self
            .pin_credentials()?
            .get(service)
            .cloned()
            .ok_or_else(|| {
                Error::Io(format!(
                    "no {service} credentials yet — add them in Studio → Pin settings"
                ))
            })?;
        let store = self.store_dir()?;
        let repo = crate::node::public_repo_for(&store);
        crate::node::launch_public_node(&store)?;
        self.wait_for_public_node()?;
        let ipns = crate::node::ipns_key_gen(&repo, name)?;

        // Each service uses its most reliable native path:
        //  • Filebase — build a CAR of the site and PUT it to S3 (`x-amz-meta-import: car`,
        //    SigV4), exactly like the @filebase/sdk. Filebase imports + pins the CAR root.
        //  • Pinata — direct multipart upload (its free plan disallows pin-by-CID).
        //  • everyone else — the Pinning Services API: add locally, pin that CID, and
        //    connect to the service's delegates so it can pull the blocks from us now.
        // The first two PUSH the bytes to the service, so they work even when this node
        // isn't publicly reachable.
        let (cid, status, request_id) = match service {
            "filebase" => {
                let cid = crate::node::unixfs_add_dir(&repo, std::path::Path::new(folder))?;
                let car = crate::node::dag_export_car(&repo, &cid)?;
                let (key, secret, bucket) =
                    crate::pinning::decode_filebase_token(&svc.token).map_err(Error::Io)?;
                // Auto-create the IPFS bucket if it doesn't exist yet, then push the CAR.
                crate::pinning::filebase_s3_ensure_bucket(&key, &secret, &bucket)
                    .map_err(Error::Io)?;
                crate::pinning::filebase_s3_put_car(&key, &secret, &bucket, &cid, &car)
                    .map_err(Error::Io)?;
                let _ = crate::node::dht_provide(&repo, &cid); // also seed from our node
                (cid, "pinned".to_string(), String::new())
            }
            "pinata" => {
                let files = crate::deploy::walk_files(std::path::Path::new(folder))
                    .map_err(Error::Io)?
                    .into_iter()
                    .map(|f| (f.rel, f.bytes))
                    .collect::<Vec<_>>();
                let cid = crate::pinning::upload_pinata_dir(&svc.token, &files, name)
                    .map_err(Error::Io)?;
                (cid, "pinned".to_string(), String::new())
            }
            _ => {
                let cid = crate::node::unixfs_add_dir(&repo, std::path::Path::new(folder))?;
                let _ = crate::node::dht_provide(&repo, &cid);
                let outcome = crate::pinning::pin_cid(&svc, &cid, name).map_err(Error::Io)?;
                for addr in &outcome.delegates {
                    let _ = crate::node::swarm_connect(&repo, addr);
                }
                (cid, outcome.status, outcome.request_id)
            }
        };

        // Point the site's stable IPNS name at the pinned CID, so `/ipns/<k51>` resolves
        // to content the service hosts (reachable even when this node is offline).
        let published = crate::node::ipns_publish(&repo, &cid, name).unwrap_or(ipns);
        Ok(PinReceipt {
            cid,
            ipns_name: published,
            service: service.to_string(),
            status,
            request_id,
            endpoint: svc.endpoint,
        })
    }

    /// Pin a single **record** (any node in the store, by CID) to an always-on pinning
    /// service, so a copy survives off-device even when this node is asleep. A record is
    /// raw IPLD that lives only in the mem store (never auto-loaded into Kubo), so we
    /// export its subgraph as a CAR and hand that to the service.
    ///
    /// `private` chooses what crosses the wire:
    ///   • **false (Public)** — the plaintext subgraph CAR. Anyone with the CID can read
    ///     it. Public egress.
    ///   • **true (Private)** — the subgraph is Cryptree-encrypted *first*
    ///     ([`MemCli::encrypt_subgraph_for_pin`]); only the opaque ciphertext CAR is
    ///     pushed. The service blind-pins inert bytes it cannot read; the owner keeps the
    ///     read capability locally.
    ///
    /// Routing mirrors [`Self::pin_site`]: Filebase takes a direct CAR push (works even
    /// when this node is unreachable); the PSA services stage the CAR on the public node,
    /// announce it, then pin by CID. Password-gated either way (pinning is egress).
    pub fn pin_record(
        &self,
        cid: &str,
        service: &str,
        private: bool,
        password: &str,
    ) -> Result<RecordPinReceipt> {
        let source = Cid(cid.to_string());

        // Resolve the bytes to pin + the CID to pin them under. Private encrypts first
        // (which password-gates + locks internally); public takes the policy lock just
        // long enough to verify the password and snapshot the plaintext CAR.
        let (pinned, car) = if private {
            self.encrypt_subgraph_for_pin(&source, password)?
        } else {
            let _policy_lock = self.policy_lock()?;
            self.verify_password_unlocked(password)?;
            (source.clone(), self.export_car(&source)?)
        };

        let label = format!(
            "ucp-record-{}{}",
            if private { "enc-" } else { "" },
            &pinned.0
        );
        let store = self.store_dir()?;

        let (status, request_id, endpoint) = match service {
            "node" => {
                // Sovereign path: seed the (cipher)blocks into THIS user's own private
                // Kubo node and pin them there, so the always-on node serves them to the
                // user's paired devices over the private swarm — no third party. The
                // bytes stay encrypted (Private); only paired devices holding the key can
                // read them. Requires the Sidekick node (auto-launched if enabled).
                let priv_repo = crate::node::private_repo_for(&store);
                if !crate::node::private_node_running(&store) {
                    crate::node::launch_private_node(&store)?;
                    self.wait_for_private_node()?;
                }
                crate::node::dag_import_and_pin(&priv_repo, &pinned.0, &car)?;
                let _ = crate::node::dht_provide(&priv_repo, &pinned.0);
                self.record_hot_pin(&source.0, &pinned.0, private)?;
                (
                    "hot".to_string(),
                    String::new(),
                    "private node (swarm)".to_string(),
                )
            }
            "filebase" => {
                let svc = self.require_pin_service(service)?;
                let (key, secret, bucket) =
                    crate::pinning::decode_filebase_token(&svc.token).map_err(Error::Io)?;
                crate::pinning::filebase_s3_ensure_bucket(&key, &secret, &bucket)
                    .map_err(Error::Io)?;
                crate::pinning::filebase_s3_put_car(&key, &secret, &bucket, &pinned.0, &car)
                    .map_err(Error::Io)?;
                ("pinned".to_string(), String::new(), svc.endpoint)
            }
            _ => {
                // PSA services pull by CID: stage the CAR on our public node + announce it
                // so the service can fetch the DAG, then ask it to pin.
                let svc = self.require_pin_service(service)?;
                let repo = crate::node::public_repo_for(&store);
                crate::node::launch_public_node(&store)?;
                self.wait_for_public_node()?;
                crate::node::dag_import_car(&repo, &car)?;
                let _ = crate::node::dht_provide(&repo, &pinned.0);
                let outcome =
                    crate::pinning::pin_cid(&svc, &pinned.0, &label).map_err(Error::Io)?;
                for addr in &outcome.delegates {
                    let _ = crate::node::swarm_connect(&repo, addr);
                }
                (outcome.status, outcome.request_id, svc.endpoint)
            }
        };

        Ok(RecordPinReceipt {
            cid: pinned.0,
            source_cid: cid.to_string(),
            private,
            service: service.to_string(),
            status,
            request_id,
            endpoint,
        })
    }

    fn require_pin_service(&self, service: &str) -> Result<crate::pinning::PinService> {
        self.pin_credentials()?
            .get(service)
            .cloned()
            .ok_or_else(|| {
                Error::Io(format!(
                    "no {service} credentials yet — add them in the Pin settings"
                ))
            })
    }

    /// Wait for this store's private (Sidekick) Kubo daemon to accept connections.
    pub(crate) fn wait_for_private_node(&self) -> Result<()> {
        let store = self.store_dir()?;
        for _ in 0..40 {
            if crate::node::private_node_running(&store) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        Err(Error::BackendDown(
            "private Kubo node did not come up in time".to_string(),
        ))
    }

    /// Append (or refresh) an entry in the on-device "kept hot" ledger
    /// (`<store>/hot-pins.json`) so the GUI can show what this node is serving and the
    /// source→ciphertext mapping survives restarts. Not secret (CIDs + a flag only).
    fn record_hot_pin(&self, source_cid: &str, pinned_cid: &str, private: bool) -> Result<()> {
        let path = self.store_dir()?.join("hot-pins.json");
        let mut entries: Vec<HotPin> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        entries.retain(|e| e.source_cid != source_cid);
        entries.push(HotPin {
            source_cid: source_cid.to_string(),
            pinned_cid: pinned_cid.to_string(),
            private,
        });
        let bytes = serde_json::to_vec_pretty(&entries)
            .map_err(|e| Error::Io(format!("serialize hot-pins: {e}")))?;
        std::fs::write(&path, &bytes).map_err(|e| Error::Io(format!("write hot-pins: {e}")))
    }

    /// The records this node is keeping hot on its private swarm.
    pub fn hot_pins(&self) -> Result<Vec<HotPin>> {
        let path = self.store_dir()?.join("hot-pins.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Stop keeping a record hot: unpin its (cipher)blocks from the private node so they
    /// can be GC'd and are no longer served, and drop it from the ledger. The original
    /// record (and any external pins) are untouched. Idempotent.
    pub fn unpin_hot(&self, source_cid: &str) -> Result<()> {
        let mut entries = self.hot_pins()?;
        let Some(entry) = entries.iter().find(|e| e.source_cid == source_cid).cloned() else {
            return Ok(()); // already not kept hot
        };
        let store = self.store_dir()?;
        if crate::node::private_node_running(&store) {
            let repo = crate::node::private_repo_for(&store);
            let _ = crate::node::ipfs_pin_rm(&repo, &entry.pinned_cid);
        }
        entries.retain(|e| e.source_cid != source_cid);
        let path = store.join("hot-pins.json");
        let bytes = serde_json::to_vec_pretty(&entries)
            .map_err(|e| Error::Io(format!("serialize hot-pins: {e}")))?;
        std::fs::write(&path, &bytes).map_err(|e| Error::Io(format!("write hot-pins: {e}")))
    }
}

/// One record kept hot on the private node, for the on-device ledger + the GUI list.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HotPin {
    pub source_cid: String,
    pub pinned_cid: String,
    pub private: bool,
}

/// Receipt for a record pin. `cid` is what the service now holds — the ciphertext root
/// for a private pin, or the record's own CID for a public one.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecordPinReceipt {
    pub cid: String,
    pub source_cid: String,
    pub private: bool,
    pub service: String,
    pub status: String,
    pub request_id: String,
    pub endpoint: String,
}
