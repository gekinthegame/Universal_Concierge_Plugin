use super::*;

impl MemCli {
    /// Configure a backend (writes `[publishing].backend` to the local config).
    pub fn add_backend(&self, name: &str) -> Result<()> {
        if !backend_exists(name) {
            return Err(Error::BackendDown(format!(
                "backend `{name}` is not compiled in"
            )));
        }
        let mut cfg = self.config()?;
        cfg.publishing.backend = name.to_string();
        cfg.save_to_project_root(&self.working_dir)
            .map_err(Error::Io)
    }

    /// Legacy ambiguous `share` never publishes. Phase A requires callers to use
    /// an explicit reviewed `publish-public` operation.
    pub fn share(&self, target: &str) -> Result<PublishReceipt> {
        let _ = target;
        Err(Error::ExplicitPublicPublishRequired)
    }

    /// Execute one explicitly reviewed public publication.
    pub fn publish_public(&self, reviewed: &crate::egress::EgressPlan) -> Result<PublishReceipt> {
        if reviewed.operation != crate::egress::EgressOperation::PublicPublish {
            return Err(Error::EgressPlanChanged(
                "reviewed plan is not a public publication".to_string(),
            ));
        }
        self.execute_approved_egress(reviewed, |approved| {
            let cfg = self.config()?;
            let mut receipt = share_via_selected_backend(self, approved, &cfg)?;
            // Sign the shared root with the AgentID: authenticity (*who* shared it) on
            // top of the CID's integrity (*what* was shared). Phase 5.5 / Decision 0007.
            let identity = self.identity()?;
            receipt.agent_id = identity.agent_id().0;
            receipt.signature = identity.sign(approved.root.0.as_bytes());
            self.append_receipt(&receipt)?;
            self.record_latest_share(&receipt, &approved.root)?;
            Ok(receipt)
        })
    }

