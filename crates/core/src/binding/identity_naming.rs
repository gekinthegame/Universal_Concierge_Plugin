use super::*;

impl MemCli {
    /// Load (or first-time generate + persist) this install's AgentID identity.
    pub fn identity(&self) -> Result<Identity> {
        let key_path = self.working_dir.join(self.config()?.identity.key_path);
        Identity::load_or_create(&key_path).map_err(|e| Error::Io(format!("identity: {e}")))
    }

    /// This install's public AgentID — stable across restarts.
    pub fn agent_id(&self) -> Result<AgentId> {
        Ok(self.identity()?.agent_id())
    }

    /// Verify a signed share: does `signature` over `root` come from `agent_id`?
    pub fn verify_share(&self, root: &Cid, agent_id: &str, signature: &str) -> Result<bool> {
        crate::identity::verify(&AgentId(agent_id.to_string()), root.0.as_bytes(), signature)
            .map_err(Error::Io)
    }

    /// Verified shares from followed AgentIDs, ready to display or fetch.
    pub fn shared_with_me(&self) -> Result<Vec<SharedWithMeEntry>> {
        let book = self.social_book()?;
        let mut out = Vec::new();
        for agent_id in &book.following {
            let nickname = book.nickname_of(agent_id).cloned();
            let name = shared_with_me_name(agent_id);
            let cid = match self.resolve(&name) {
                Ok(cid) => cid,
                Err(Error::NameUnbound(_)) => continue,
                Err(e) => return Err(e),
            };
            let record = self.get(&CidOrName::Cid(cid.clone()))?;
            let Record::Live { body_json, .. } = record else {
                continue;
            };
            let (pointer_agent_id, root, signature) = parse_share_pointer(&body_json)?;
            if pointer_agent_id != *agent_id {
                continue;
            }
            let verified = self.verify_share(&root, agent_id, &signature)?;
            if verified {
                out.push(SharedWithMeEntry {
                    agent_id: agent_id.clone(),
                    nickname,
                    root,
                    signature,
                    pointer_cid: cid,
                });
            }
        }
        Ok(out)
    }

    fn social_path(&self) -> Result<PathBuf> {
        Ok(self
            .working_dir
            .join(self.config()?.store.root.join("social.json")))
    }

    /// The local petname + follow book.
    pub fn social_book(&self) -> Result<SocialBook> {
        SocialBook::load(&self.social_path()?).map_err(Error::Io)
    }

    /// Follow an AgentID (persisted to the local book; also the inbound allowlist).
    pub fn follow(&self, agent_id: &str) -> Result<()> {
        let path = self.social_path()?;
        crate::state::update_json::<SocialBook, _>(&path, |book| {
            book.follow(agent_id);
            Ok(())
        })
    }

    /// Give an AgentID a local petname.
    pub fn set_nickname(&self, agent_id: &str, nickname: &str) -> Result<()> {
        let path = self.social_path()?;
        crate::state::update_json::<SocialBook, _>(&path, |book| {
            book.set_nickname(agent_id, nickname);
            Ok(())
        })
    }

    /// Remove a petname.
    pub fn remove_nickname(&self, agent_id: &str) -> Result<()> {
        let path = self.social_path()?;
        crate::state::update_json::<SocialBook, _>(&path, |book| {
            book.remove_nickname(agent_id);
            Ok(())
        })
    }

    // ── Sovereign naming: Layer 2 contact cards + resolution + introductions ──

