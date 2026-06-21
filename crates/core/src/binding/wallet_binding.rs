use super::*;

impl MemCli {
    /// Sync the user's wallet-browser (Brave/Opera) bookmarks into memory (Pillar A,
    /// Decision 0033). Each new bookmark (deduped by URL via a bound `bookmark:<hash>`
    /// name) becomes a `memory`/`reference` node filed into the day calendar so it
    /// shows in Records. Read-only on the browser's side; ingested content is an
    /// **untrusted source** — retrievable, never auto-injected. Returns count added.
    /// Sync browser bookmarks, returning the **new** records appended as
    /// `(cid, bind_name, preview)`. Returning the appended nodes (not just a count)
    /// lets the UI insert exactly those rows — leveraging IPLD's append-only nature
    /// instead of re-deriving the whole view for one write.
    pub fn sync_browser_bookmarks(&self) -> Result<Vec<(Cid, String, String)>> {
        let mut added = Vec::new();
        for bm in crate::browser::read_bookmarks() {
            let key = crate::browser::url_key(&bm.url);
            let dedup_name = format!("bookmark:{key}");
            if self.resolve(&dedup_name).is_ok() {
                continue; // already ingested
            }
            let ts = if bm.added_unix > 0 {
                bm.added_unix
            } else {
                now_secs()
            };
            let location = if bm.folder.is_empty() {
                String::new()
            } else {
                format!("\n(in {})", bm.folder)
            };
            let text = format!("Bookmark — {}\n{}{}", bm.title, bm.url, location);
            let preview = text.clone();
            let node = Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({ "kind": "reference", "text": text }).to_string(),
            };
            let cid = self.put_node(&node)?;
            self.bind(&dedup_name, &cid)?;
            let _ = self.record_event_in_day(&utc_date(ts), &format!("bookmark-{key}"), &cid);
            added.push((cid, dedup_name, preview));
        }
        Ok(added)
    }

    // ── Wallet attestation + settings (Pillar C, Decision 0033) ──────────────
    // The browser (Brave/Opera) custodies keys and signs; we only verify + store.

    fn wallet_state_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("wallet.json"))
    }

    /// Load the on-device wallet state (links + settings), empty if none yet.
    pub fn wallet_state(&self) -> Result<crate::wallet::WalletState> {
        match std::fs::read(self.wallet_state_path()?) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Io(format!("parse wallet state: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::wallet::WalletState::default())
            }
            Err(e) => Err(Error::Io(format!("read wallet state: {e}"))),
        }
    }

    fn save_wallet_state(&self, state: &crate::wallet::WalletState) -> Result<()> {
        let dir = self.store_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(format!("create store dir: {e}")))?;
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| Error::Io(format!("serialize wallet state: {e}")))?;
        std::fs::write(self.wallet_state_path()?, bytes)
            .map_err(|e| Error::Io(format!("write wallet state: {e}")))
    }

    /// Verify a browser-wallet `personal_sign` over our AgentID and record the link
    /// (idempotent per address). Returns the stored link. We hold no keys — this only
    /// confirms the address controls a key that signed our AgentID.
    pub fn link_wallet(
        &self,
        address: &str,
        chain: &str,
        signature: &str,
    ) -> Result<crate::wallet::WalletLink> {
        let agent_id = self.identity()?.agent_id().0;
        let message = crate::wallet::link_message(&agent_id);
        let recovered =
            crate::wallet::recover_eth_personal_sign(&message, signature).map_err(Error::Io)?;
        if recovered.to_lowercase() != address.trim().to_lowercase() {
            return Err(Error::Io(format!(
                "signature does not match {address} (recovered {recovered})"
            )));
        }
        let link = crate::wallet::WalletLink {
            address: recovered.to_lowercase(),
            chain: if chain.is_empty() {
                "evm".to_string()
            } else {
                chain.to_string()
            },
            agent_id,
            signature: signature.to_string(),
            linked_at: now_secs(),
        };
        let mut state = self.wallet_state()?;
        state.links.retain(|l| l.address != link.address);
        state.links.push(link.clone());
        self.save_wallet_state(&state)?;
        Ok(link)
    }

    pub fn wallet_links(&self) -> Result<Vec<crate::wallet::WalletLink>> {
        Ok(self.wallet_state()?.links)
    }

    pub fn unlink_wallet(&self, address: &str) -> Result<()> {
        let mut state = self.wallet_state()?;
        let want = address.trim().to_lowercase();
        state.links.retain(|l| l.address != want);
        self.save_wallet_state(&state)
    }

    pub fn wallet_settings(&self) -> Result<crate::wallet::WalletSettings> {
        Ok(self.wallet_state()?.settings)
    }

    /// Replace the wallet settings from a JSON object (agent_access / spend_cap /
    /// allowlist / preferred_chain).
    pub fn set_wallet_settings(&self, settings_json: &str) -> Result<()> {
        let settings: crate::wallet::WalletSettings = serde_json::from_str(settings_json)
            .map_err(|e| Error::Io(format!("parse wallet settings: {e}")))?;
        let mut state = self.wallet_state()?;
        state.settings = settings;
        self.save_wallet_state(&state)
    }

    /// Stage a transaction the host AI *proposes* (the agent-propose tier). We never
    /// send it — the GUI surfaces it and the user approves it in their browser wallet,
    /// which confirms again. All guards are enforced HERE, before staging:
    /// `agent_access` must be on, the spend cap must cover the value, and (if set) the
    /// recipient must be allowlisted.
    pub fn propose_wallet_tx(
        &self,
        to: &str,
        value: &str,
        data: &str,
        reason: &str,
    ) -> Result<crate::wallet::WalletProposal> {
        let s = self.wallet_settings()?;
        if !s.agent_access {
            return Err(Error::Io(
                "AI wallet access is off — enable it in the Wallet tab to let the AI propose transactions".to_string(),
            ));
        }
        let to_l = to.trim().to_lowercase();
        if !to_l.starts_with("0x") || to_l.len() != 42 {
            return Err(Error::Io(format!("invalid recipient address: {to}")));
        }
        if !s.allowlist.is_empty() && !s.allowlist.iter().any(|a| a.trim().to_lowercase() == to_l) {
            return Err(Error::Io(format!(
                "recipient {to} is not in your allowlist"
            )));
        }
        let cap: f64 = s.spend_cap.trim().parse().map_err(|_| {
            Error::Io("no per-transaction spend cap set — AI sends are disabled".to_string())
        })?;
        let amount: f64 = value
            .trim()
            .parse()
            .map_err(|_| Error::Io(format!("invalid value: {value}")))?;
        if amount <= 0.0 || amount > cap {
            return Err(Error::Io(format!(
                "value {value} exceeds your per-transaction cap of {}",
                s.spend_cap
            )));
        }
        let proposed_at = now_secs();
        let id = format!(
            "tx-{}",
            &crate::browser::url_key(&format!("{to_l}{value}{data}{proposed_at}"))[..12]
        );
        let proposal = crate::wallet::WalletProposal {
            id,
            to: to_l,
            value: value.trim().to_string(),
            data: data.trim().to_string(),
            reason: reason.trim().to_string(),
            proposed_at,
            status: "pending".to_string(),
            tx_hash: String::new(),
        };
        let mut state = self.wallet_state()?;
        state.proposals.push(proposal.clone());
        // Bound the history.
        if state.proposals.len() > 50 {
            let drop = state.proposals.len() - 50;
            state.proposals.drain(0..drop);
        }
        self.save_wallet_state(&state)?;
        Ok(proposal)
    }

    /// Pending (not-yet-approved/rejected) proposals, newest first.
    pub fn pending_wallet_proposals(&self) -> Result<Vec<crate::wallet::WalletProposal>> {
        let mut out: Vec<_> = self
            .wallet_state()?
            .proposals
            .into_iter()
            .filter(|p| p.status == "pending")
            .collect();
        out.reverse();
        Ok(out)
    }

    /// Record the user's decision on a proposal (`approved` with a tx hash, or
    /// `rejected`).
    pub fn resolve_wallet_proposal(&self, id: &str, status: &str, tx_hash: &str) -> Result<()> {
        let mut state = self.wallet_state()?;
        let p = state
            .proposals
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::Io(format!("no such proposal: {id}")))?;
        p.status = status.to_string();
        p.tx_hash = tx_hash.to_string();
        self.save_wallet_state(&state)
    }
}