    /// Where the published-site registry lives.
    fn sites_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("sites.json"))
    }

    /// Wait (briefly) for the public publishing node's API to come up.
    fn wait_for_public_node(&self) -> Result<()> {
        for _ in 0..40 {
            if crate::node::public_node_running() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        Err(Error::BackendDown(
            "public IPFS node did not come up in time".to_string(),
        ))
    }

    fn site_deploy_destination(&self, name: &str, platform: &str) -> Result<String> {
        let credentials = self.deploy_credentials()?;
        match platform {
            "ipfs" => Ok(format!(
                "ipfs-public:{}",
                crate::node::public_repo_for(&self.store_dir()?).display()
            )),
            "github" => credentials
                .github
                .map(|c| {
                    format!(
                        "https://api.github.com/repos/{}/{}/branches/{}",
                        c.owner, c.repo, c.branch
                    )
                })
                .ok_or_else(|| Error::Io("no github credentials yet".to_string())),
            "netlify" => credentials
                .netlify
                .map(|c| {
                    format!(
                        "https://api.netlify.com/site/{}",
                        c.site_id.unwrap_or_else(|| format!("new:{name}"))
                    )
                })
                .ok_or_else(|| Error::Io("no netlify credentials yet".to_string())),
            "vercel" => credentials
                .vercel
                .map(|c| {
                    format!(
                        "https://api.vercel.com/project/{}/team/{}",
                        c.project.unwrap_or_else(|| name.to_string()),
                        c.team_id.unwrap_or_else(|| "default".to_string())
                    )
                })
                .ok_or_else(|| Error::Io("no vercel credentials yet".to_string())),
            "cloudflare" => credentials
                .cloudflare
                .map(|c| {
                    format!(
                        "https://api.cloudflare.com/client/v4/accounts/{}/pages/projects/{}",
                        c.account_id, c.project
                    )
                })
                .ok_or_else(|| Error::Io("no cloudflare credentials yet".to_string())),
            "ftp" => Err(Error::SecurityPolicy(
                "plaintext FTP deployment is disabled".to_string(),
            )),
            other => Err(Error::Io(format!("unsupported platform: {other}"))),
        }
    }

    fn build_site_deploy_plan(
        &self,
        name: &str,
        folder: &str,
        kind: &str,
        platform: &str,
    ) -> Result<crate::deploy::SiteDeployPlan> {
        let folder_path = std::path::Path::new(folder);
        if !folder_path.is_dir() {
            return Err(Error::Io(format!("not a folder: {folder}")));
        }
        let files = crate::deploy::walk_files(folder_path).map_err(Error::Io)?;
        crate::deploy::SiteDeployPlan::from_files(
            name,
            folder_path,
            kind,
            platform,
            &self.site_deploy_destination(name, platform)?,
            &files,
        )
        .map_err(Error::Io)
    }

    /// Build the exact website deployment plan that the user must review before
    /// entering a password. Generated gallery/player front-ends are staged before
    /// the manifest is calculated so they are included in the review.
    pub fn review_site_deploy(
        &self,
        name: &str,
        folder: &str,
        kind: &str,
        platform: &str,
    ) -> Result<crate::deploy::SiteDeployPlan> {
        let folder_path = std::path::Path::new(folder);
        if !folder_path.is_dir() {
            return Err(Error::Io(format!("not a folder: {folder}")));
        }
        crate::site::write_index(folder_path, crate::site::SiteKind::parse(kind), name)?;
        self.build_site_deploy_plan(name, folder, kind, platform)
    }

    /// Publish exactly one previously reviewed website manifest. The folder and
    /// destination are recomputed and compared while the security policy lock is
    /// held, immediately before egress.
    pub fn publish_site(
        &self,
        reviewed: &crate::deploy::SiteDeployPlan,
        password: &str,
    ) -> Result<PublishReceipt> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let current = self.build_site_deploy_plan(
            &reviewed.name,
            &reviewed.folder,
            &reviewed.kind,
            &reviewed.platform,
        )?;
        if current != *reviewed {
            return Err(Error::EgressPlanChanged(
                "website files, destination, or deployment metadata changed after review"
                    .to_string(),
            ));
        }
        let event_root = Cid(format!("external-manifest:{}", reviewed.manifest_digest));
        self.append_security_event_unlocked(
            "site_deploy_approved",
            &event_root,
            &format!("{} via {}", reviewed.name, reviewed.destination),
        )?;
        let folder_path = std::path::Path::new(&reviewed.folder);
        let files = crate::deploy::walk_files(folder_path).map_err(Error::Io)?;

        match reviewed.platform.as_str() {
            "ipfs" => {
                let store = self.store_dir()?;
                let repo = crate::node::public_repo_for(&store);
                crate::node::launch_public_node(&store)?;
                self.wait_for_public_node()?;
                let ipns = crate::node::ipns_key_gen(&repo, &reviewed.name)?;
                let cid = crate::node::unixfs_add_dir(&repo, folder_path)?;
                let published = crate::node::ipns_publish(&repo, &cid, &reviewed.name)?;
                let identity = self.identity()?;
                let receipt = PublishReceipt {
                    root: cid.clone(),
                    backend: "ipfs-public".to_string(),
                    unix_time: now_secs(),
                    gateway_url: format!("https://ipfs.io/ipns/{published}"),
                    agent_id: identity.agent_id().0,
                    signature: identity.sign(cid.as_bytes()),
                    ipns_name: Some(published.clone()),
                    site_name: Some(reviewed.name.clone()),
                };
                self.append_receipt(&receipt)?;
                self.record_publication(&receipt)?;
                // Reuse the existing IPNS address if the site was published before.
                let path = self.sites_path()?;
                crate::state::update_json::<Sites, _>(&path, |sites| {
                    let ipns = sites
                        .sites
                        .get(&reviewed.name)
                        .map(|site| site.ipns.clone())
                        .unwrap_or(ipns);
                    sites.sites.insert(
                        reviewed.name.clone(),
                        SiteRecord {
                            name: reviewed.name.clone(),
                            ipns,
                            dir: reviewed.folder.clone(),
                            last_cid: Some(cid),
                            published_at: now_secs() as i64,
                        },
                    );
                    Ok(())
                })?;
                Ok(receipt)
            }
            "github" | "netlify" | "vercel" | "cloudflare" => {
                self.publish_external(reviewed, &files)
            }
            "ftp" => Err(Error::SecurityPolicy(
                "plaintext FTP deployment is disabled".to_string(),
            )),
            _ => Err(Error::Io(format!(
                "unsupported platform: {}",
                reviewed.platform
            ))),
        }
    }

    /// Path to the on-device deploy-credentials vault (`<store>/security/deploy.json`,
    /// 0600). Tokens live here and never go anywhere but their own platform's API.
    pub(super) fn deploy_credentials_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("deploy.json"))
    }

    /// Load the stored deploy credentials (empty if none configured yet).
    pub fn deploy_credentials(&self) -> Result<crate::deploy::DeployCredentials> {
        let path = self.deploy_credentials_path()?;
        if path
            .try_exists()
            .map_err(|error| Error::Io(format!("inspect deploy credentials: {error}")))?
        {
            crate::egress::validate_private_file(&path)?;
        }
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Io(format!("parse deploy credentials: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::deploy::DeployCredentials::default())
            }
            Err(e) => Err(Error::Io(format!("read deploy credentials: {e}"))),
        }
    }

    /// Set (merge) the credentials for one platform from a JSON object, written
    /// 0600. `fields_json` is the platform's credential block (e.g. the GitHub
    /// `{token,owner,repo,branch?}`); an explicit JSON `null` clears it.
    pub fn set_deploy_credentials(&self, platform: &str, fields_json: &str) -> Result<()> {
        let mut creds = self.deploy_credentials()?;
        let value: serde_json::Value = serde_json::from_str(fields_json)
            .map_err(|e| Error::Io(format!("parse credential fields: {e}")))?;
        let cleared = value.is_null();
        macro_rules! merge {
            ($field:ident) => {{
                if cleared {
                    creds.$field = None;
                } else {
                    creds.$field =
                        Some(serde_json::from_value(value.clone()).map_err(|e| {
                            Error::Io(format!("invalid {platform} credentials: {e}"))
                        })?);
                }
            }};
        }
        match platform {
            "github" => merge!(github),
            "netlify" => merge!(netlify),
            "vercel" => merge!(vercel),
            "cloudflare" => merge!(cloudflare),
            "ftp" => {
                return Err(Error::SecurityPolicy(
                    "plaintext FTP credentials are not accepted".to_string(),
                ))
            }
            other => return Err(Error::Io(format!("unknown deploy platform: {other}"))),
        }
        let required = |label: &str, value: &str| -> Result<()> {
            if value.trim().is_empty() || value.chars().any(char::is_control) {
                Err(Error::SecurityPolicy(format!(
                    "{label} must be non-empty and contain no control characters"
                )))
            } else {
                Ok(())
            }
        };
        match platform {
            "github" if !cleared => {
                let c = creds
                    .github
                    .as_ref()
                    .expect("github credentials were just parsed");
                required("github token", &c.token)?;
                required("github owner", &c.owner)?;
                required("github repository", &c.repo)?;
                required("github branch", &c.branch)?;
                if c.owner.contains('/') || c.repo.contains('/') || c.branch.contains("..") {
                    return Err(Error::SecurityPolicy(
                        "github owner, repository, or branch contains an unsafe path component"
                            .to_string(),
                    ));
                }
            }
            "netlify" if !cleared => {
                required(
                    "netlify token",
                    &creds
                        .netlify
                        .as_ref()
                        .expect("netlify credentials were just parsed")
                        .token,
                )?;
            }
            "vercel" if !cleared => {
                required(
                    "vercel token",
                    &creds
                        .vercel
                        .as_ref()
                        .expect("vercel credentials were just parsed")
                        .token,
                )?;
            }
            "cloudflare" if !cleared => {
                let c = creds
                    .cloudflare
                    .as_ref()
                    .expect("cloudflare credentials were just parsed");
                required("cloudflare token", &c.token)?;
                required("cloudflare account id", &c.account_id)?;
                required("cloudflare project", &c.project)?;
                if c.account_id.contains('/') || c.project.contains('/') {
                    return Err(Error::SecurityPolicy(
                        "cloudflare account or project contains an unsafe path component"
                            .to_string(),
                    ));
                }
            }
            _ => {}
        }
        self.ensure_security_dir()?;
        let bytes = serde_json::to_vec_pretty(&creds)
            .map_err(|e| Error::Io(format!("serialize deploy credentials: {e}")))?;
        crate::egress::atomic_private_write(&self.deploy_credentials_path()?, &bytes)
    }

    /// Non-secret status: which platforms are configured + their public fields
    /// (owner/repo/project/host). Tokens/passwords are NEVER returned to the GUI.
    pub fn deploy_status(&self) -> Result<serde_json::Value> {
        let c = self.deploy_credentials()?;
        Ok(serde_json::json!({
            "github": c.github.as_ref().map(|g| serde_json::json!({
                "owner": g.owner, "repo": g.repo, "branch": g.branch })),
            "netlify": c.netlify.as_ref().map(|n| serde_json::json!({
                "site_id": n.site_id })),
            "vercel": c.vercel.as_ref().map(|v| serde_json::json!({
                "project": v.project, "team_id": v.team_id })),
            "cloudflare": c.cloudflare.as_ref().map(|c| serde_json::json!({
                "account_id": c.account_id, "project": c.project })),
        }))
    }

    /// Verify a platform's credentials live against its API (the "Test connection"
    /// step of the connect walk-through). `fields_json` lets the GUI test *unsaved*
    /// input (a single platform's `{token,…}` block) before saving; when `None` the
    /// stored credentials are tested. Returns a short account label on success.
    pub fn verify_deploy_credentials(
        &self,
        platform: &str,
        fields_json: Option<&str>,
    ) -> Result<String> {
        let creds = match fields_json {
            Some(json) if !json.trim().is_empty() && json.trim() != "null" => {
                let value: serde_json::Value = serde_json::from_str(json)
                    .map_err(|e| Error::Io(format!("parse credential fields: {e}")))?;
                let mut c = crate::deploy::DeployCredentials::default();
                macro_rules! set {
                    ($field:ident) => {
                        c.$field = Some(serde_json::from_value(value.clone()).map_err(|e| {
                            Error::Io(format!("invalid {platform} credentials: {e}"))
                        })?)
                    };
                }
                match platform {
                    "github" => set!(github),
                    "netlify" => set!(netlify),
                    "vercel" => set!(vercel),
                    "cloudflare" => set!(cloudflare),
                    other => return Err(Error::Io(format!("unknown deploy platform: {other}"))),
                }
                c
            }
            _ => self.deploy_credentials()?,
        };
        crate::deploy::verify(platform, &creds).map_err(Error::Io)
    }

    /// Deploy the staged folder to an external Web2 host using the stored
    /// credentials. Password is already verified upstream (`publish_site`); this is
    /// explicit, gated egress. Returns a real receipt with the live URL.
    fn publish_external(
        &self,
        reviewed: &crate::deploy::SiteDeployPlan,
        files: &[crate::deploy::DeployFile],
    ) -> Result<PublishReceipt> {
        let identity = self.identity()?;
        let creds = self.deploy_credentials()?;
        let platform = reviewed.platform.as_str();

        let missing = || {
            Error::Io(format!(
                "no {platform} credentials yet — add them in Studio → Deploy settings"
            ))
        };
        let url = match platform {
            "github" => crate::deploy::deploy_github(&creds.github.ok_or_else(missing)?, files),
            "netlify" => crate::deploy::deploy_netlify(
                &creds.netlify.ok_or_else(missing)?,
                files,
                &reviewed.name,
            ),
            "vercel" => crate::deploy::deploy_vercel(
                &creds.vercel.ok_or_else(missing)?,
                files,
                &reviewed.name,
            ),
            "cloudflare" => {
                crate::deploy::deploy_cloudflare(&creds.cloudflare.ok_or_else(missing)?, files)
            }
            other => return Err(Error::Io(format!("unsupported platform: {other}"))),
        }
        .map_err(Error::Io)?;

        let signed = format!(
            "{}\n{}\n{}\n{}",
            reviewed.manifest_digest, reviewed.destination, platform, url
        );
        let receipt = PublishReceipt {
            root: format!("external-manifest:{}", reviewed.manifest_digest),
            backend: platform.to_string(),
            unix_time: now_secs(),
            gateway_url: url.clone(),
            agent_id: identity.agent_id().0,
            signature: identity.sign(signed.as_bytes()),
            ipns_name: None,
            site_name: Some(reviewed.name.clone()),
        };
        self.append_receipt(&receipt)?;
        self.record_publication(&receipt)?;
        Ok(receipt)
    }

    /// Verify that an external-site receipt authenticates this exact reviewed
    /// manifest, destination, platform, and returned live URL.
    pub fn verify_external_site_receipt(
        &self,
        receipt: &PublishReceipt,
        reviewed: &crate::deploy::SiteDeployPlan,
    ) -> Result<bool> {
        if receipt.root != format!("external-manifest:{}", reviewed.manifest_digest)
            || receipt.backend != reviewed.platform
            || receipt.site_name.as_deref() != Some(reviewed.name.as_str())
        {
            return Ok(false);
        }
        let signed = format!(
            "{}\n{}\n{}\n{}",
            reviewed.manifest_digest, reviewed.destination, reviewed.platform, receipt.gateway_url
        );
        crate::identity::verify(
            &AgentId(receipt.agent_id.clone()),
            signed.as_bytes(),
            &receipt.signature,
        )
        .map_err(Error::Io)
    }

    /// The published sites this install knows.
    pub fn site_list(&self) -> Result<Vec<SiteRecord>> {
        Ok(Sites::load(&self.sites_path()?)
            .map_err(Error::Io)?
            .sites
            .into_values()
            .collect())
    }

    /// Forget a site from the registry (does not unpin or revoke the IPNS key).
    pub fn site_unpublish(&self, name: &str) -> Result<()> {
        let path = self.sites_path()?;
        crate::state::update_json::<Sites, _>(&path, |sites| {
            sites.sites.remove(name);
            Ok(())
        })
    }

    /// Export a site's IPNS private key to `out_path` for backup/portability.
    pub fn export_site_key(&self, name: &str, out_path: &std::path::Path) -> Result<()> {
        let repo = crate::node::public_repo_for(&self.store_dir()?);
        crate::node::ipns_key_export(&repo, name, out_path)
    }

    /// Read the local publish-receipt trail. The visual explorer uses this as
    /// the read-only source of truth for whether a root has a recorded pin.
    pub fn publish_receipts(&self) -> Result<Vec<PublishReceipt>> {
        let path = self
            .working_dir
            .join(self.config()?.store.root.join("publish-receipts.jsonl"));
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Io(format!("read receipt trail: {e}")))?;
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|e| Error::Io(format!("parse publish receipt: {e}")))
            })
            .collect()
    }
}