    fn own_card_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("contact-card.json"))
    }
    fn cards_dir(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("cards"))
    }
    fn introductions_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("introductions.json"))
    }

    /// Edit the local user's own contact card (the self-asserted profile fields).
    /// Stored unsigned; [`MemCli::my_card`] signs a fresh copy on demand. `None`
    /// fields are left unchanged; an empty string clears an optional field.
    pub fn update_my_card(
        &self,
        display_name: Option<&str>,
        bio: Option<&str>,
        avatar: Option<&str>,
        site_ipns: Option<&str>,
    ) -> Result<()> {
        let path = self.own_card_path()?;
        crate::state::update_json::<ContactCard, _>(&path, |card| {
            if let Some(n) = display_name {
                card.display_name = n.to_string();
            }
            let opt = |v: &str| (!v.trim().is_empty()).then(|| v.trim().to_string());
            if let Some(b) = bio {
                card.bio = opt(b);
            }
            if let Some(a) = avatar {
                card.avatar = opt(a);
            }
            if let Some(s) = site_ipns {
                card.site_ipns = opt(s);
            }
            card.sig = String::new();
            validate_contact_card_limits(card)
        })
    }

    /// Build and **sign** the user's current contact card (refreshing `updated_at`).
    pub fn my_card(&self) -> Result<ContactCard> {
        let identity = self.identity()?;
        let aid = identity.agent_id();
        let did = naming::did_key_from_agent(&aid).map_err(Error::Io)?;
        let mut card: ContactCard = std::fs::read_to_string(self.own_card_path()?)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        card.did = did;
        if card.display_name.trim().is_empty() {
            card.display_name = naming::short_agent(&aid.0);
        }
        card.updated_at = now_secs();
        card.sign(&identity);
        validate_contact_card_limits(&card)?;
        Ok(card)
    }

    /// Import a peer's signed card: verify the signature, then cache it. Only the
    /// self-authenticating part is trusted; the name stays a hint until petnamed.
    /// Returns the author's AgentID hex.
    pub fn import_card(&self, card_json: &str) -> Result<String> {
        let card: ContactCard = serde_json::from_str(card_json)
            .map_err(|e| Error::Io(format!("parse contact card: {e}")))?;
        if !card.verify() {
            return Err(Error::Io(
                "contact card signature does not verify".to_string(),
            ));
        }
        validate_contact_card_limits(&card)?;
        let aid = card.agent_id().map_err(Error::Io)?;
        let dir = self.cards_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io(e.to_string()))?;
        let path = dir.join(format!("{}.json", aid.0));
        if let Some(existing) = self.card_of(&aid.0)? {
            if card.updated_at <= existing.updated_at {
                return Err(Error::SecurityPolicy(
                    "stale or duplicate contact card update rejected".to_string(),
                ));
            }
        }
        crate::state::save_json(&path, &card)?;
        Ok(aid.0)
    }

    /// The cached, verified card for an AgentID, if any.
    pub fn card_of(&self, agent_id: &str) -> Result<Option<ContactCard>> {
        let path = self.cards_dir()?.join(format!("{agent_id}.json"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        let card: ContactCard = match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        // Re-verify on read — a cached file is only trusted if it still verifies.
        Ok(card.verify().then_some(card))
    }

    fn load_introductions(&self) -> Vec<Introduction> {
        std::fs::read_to_string(self.introductions_path().unwrap_or_default())
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<Introduction>>(&s).ok())
            .unwrap_or_default()
    }

    /// Resolve an AgentID to a display name with provenance: petname (pinned) >
    /// introduction (from someone you follow) > self-asserted card (hint) > short id.
    pub fn resolve_display(&self, agent_id: &str) -> ResolvedName {
        if let Ok(book) = self.social_book() {
            if let Some(petname) = book.nickname_of(agent_id) {
                return ResolvedName::new(petname.clone(), NameSource::Petname);
            }
            // An introduction from someone you follow is a strong hint.
            let intros = self.load_introductions();
            let from_followed = intros.iter().find(|i| {
                i.verify()
                    && i.subject_agent().ok().map(|a| a.0).as_deref() == Some(agent_id)
                    && naming::agent_id_from_did(&i.from)
                        .map(|a| book.is_following(&a.0))
                        .unwrap_or(false)
            });
            if let Some(i) = from_followed {
                return ResolvedName::new(i.asserted_name.clone(), NameSource::Introduced);
            }
        }
        if let Ok(Some(card)) = self.card_of(agent_id) {
            if !card.display_name.trim().is_empty() {
                return ResolvedName::new(card.display_name, NameSource::Card);
            }
        }
        ResolvedName::new(naming::short_agent(agent_id), NameSource::Unknown)
    }

    /// Reverse lookup: a query like `alice` → every AgentID it could mean, across
    /// petnames, introductions, and cached cards. Returns the full candidate set so
    /// the UI can disambiguate (it never silently picks one).
    pub fn resolve_name(&self, query: &str) -> Vec<(String, ResolvedName)> {
        let q = query.trim().trim_start_matches('@').to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<(String, ResolvedName)> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let push = |aid: String,
                    name: ResolvedName,
                    out: &mut Vec<_>,
                    seen: &mut std::collections::BTreeSet<String>| {
            if seen.insert(format!("{aid}:{:?}", name.source)) {
                out.push((aid, name));
            }
        };
        if let Ok(book) = self.social_book() {
            for (aid, petname) in &book.nicknames {
                if petname.to_lowercase().contains(&q) {
                    push(
                        aid.clone(),
                        ResolvedName::new(petname.clone(), NameSource::Petname),
                        &mut out,
                        &mut seen,
                    );
                }
            }
        }
        for intro in self.load_introductions() {
            if intro.verify() && intro.asserted_name.to_lowercase().contains(&q) {
                if let Ok(aid) = intro.subject_agent() {
                    push(
                        aid.0,
                        ResolvedName::new(intro.asserted_name, NameSource::Introduced),
                        &mut out,
                        &mut seen,
                    );
                }
            }
        }
        if let Ok(dir) = self.cards_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if let Ok(text) = std::fs::read_to_string(entry.path()) {
                        if let Ok(card) = serde_json::from_str::<ContactCard>(&text) {
                            if card.verify() && card.display_name.to_lowercase().contains(&q) {
                                if let Ok(aid) = card.agent_id() {
                                    push(
                                        aid.0,
                                        ResolvedName::new(card.display_name, NameSource::Card),
                                        &mut out,
                                        &mut seen,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Build a signed introduction vouching that `subject_agent` goes by `name`,
    /// to hand to a contact (it travels over the chat channel).
    pub fn make_introduction(&self, subject_agent: &str, name: &str) -> Result<Introduction> {
        let identity = self.identity()?;
        let from = naming::did_key_from_agent(&identity.agent_id()).map_err(Error::Io)?;
        let subject_did =
            naming::did_key_from_agent(&AgentId(subject_agent.to_string())).map_err(Error::Io)?;
        let card_ipns = self
            .card_of(subject_agent)
            .ok()
            .flatten()
            .and_then(|c| c.site_ipns);
        let mut intro = Introduction {
            from,
            subject_did,
            asserted_name: name.to_string(),
            card_ipns,
            updated_at: now_secs(),
            sig: String::new(),
        };
        intro.sign(&identity);
        Ok(intro)
    }

    /// Accept a received introduction: verify + store it as a name *candidate*
    /// (still petname-gated). Returns the subject AgentID.
    pub fn accept_introduction(&self, intro_json: &str) -> Result<String> {
        let intro: Introduction = serde_json::from_str(intro_json)
            .map_err(|e| Error::Io(format!("parse introduction: {e}")))?;
        if !intro.verify() {
            return Err(Error::Io(
                "introduction signature does not verify".to_string(),
            ));
        }
        validate_introduction_limits(&intro)?;
        let subject = intro.subject_agent().map_err(Error::Io)?.0;
        let path = self.introductions_path()?;
        crate::state::update_json::<Vec<Introduction>, _>(&path, |intros| {
            if intros.iter().any(|existing| {
                existing.from == intro.from
                    && existing.subject_did == intro.subject_did
                    && existing.updated_at >= intro.updated_at
            }) {
                return Err(Error::SecurityPolicy(
                    "stale or duplicate introduction update rejected".to_string(),
                ));
            }
            intros.retain(|existing| {
                !(existing.from == intro.from && existing.subject_did == intro.subject_did)
            });
            intros.insert(0, intro);
            intros.truncate(500);
            Ok(())
        })?;
        Ok(subject)
    }

    /// The DNSLink TXT record value for a site's IPNS — an optional Web2 bridge a
    /// domain owner pastes at their registrar (no chain, no dependency).
    pub fn dnslink_txt(&self, site_ipns: &str) -> String {
        format!("dnslink=/ipns/{}", site_ipns.trim())
    }
}
