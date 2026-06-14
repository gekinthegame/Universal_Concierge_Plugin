use super::*;

impl MemCli {
    fn room_book_path(&self) -> Result<PathBuf> {
        Ok(self
            .working_dir
            .join(self.config()?.store.root.join("rooms.json")))
    }

    /// The per-room participation policies (the AI-send lever + mutes).
    pub fn room_book(&self) -> Result<RoomBook> {
        RoomBook::load(&self.room_book_path()?).map_err(Error::Io)
    }

    /// Put a typed node whose **provenance is `from`** — recorded as a derived
    /// `Source`, so `walk`/`record_links` follow the links back to the sub-graph
    /// it was derived from (e.g. a §4 synthesis linking its source thread). Unlike
    /// `put_node` (which records `Source::User`), this attaches real, gravity-
    /// counted edges to the originating CIDs.
    pub fn put_node_derived(&self, node: &Node, from: &[Cid]) -> Result<Cid> {
        let mut value: serde_json::Value = serde_json::from_str(&node.fields_json)
            .map_err(|e| Error::Io(format!("node fields_json is not valid JSON: {e}")))?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| Error::Io("node fields_json must be a JSON object".to_string()))?;
        obj.insert(
            "type".to_string(),
            serde_json::Value::String(node.kind.clone()),
        );
        let typed: mem::node::Node = serde_json::from_value(value)
            .map_err(|e| Error::CidNotFound(format!("invalid node json: {e}")))?;
        let mut links = Vec::with_capacity(from.len());
        for cid in from {
            let parsed: mem::cid::Cid = cid
                .0
                .parse()
                .map_err(|e| Error::CidNotFound(format!("invalid cid {}: {e}", cid.0)))?;
            links.push(parsed);
        }
        let store = self.open_store()?;
        let cid = store
            .put_node(typed, mem::node::Source::Derived { from: links })
            .map_err(|e| Error::Io(format!("put derived node: {e}")))?;
        Ok(Cid(cid.to_string()))
    }

    /// Set a room's AI-send lever: `"off"` (Human-only), `"on"`, or `"on_mention"`.
    pub fn set_room_ai_send(&self, room: &str, value: &str) -> Result<()> {
        let path = self.room_book_path()?;
        crate::state::update_json::<RoomBook, _>(&path, |book| {
            book.set_ai_send(room, value);
            Ok(())
        })
    }

    /// Mute an AgentID in a room (receiver-side; muted messages stay in the DAG).
    pub fn mute_in_room(&self, room: &str, agent_id: &str) -> Result<()> {
        let path = self.room_book_path()?;
        crate::state::update_json::<RoomBook, _>(&path, |book| {
            book.mute(room, agent_id);
            Ok(())
        })
    }

    /// Post a signed message to a room, returning its CID. Enforces the AI-send
    /// lever (send-side): an `ai` install cannot post to a Human-only room.
    pub fn post_message(&self, room: &str, payload: &str) -> Result<Cid> {
        let cfg = self.config()?;
        let policy = self.room_book()?.policy(room);
        if !policy.may_send(&cfg.identity.kind, payload) {
            return Err(Error::Io(format!(
                "muted: room `{room}` is Human-only and this install is `{}`",
                cfg.identity.kind
            )));
        }
        let identity = self.identity()?;
        let parent = match self.resolve(&room_latest_name(room)) {
            Ok(cid) => Some(cid),
            Err(Error::NameUnbound(_)) => None,
            Err(e) => return Err(e),
        };
        // Link by the parent's *signature* (its install-independent message id),
        // not its block CID, so threads cohere across installs.
        let (clock, next) = match &parent {
            Some(p) => {
                let parent_env = self.read_message(p)?;
                (parent_env.clock + 1, vec![parent_env.sig])
            }
            None => (1, Vec::new()),
        };
        let mut env = MessageEnvelope {
            id: room.to_string(),
            payload: payload.to_string(),
            next,
            refs: Vec::new(),
            clock,
            key: identity.agent_id().0,
            sig: String::new(),
        };
        env.sig = identity.sign(&env.signing_bytes());
        let text = serde_json::to_string(&env).map_err(|e| Error::Io(e.to_string()))?;
        let cid = self.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: serde_json::json!({ "text": text, "kind": "reference" }).to_string(),
        })?;
        self.bind(&message_id_name(&env.sig), &cid)?;
        self.bind(&room_latest_name(room), &cid)?;
        Ok(cid)
    }

    /// Read a message by CID, **verifying its signature**: a forged or tampered
    /// message (author's key doesn't sign it) is rejected.
    pub fn read_message(&self, cid: &Cid) -> Result<MessageEnvelope> {
        let record = self.get(&CidOrName::Cid(cid.clone()))?;
        let Record::Live { body_json, .. } = record else {
            return Err(Error::Io("message is tombstoned".to_string()));
        };
        let env = parse_message_envelope(&body_json)?;
        let ok = crate::identity::verify(&AgentId(env.key.clone()), &env.signing_bytes(), &env.sig)
            .map_err(Error::Io)?;
        if !ok {
            return Err(Error::Io(format!(
                "message signature does not verify for author {}",
                env.key
            )));
        }
        Ok(env)
    }

    /// Accept an **inbound** signed message from a peer (gossipsub / relay): verify
    /// the author's signature, store it idempotently, and advance the room head if
    /// this message is at least as new as the current head. The room is the
    /// envelope's `id`. Returns the stored block CID (the existing one if we have
    /// already seen this message). The wire form is the bare envelope JSON — the
    /// same `text` `post_message` stores and the transport publishes.
    pub fn accept_message(&self, env_json: &str) -> Result<Cid> {
        let env: MessageEnvelope = serde_json::from_str(env_json)
            .map_err(|e| Error::Io(format!("parse inbound message: {e}")))?;
        let ok = crate::identity::verify(&AgentId(env.key.clone()), &env.signing_bytes(), &env.sig)
            .map_err(Error::Io)?;
        if !ok {
            return Err(Error::Io(format!(
                "inbound message signature does not verify for author {}",
                env.key
            )));
        }
        // Idempotent: a message is identified by its signature, so re-delivery
        // (gossipsub fan-out, reconnect replay) maps to the same stored node.
        let id_name = message_id_name(&env.sig);
        if let Ok(existing) = self.resolve(&id_name) {
            return Ok(existing);
        }
        let room = env.id.clone();
        let text = serde_json::to_string(&env).map_err(|e| Error::Io(e.to_string()))?;
        let cid = self.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: serde_json::json!({ "text": text, "kind": "reference" }).to_string(),
        })?;
        self.bind(&id_name, &cid)?;
        // Advance the room head if this message is at least as new as ours, so the
        // thread view (which walks back from the head) includes it.
        let advance = match self.resolve(&room_latest_name(&room)) {
            Ok(head) => self
                .read_message(&head)
                .map(|h| env.clock >= h.clock)
                .unwrap_or(true),
            Err(Error::NameUnbound(_)) => true,
            Err(e) => return Err(e),
        };
        if advance {
            self.bind(&room_latest_name(&room), &cid)?;
        }
        Ok(cid)
    }

    /// Where the direct-message consent allowlist + held requests live.
    fn contacts_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("contacts.json"))
    }

    /// Is `username` (a hex AgentID) an approved contact whose messages we accept?
    pub fn is_contact(&self, username: &str) -> bool {
        self.contacts_path()
            .ok()
            .and_then(|path| Contacts::load(&path).ok())
            .map(|contacts| contacts.approved.contains(username))
            .unwrap_or(false)
    }

    /// Approve `username` so their messages land in threads (idempotent). Called
    /// when *we* initiate a conversation (initiating implies trust).
    pub fn add_contact(&self, username: &str) -> Result<()> {
        let path = self.contacts_path()?;
        crate::state::update_json::<Contacts, _>(&path, |contacts| {
            contacts.approved.insert(username.to_string());
            Ok(())
        })
    }

    /// The approved contacts — usernames (hex AgentIDs) whose direct messages we
    /// accept into threads. Sorted (the underlying set is ordered).
    pub fn approved_contacts(&self) -> Result<Vec<String>> {
        let path = self.contacts_path()?;
        let contacts = Contacts::load(&path).map_err(Error::Io)?;
        Ok(contacts.approved.iter().cloned().collect())
    }

    /// Revoke approval for `username` so their future messages are held as requests
    /// again (the user can re-approve). Returns whether they were approved. Thread
    /// history already received is not touched.
    pub fn remove_contact(&self, username: &str) -> Result<bool> {
        let path = self.contacts_path()?;
        crate::state::update_json::<Contacts, _>(&path, |contacts| {
            Ok(contacts.approved.remove(username))
        })
    }

    /// The consent gate for **inbound** messages (the "only an approved concierge"
    /// rule). Verifies authorship, then: a message from us or an approved contact
    /// is accepted into its thread (`"accepted"`); a message from an unknown
    /// author is held as a request the user must accept/decline (`"pending"`) — a
    /// public username is never enough to land a message.
    pub fn receive_message(&self, env_json: &str) -> Result<&'static str> {
        let env: MessageEnvelope = serde_json::from_str(env_json)
            .map_err(|e| Error::Io(format!("parse inbound message: {e}")))?;
        let ok = crate::identity::verify(&AgentId(env.key.clone()), &env.signing_bytes(), &env.sig)
            .map_err(Error::Io)?;
        if !ok {
            return Err(Error::Io(format!(
                "inbound message signature does not verify for author {}",
                env.key
            )));
        }
        let me = self.identity()?.agent_id().0;
        if env.key == me || self.is_contact(&env.key) {
            self.accept_message(env_json)?;
            return Ok("accepted");
        }
        // Unknown sender: hold it as a request (de-duped by signature).
        let path = self.contacts_path()?;
        crate::state::update_json::<Contacts, _>(&path, |contacts| {
            let queue = contacts.requests.entry(env.key.clone()).or_default();
            if !queue.iter().any(|held| held.contains(&env.sig)) {
                queue.push(env_json.to_string());
            }
            Ok(())
        })?;
        Ok("pending")
    }

    /// Pending message requests: `(sender username, held count, latest preview)`.
    pub fn message_requests(&self) -> Result<Vec<(String, usize, String)>> {
        let path = self.contacts_path()?;
        let contacts = Contacts::load(&path).map_err(Error::Io)?;
        let mut out = Vec::new();
        for (username, queue) in &contacts.requests {
            let preview = queue
                .last()
                .and_then(|json| serde_json::from_str::<MessageEnvelope>(json).ok())
                .map(|env| env.payload)
                .unwrap_or_default();
            out.push((username.clone(), queue.len(), preview));
        }
        Ok(out)
    }

    /// Accept a request: approve the sender and flush every held message from them
    /// into its thread. Returns how many were delivered.
    pub fn accept_contact(&self, username: &str) -> Result<usize> {
        let path = self.contacts_path()?;
        let held = crate::state::update_json::<Contacts, _>(&path, |contacts| {
            contacts.approved.insert(username.to_string());
            Ok(contacts.requests.remove(username).unwrap_or_default())
        })?;
        let mut delivered = 0;
        for env_json in &held {
            if self.accept_message(env_json).is_ok() {
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    /// Decline a request: drop every held message from `username` without
    /// approving them (they stay blocked).
    pub fn decline_contact(&self, username: &str) -> Result<()> {
        let path = self.contacts_path()?;
        crate::state::update_json::<Contacts, _>(&path, |contacts| {
            contacts.requests.remove(username);
            Ok(())
        })
    }

    /// The sender-side store-and-forward outbox for undelivered direct messages.
    fn dm_outbox_path(&self) -> Result<PathBuf> {
        Ok(self.store_dir()?.join("outbox-dm.json"))
    }

    /// Queue a direct message for retry until the recipient acknowledges it,
    /// keyed by its transport content `id` (idempotent — re-queuing is a no-op).
    pub fn queue_outbound(&self, id: &str, recipient: &str, envelope: &str) -> Result<()> {
        let path = self.dm_outbox_path()?;
        crate::state::update_json::<DmOutbox, _>(&path, |outbox| {
            outbox
                .pending
                .entry(id.to_string())
                .or_insert_with(|| OutboundDm {
                    recipient: recipient.to_string(),
                    envelope: envelope.to_string(),
                    queued_at: now_secs() as i64,
                });
            outbox.prune(now_secs() as i64);
            Ok(())
        })
    }

    /// Undelivered direct messages to retry: `(content id, recipient, envelope)`.
    pub fn pending_outbound(&self) -> Result<Vec<(String, String, String)>> {
        let path = self.dm_outbox_path()?;
        let outbox = crate::state::update_json::<DmOutbox, _>(&path, |outbox| {
            outbox.prune(now_secs() as i64);
            Ok(outbox.clone())
        })?;
        Ok(outbox
            .pending
            .iter()
            .map(|(id, dm)| (id.clone(), dm.recipient.clone(), dm.envelope.clone()))
            .collect())
    }

    /// Clear an outbound message once its recipient acknowledged receipt.
    pub fn mark_outbound_delivered(&self, id: &str) -> Result<()> {
        let path = self.dm_outbox_path()?;
        crate::state::update_json::<DmOutbox, _>(&path, |outbox| {
            outbox.pending.remove(id);
            Ok(())
        })
    }

    /// Assemble a room's thread in chronological order by walking parent links
    /// back from the room head, verifying every message. Muted authors are hidden
    /// (receiver-side) but still traversed — **mute ≠ deafen**.
    pub fn room_thread(&self, room: &str) -> Result<Vec<(Cid, MessageEnvelope)>> {
        let book = self.room_book()?;
        let mut out = Vec::new();
        let mut visited = BTreeSet::new();
        let root = match self.resolve(&room_latest_name(room)) {
            Ok(cid) => Some(cid),
            Err(Error::NameUnbound(_)) => None,
            Err(e) => return Err(e),
        };
        if let Some(cid) = root {
            self.collect_thread(room, &book, &cid, &mut visited, &mut out)?;
        }
        out.sort_by(|a, b| message_order(&a.1, &b.1).then_with(|| a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Raw stored message-envelope JSON for every message in a room — what the
    /// transport publishes to peers, byte-for-byte (so CIDs and signatures match).
    pub fn room_message_envelopes(&self, room: &str) -> Result<Vec<String>> {
        Ok(self
            .room_message_envelopes_with_cids(room)?
            .into_iter()
            .map(|(_, envelope)| envelope)
            .collect())
    }

    /// Stored message CIDs paired with their exact signed envelope JSON. Public
    /// transports use the CID to build and execute a `PublicRoomAttach` plan.
    pub fn room_message_envelopes_with_cids(&self, room: &str) -> Result<Vec<(Cid, String)>> {
        let mut out = Vec::new();
        for (cid, _) in self.room_thread(room)? {
            if let Record::Live { body_json, .. } = self.get(&CidOrName::Cid(cid.clone()))? {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body_json) {
                    if let Some(text) = value
                        .get("body")
                        .and_then(|b| b.get("text"))
                        .and_then(|t| t.as_str())
                    {
                        out.push((cid, text.to_string()));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Store a message received over the transport: **verify its signature**, gate
    /// it by the follow-list (unless `trust_all`), then persist it preserving the
    /// exact bytes (so its CID matches the sender's) and advance the room head.
    /// Returns the stored CID, or `None` if rejected (bad signature / unfollowed).
    pub fn store_inbound_message(
        &self,
        envelope_json: &str,
        trust_all: bool,
    ) -> Result<Option<Cid>> {
        let env: MessageEnvelope = serde_json::from_str(envelope_json)
            .map_err(|e| Error::Io(format!("inbound message parse: {e}")))?;
        let verified = crate::identity::verify(
            &AgentId(env.author().to_string()),
            &env.signing_bytes(),
            &env.sig,
        )
        .map_err(Error::Io)?;
        if !verified {
            return Ok(None);
        }
        if !trust_all {
            let me = self.agent_id()?.0;
            if env.author() != me && !self.social_book()?.is_following(env.author()) {
                return Ok(None);
            }
        }
        // Idempotent receive: a message's signature is its stable identity, so a
        // re-received message (e.g. periodic republish) is stored once. (mem stamps
        // `created_at`, so the *block* CID is install-specific; the signature is the
        // install-independent message id.)
        let id_name = message_id_name(&env.sig);
        if let Ok(existing) = self.resolve(&id_name) {
            return Ok(Some(existing));
        }
        let cid = self.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: serde_json::json!({ "text": envelope_json, "kind": "reference" })
                .to_string(),
        })?;
        self.bind(&id_name, &cid)?;
        self.bind(&room_latest_name(env.room()), &cid)?;
        Ok(Some(cid))
    }

    fn collect_thread(
        &self,
        room: &str,
        book: &RoomBook,
        cid: &Cid,
        visited: &mut BTreeSet<Cid>,
        out: &mut Vec<(Cid, MessageEnvelope)>,
    ) -> Result<()> {
        if !visited.insert(cid.clone()) {
            return Ok(());
        }
        let env = self.read_message(cid)?;
        for entry in &env.next {
            // `next` entries are parent *message ids* (signatures); resolve each to
            // its local block CID via the index. Fall back to treating the entry as
            // a block CID directly (legacy/manually-built links), and skip any
            // ancestor not present locally (a partial cross-install thread).
            let parent_cid = self
                .resolve(&message_id_name(entry))
                .unwrap_or_else(|_| Cid(entry.clone()));
            if matches!(
                self.get(&CidOrName::Cid(parent_cid.clone())),
                Ok(Record::Live { .. })
            ) {
                self.collect_thread(room, book, &parent_cid, visited, out)?;
            }
        }
        if !book.is_muted(room, &env.key) {
            out.push((cid.clone(), env));
        }
        Ok(())
    }
}
