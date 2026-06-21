#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;

    /// A unique scratch working dir under the OS temp dir, so `mem`'s
    /// cwd-scoped `.concierge` store never touches the user's real store.
    fn temp_workdir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut p = std::env::temp_dir();
        p.push(format!(
            "concierge-core-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn publish(mem: &MemCli, target: &str) -> Result<PublishReceipt> {
        // Decision 0026: everything is fenced from egress by default, so the
        // egress-unlock (set password + clear) is a precondition of publishing.
        // Already-public or already-cleared roots clear idempotently.
        if let Ok(root) = mem.resolve(target) {
            let _ = mem.set_password("pw");
            let _ = mem.clear_for_egress(&root, "test", "pw");
        }
        let plan = mem
            .build_egress_plan_for_target(target, crate::egress::EgressOperation::PublicPublish)?;
        mem.publish_public(&plan)
    }

    fn configure_fake_ipfs_backend(
        mem: &MemCli,
        dir: &Path,
        expected_requests: usize,
    ) -> (std::thread::JoinHandle<()>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake node");
        let addr = listener.local_addr().expect("addr");
        let api_url = format!("http://{addr}/api/v0");

        let join = std::thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).expect("read request");
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let headers_end = request
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .expect("headers end")
                    + 4;
                let header_text = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(k, v)| {
                            if k.eq_ignore_ascii_case("content-length") {
                                v.trim().parse::<usize>().ok()
                            } else {
                                None
                            }
                        })
                    })
                    .expect("content length");
                let already = request.len().saturating_sub(headers_end);
                let remaining = content_length.saturating_sub(already);
                if remaining > 0 {
                    let mut body = vec![0u8; remaining];
                    stream.read_exact(&mut body).expect("read body");
                }
                let response =
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                stream.write_all(response).expect("write response");
            }
        });

        let mut cfg = mem.config().expect("config");
        cfg.publishing.backend = "ipfs".to_string();
        cfg.publishing.ipfs_api = api_url.clone();
        cfg.save_to_project_root(dir).expect("save config");
        (join, api_url)
    }

    #[test]
    fn naming_petname_precedence_card_import_and_resolution() {
        let dir = temp_workdir("naming");
        let mem = MemCli::new(&dir);
        let me = mem.agent_id().unwrap().0;

        // A peer signs their own card; we import it (verifies).
        let peer = crate::identity::Identity::generate();
        let peer_aid = peer.agent_id().0;
        let mut card = crate::naming::ContactCard::new(&peer.agent_id(), "Jason", 100).unwrap();
        card.site_ipns = Some("k51peer".into());
        card.sign(&peer);
        assert_eq!(
            mem.import_card(&serde_json::to_string(&card).unwrap())
                .unwrap(),
            peer_aid
        );

        // No petname yet → the card name is a Card *hint* (unverified).
        let r = mem.resolve_display(&peer_aid);
        assert_eq!(r.text, "Jason");
        assert_eq!(r.source, NameSource::Card);
        assert!(!r.verified);

        // Petname wins and is verified (anti-spoofing precedence).
        mem.set_nickname(&peer_aid, "J-dawg").unwrap();
        let r = mem.resolve_display(&peer_aid);
        assert_eq!(r.text, "J-dawg");
        assert_eq!(r.source, NameSource::Petname);
        assert!(r.verified);

        // A tampered card is rejected at import.
        let mut forged = card.clone();
        forged.display_name = "Mallory".into();
        assert!(mem
            .import_card(&serde_json::to_string(&forged).unwrap())
            .is_err());

        // The user's own card builds + self-verifies for their AgentID.
        mem.update_my_card(Some("Me"), Some("hi"), None, Some("k51mine"))
            .unwrap();
        let my = mem.my_card().unwrap();
        assert_eq!(my.display_name, "Me");
        assert_eq!(my.site_ipns.as_deref(), Some("k51mine"));
        assert!(my.verify());
        assert_eq!(my.agent_id().unwrap().0, me);

        // Reverse lookup finds the petname candidate.
        assert!(mem
            .resolve_name("@j-dawg")
            .iter()
            .any(|(a, n)| a == &peer_aid && n.text == "J-dawg"));

        // A signed introduction round-trips through accept_introduction.
        let intro = mem.make_introduction(&peer_aid, "Jason from work").unwrap();
        assert_eq!(
            mem.accept_introduction(&serde_json::to_string(&intro).unwrap())
                .unwrap(),
            peer_aid
        );
    }

    #[test]
    fn put_bind_resolve_get_roundtrip_survives_restart() {
        let dir = temp_workdir("restart");

        // "Process 1": write a node and bind a name to it.
        let cid = {
            let mem = MemCli::new(&dir);
            let node = Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"phase 1 lives","kind":"project"}"#.to_string(),
            };
            let cid = mem.put_node(&node).expect("put_node");
            mem.bind("latest", &cid).expect("bind");
            assert_eq!(mem.resolve("latest").expect("resolve same-process"), cid);
            cid
        };

        // "Process 2": a brand-new binding over the same on-disk store. Because
        // each call is a fresh `mem` process reading `.concierge`, this is a real
        // restart — the Phase 1 exit criterion.
        let mem2 = MemCli::new(&dir);
        assert_eq!(
            mem2.resolve("latest").expect("resolve after restart"),
            cid,
            "a bound name must resolve to the same CID after restart"
        );
        match mem2
            .get(&CidOrName::Name("latest".to_string()))
            .expect("get after restart")
        {
            Record::Live {
                cid: got,
                kind,
                body_json,
            } => {
                assert_eq!(got, cid);
                assert_eq!(kind, "memory");
                assert!(body_json.contains("phase 1 lives"), "body must round-trip");
            }
            other => panic!("expected a live record, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_unbound_name_is_typed_error() {
        let dir = temp_workdir("unbound");
        let mem = MemCli::new(&dir);
        match mem.resolve("never-bound") {
            Err(Error::NameUnbound(_)) => {}
            other => panic!("expected NameUnbound, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_workdir_is_store_not_found() {
        let dir = temp_workdir("missing-store");
        let missing = dir.join("does-not-exist");
        let mem = MemCli::new(&missing);
        match mem.resolve("latest") {
            Err(Error::StoreNotFound(_)) => {}
            other => panic!("expected StoreNotFound, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_blob_roundtrips_and_walk_sees_it() {
        let dir = temp_workdir("blob");
        let mem = MemCli::new(&dir);
        let cid = mem.put_blob(b"hi", "text/plain").expect("put_blob");
        match mem.get(&CidOrName::Cid(cid.clone())).expect("get blob") {
            Record::Live {
                cid: got,
                kind,
                body_json,
            } => {
                assert_eq!(got, cid);
                assert_eq!(kind, "blob");
                let value: serde_json::Value = serde_json::from_str(&body_json).unwrap();
                assert_eq!(value["created_at"], serde_json::json!(0));
                assert_eq!(value["body"]["bytes"], serde_json::json!([104, 105]));
                assert_eq!(value["body"]["media_type"], serde_json::json!("text/plain"));
            }
            other => panic!("expected live blob record, got {other:?}"),
        }
        assert_eq!(
            mem.put_blob(b"hi", "text/plain").expect("repeat put_blob"),
            cid,
            "identical blobs have one stable content address"
        );
        assert_eq!(mem.walk(&cid).expect("walk blob"), vec![cid]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn day_tier_buckets_events_under_one_day_root() {
        let dir = temp_workdir("day-tier");
        let mem = MemCli::new(&dir);

        let prompt = mem
            .put_node(&Node {
                kind: "prompt".into(),
                fields_json: serde_json::json!({ "text": "hi" }).to_string(),
            })
            .expect("put prompt");
        let response = mem
            .put_node(&Node {
                kind: "response".into(),
                fields_json: serde_json::json!({ "text": "yo", "model": "unknown" }).to_string(),
            })
            .expect("put response");

        let ts = 1_749_470_400u64; // a fixed point inside one UTC day
        let date = mem::tombstones::iso8601(ts)[0..10].to_string();
        let day_name = format!("day-{date}");

        // Two events on the same day fold into ONE re-bound day root.
        mem.record_event_in_day(&date, "evt-1", &prompt)
            .expect("record evt-1");
        let day_cid = mem
            .record_event_in_day(&date, "evt-2", &response)
            .expect("record evt-2");
        assert_eq!(mem.resolve(&day_name).expect("resolve day"), day_cid);

        // The day's HAMT holds both events, keyed by their stable ids.
        let blocks = mem.blockstore().unwrap();
        let hamt_root = mem
            .day_hamt_root(&day_name)
            .unwrap()
            .expect("day hamt root");
        let hamt: mem::hamt::Hamt<_, mem::cid::Cid> =
            mem::hamt::Hamt::load(&blocks, &hamt_root).unwrap();
        let prompt_cid: mem::cid::Cid = prompt.0.parse().unwrap();
        let response_cid: mem::cid::Cid = response.0.parse().unwrap();
        assert_eq!(hamt.get(b"evt-1").unwrap(), Some(prompt_cid));
        assert_eq!(hamt.get(b"evt-2").unwrap(), Some(response_cid));

        // The day fans out to its events for the explorer.
        let mut events = mem.day_events(&date).expect("day events");
        events.sort();
        assert_eq!(
            events,
            vec![
                ("evt-1".to_string(), prompt.clone()),
                ("evt-2".to_string(), response.clone()),
            ]
        );

        // A different UTC day gets its own root (not the same day index).
        let date2 = mem::tombstones::iso8601(ts + 86_400)[0..10].to_string();
        let next_day = mem
            .record_event_in_day(&date2, "evt-3", &prompt)
            .expect("record next day");
        assert_ne!(next_day, day_cid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publish_is_recorded_as_a_store_node_in_the_day_calendar() {
        // A published site's CID must also be a node in the store so it appears in
        // Records — not just in the receipt trail / Studio sidecar.
        let dir = temp_workdir("publish-record");
        let mem = MemCli::new(&dir);
        let ts = 1_700_000_000u64;
        let receipt = PublishReceipt {
            root: "bafyTESTsiteroot".to_string(),
            backend: "ipfs-public".to_string(),
            unix_time: ts,
            gateway_url: "https://ipfs.io/ipns/k51TESTipns".to_string(),
            agent_id: "deadbeef".to_string(),
            signature: String::new(),
            ipns_name: Some("k51TESTipns".to_string()),
            site_name: Some("ConciergeSideKick".to_string()),
        };
        mem.record_publication(&receipt)
            .expect("record publication");

        // It lands in today's day calendar under a stable per-publish key.
        let date = utc_date(ts);
        let events = mem.day_events(&date).expect("day events");
        let (_key, node_cid) = events
            .iter()
            .find(|(k, _)| k == "publication-ipfs-public-1700000000")
            .expect("publication event filed in the day");

        // And the node is a real, content-addressed `publication` record carrying
        // the published root + IPNS so the explorer can surface the CID.
        match mem
            .get(&CidOrName::Cid(node_cid.clone()))
            .expect("get node")
        {
            Record::Live {
                kind, body_json, ..
            } => {
                assert_eq!(kind, "memory");
                assert!(body_json.contains("Published"), "reads as a publication");
                assert!(
                    body_json.contains("bafyTESTsiteroot"),
                    "carries the published root CID"
                );
                assert!(body_json.contains("k51TESTipns"), "carries the IPNS name");
                assert!(body_json.contains("ipfs-public"), "carries the platform");
            }
            other => panic!("expected live publication record, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn studio_checkpoint_saves_a_content_addressed_node_in_the_day_calendar() {
        // "Save checkpoint" snapshots the draft any time — a real CID + a checkpoint
        // node filed into Records, with no publish/egress.
        let dir = temp_workdir("studio-ckpt");
        let mem = MemCli::new(&dir);
        let (cid, ts) = mem
            .save_site_checkpoint("portfolio", "<h1>draft v1</h1>")
            .expect("save checkpoint");
        assert!(!cid.is_empty(), "snapshot is content-addressed");

        // Filed into today's calendar under a studio-checkpoint key so Records shows it.
        let events = mem.day_events(&utc_date(ts)).expect("day events");
        let (_key, node_cid) = events
            .iter()
            .find(|(k, _)| k.starts_with("studio-checkpoint-portfolio-"))
            .expect("studio checkpoint filed in the day");

        // The node is a real `checkpoint` over the snapshot blob.
        match mem
            .get(&CidOrName::Cid(node_cid.clone()))
            .expect("get node")
        {
            Record::Live {
                kind, body_json, ..
            } => {
                assert_eq!(kind, "checkpoint");
                assert!(
                    body_json.contains("studio:portfolio"),
                    "labelled as the studio checkpoint"
                );
            }
            other => panic!("expected live checkpoint record, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bookmark_sync_ingests_dedupes_and_files_into_records() {
        // Pillar A: a wallet-browser bookmark becomes a retrievable memory node, once.
        let dir = temp_workdir("bookmarks");
        let mem = MemCli::new(&dir);
        let bm_file = dir.join("Bookmarks");
        std::fs::write(
            &bm_file,
            r#"{"roots":{"bookmark_bar":{"type":"folder","name":"Bookmarks bar","children":[
                {"type":"url","name":"IPFS paper","url":"https://ipfs.tech/paper"},
                {"type":"url","name":"libp2p","url":"https://libp2p.io"},
                {"type":"url","name":"dup","url":"https://ipfs.tech/paper"}
            ]},"other":{"type":"folder","name":"Other","children":[]}}}"#,
        )
        .unwrap();
        std::env::set_var("CONCIERGE_BOOKMARKS_FILE", &bm_file);

        // Two unique URLs ingested (the duplicate is deduped).
        assert_eq!(mem.sync_browser_bookmarks().expect("sync").len(), 2);
        // Re-sync adds nothing (URL-keyed dedup via the bound name).
        assert_eq!(mem.sync_browser_bookmarks().expect("re-sync").len(), 0);

        // Each is a retrievable `memory` node bound under bookmark:<url-hash>.
        let key = crate::browser::url_key("https://libp2p.io");
        let cid = mem.resolve(&format!("bookmark:{key}")).expect("bound bookmark");
        match mem.get(&CidOrName::Cid(cid)).expect("get bookmark") {
            Record::Live { kind, body_json, .. } => {
                assert_eq!(kind, "memory");
                assert!(body_json.contains("libp2p"), "carries the bookmark");
            }
            other => panic!("expected live bookmark record, got {other:?}"),
        }

        std::env::remove_var("CONCIERGE_BOOKMARKS_FILE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wallet_link_verifies_a_signature_over_the_agent_id_and_rejects_mismatch() {
        use k256::ecdsa::{RecoveryId, Signature, SigningKey};
        use sha3::{Digest, Keccak256};
        let dir = temp_workdir("wallet-link");
        let mem = MemCli::new(&dir);
        let agent_id = mem.identity().unwrap().agent_id().0;
        let message = crate::wallet::link_message(&agent_id);

        // An external EVM key signs the link message (EIP-191 personal_sign).
        let key = SigningKey::from_bytes(&[9u8; 32].into()).unwrap();
        let point = key.verifying_key().to_encoded_point(false);
        let hash = Keccak256::digest(&point.as_bytes()[1..]);
        let address: String =
            format!("0x{}", hash[12..].iter().map(|b| format!("{b:02x}")).collect::<String>());
        let prefixed = format!("\x19Ethereum Signed Message:\n{}{}", message.len(), message);
        let (sig, recid): (Signature, RecoveryId) =
            key.sign_digest_recoverable(Keccak256::new_with_prefix(prefixed.as_bytes())).unwrap();
        let mut raw = sig.to_bytes().to_vec();
        raw.push(recid.to_byte() + 27);
        let sig_hex: String =
            format!("0x{}", raw.iter().map(|b| format!("{b:02x}")).collect::<String>());

        // The matching address links; the signature ties it to our AgentID.
        let link = mem.link_wallet(&address, "evm", &sig_hex).expect("link");
        assert_eq!(link.address, address.to_lowercase());
        assert_eq!(link.agent_id, agent_id);
        assert_eq!(mem.wallet_links().unwrap().len(), 1);
        // A different claimed address with the same signature is rejected.
        assert!(mem.link_wallet("0x000000000000000000000000000000000000dEaD", "evm", &sig_hex).is_err());
        // Unlink removes it.
        mem.unlink_wallet(&address).unwrap();
        assert!(mem.wallet_links().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wallet_propose_enforces_guards_and_stages_for_approval() {
        let dir = temp_workdir("wallet-propose");
        let mem = MemCli::new(&dir);
        let to = "0x000000000000000000000000000000000000bEEF";

        // Off by default → refused.
        assert!(mem.propose_wallet_tx(to, "0.01", "", "pay").is_err());

        // Enable access, cap 0.05, allowlist the recipient.
        mem.set_wallet_settings(
            &serde_json::json!({ "agent_access": true, "spend_cap": "0.05",
                "allowlist": [to.to_lowercase()], "preferred_chain": "" })
            .to_string(),
        )
        .unwrap();

        // Over the cap → refused; off-allowlist → refused; bad address → refused.
        assert!(mem.propose_wallet_tx(to, "0.10", "", "pay").is_err());
        assert!(mem.propose_wallet_tx("0x0000000000000000000000000000000000001234", "0.01", "", "x").is_err());
        assert!(mem.propose_wallet_tx("not-an-address", "0.01", "", "x").is_err());

        // Valid → staged pending; resolving clears it from pending.
        let p = mem.propose_wallet_tx(to, "0.01", "", "pay for X").unwrap();
        assert_eq!(p.status, "pending");
        assert_eq!(mem.pending_wallet_proposals().unwrap().len(), 1);
        mem.resolve_wallet_proposal(&p.id, "approved", "0xhash").unwrap();
        assert!(mem.pending_wallet_proposals().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calendar_rollup_links_days_into_months_and_years() {
        let dir = temp_workdir("calendar-rollup");
        let mem = MemCli::new(&dir);
        let rec = mem
            .put_node(&Node {
                kind: "prompt".into(),
                fields_json: serde_json::json!({ "text": "x" }).to_string(),
            })
            .expect("put");

        // Two June days + one July day.
        mem.record_event_in_day("2026-06-09", "e1", &rec).unwrap();
        mem.record_event_in_day("2026-06-10", "e2", &rec).unwrap();
        mem.record_event_in_day("2026-07-01", "e3", &rec).unwrap();
        mem.roll_up_calendar().unwrap();

        // Helper: the `label`s a manifest links, in order.
        let labels = |name: &str, field: &str| -> Vec<String> {
            let body_json = match mem.get(&CidOrName::Name(name.to_string())).unwrap() {
                Record::Live { body_json, .. } => body_json,
                other => panic!("expected live manifest, got {other:?}"),
            };
            let v: serde_json::Value = serde_json::from_str(&body_json).unwrap();
            v["body"][field]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["label"].as_str().unwrap().to_string())
                .collect()
        };

        assert_eq!(
            labels("month-2026-06", "days"),
            ["2026-06-09", "2026-06-10"]
        );
        assert_eq!(labels("month-2026-07", "days"), ["2026-07-01"]);
        assert_eq!(labels("year-2026", "months"), ["2026-06", "2026-07"]);

        // Re-running is idempotent (content-addressed): same year root CID.
        let year_first = mem.resolve("year-2026").unwrap();
        mem.roll_up_calendar().unwrap();
        assert_eq!(mem.resolve("year-2026").unwrap(), year_first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn calendar_manifests_use_a_deterministic_period_timestamp_not_the_wall_clock() {
        // Regression: a derived month/year manifest must stamp a `created_at`
        // derived from the *period it indexes* (midnight UTC of its start), not the
        // wall clock — otherwise re-rolling a second later produces a new CID and the
        // rollup is non-idempotent (the calendar flake).
        let dir = temp_workdir("calendar-deterministic");
        let mem = MemCli::new(&dir);
        let rec = mem
            .put_node(&Node {
                kind: "prompt".into(),
                fields_json: serde_json::json!({ "text": "x" }).to_string(),
            })
            .expect("put");
        mem.record_event_in_day("2026-06-09", "e1", &rec).unwrap();
        mem.roll_up_calendar().unwrap();

        let created_at = |name: &str| -> u64 {
            match mem.get(&CidOrName::Name(name.to_string())).unwrap() {
                Record::Live { body_json, .. } => {
                    serde_json::from_str::<serde_json::Value>(&body_json).unwrap()["created_at"]
                        .as_u64()
                        .unwrap()
                }
                other => panic!("expected live manifest, got {other:?}"),
            }
        };
        // Year 2026 → 2026-01-01T00:00:00Z; month 2026-06 → 2026-06-01T00:00:00Z.
        assert_eq!(created_at("year-2026"), period_start_unix("2026"));
        assert_eq!(created_at("year-2026"), 1_767_225_600, "Jan 1 2026 UTC");
        assert_eq!(created_at("month-2026-06"), period_start_unix("2026-06"));
        assert_eq!(created_at("month-2026-06"), 1_780_272_000, "Jun 1 2026 UTC");
        // It is the period start, deterministically — never the wall clock.
        assert!(
            created_at("year-2026") < now_secs(),
            "a fixed past timestamp, not 'now'"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn checkpoint_roundtrips_with_parent_and_walks_subgraph() {
        let dir = temp_workdir("checkpoint");
        let mem = MemCli::new(&dir);
        let root = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"phase 1 lives","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        let checkpoint = mem.checkpoint("latest", &root, None).expect("checkpoint");

        match mem
            .get(&CidOrName::Cid(checkpoint.clone()))
            .expect("get checkpoint")
        {
            Record::Live {
                cid: got,
                kind,
                body_json,
            } => {
                assert_eq!(got, checkpoint);
                assert_eq!(kind, "checkpoint");
                let value: serde_json::Value = serde_json::from_str(&body_json).unwrap();
                assert_eq!(value["body"]["label"], serde_json::json!("latest"));
                assert_eq!(value["body"]["root"], cid_to_json(&root).unwrap());
                assert!(value["body"]["parent"].is_null());
            }
            other => panic!("expected live checkpoint record, got {other:?}"),
        }

        let walked = mem.walk(&checkpoint).expect("walk checkpoint");
        let walked: std::collections::BTreeSet<_> = walked.into_iter().collect();
        assert!(walked.contains(&checkpoint));
        assert!(walked.contains(&root));
        assert_eq!(walked.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gc_returns_a_parsed_summary() {
        let dir = temp_workdir("gc");
        let mem = MemCli::new(&dir);
        let _orphan = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"throwaway","kind":"project"}"#.to_string(),
            })
            .expect("put_node");

        let report = mem.gc(&GcPolicy::default()).expect("gc");
        assert_eq!(report.removed, 1);
        assert_eq!(report.kept, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase 4 exit criterion: a memory graph moves between machines without
    /// losing identity. Export a subgraph from store A, import into a fresh
    /// store B, and assert the root CID and the whole reachable set are identical.
    #[test]
    fn car_roundtrip_preserves_root_and_subgraph() {
        let dir_a = temp_workdir("car-a");
        let mem_a = MemCli::new(&dir_a);
        let m1 = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"shared artifact","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        let cp = mem_a.checkpoint("snap", &m1, None).expect("checkpoint");
        let mut walk_a = mem_a.walk(&cp).expect("walk a");
        walk_a.sort();
        assert!(walk_a.len() >= 2, "checkpoint reaches its root node");

        let car = mem_a.export_car(&cp).expect("export_car");

        // A fresh store on "another machine".
        let dir_b = temp_workdir("car-b");
        let mem_b = MemCli::new(&dir_b);
        let root = mem_b.import_car(&car, "imported").expect("import_car");

        assert_eq!(root, cp, "root CID is preserved across export/import");
        assert_eq!(mem_b.resolve("imported").expect("resolve imported"), cp);
        let mut walk_b = mem_b.walk(&cp).expect("walk b");
        walk_b.sort();
        assert_eq!(walk_b, walk_a, "the full subgraph moves intact");

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn car_import_rejects_a_tampered_block() {
        let dir_a = temp_workdir("car-tamper-a");
        let mem_a = MemCli::new(&dir_a);
        let m1 = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"trust me","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        let cp = mem_a.checkpoint("snap", &m1, None).expect("checkpoint");
        let mut car = mem_a.export_car(&cp).expect("export_car");

        // Flip a byte in the block region — its CID will no longer verify.
        let last = car.len() - 1;
        car[last] ^= 0xFF;

        let dir_b = temp_workdir("car-tamper-b");
        let mem_b = MemCli::new(&dir_b);
        assert!(
            mem_b.import_car(&car, "x").is_err(),
            "a tampered CAR must be rejected, not silently imported"
        );

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn signed_share_imports_only_for_followed_signers_and_surfaces_in_shared_with_me() {
        let dir_a = temp_workdir("signed-share-a");
        let mem_a = MemCli::new(&dir_a);
        let (join, _api_url) = configure_fake_ipfs_backend(&mem_a, &dir_a, 1);
        let root = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"shared root","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem_a.bind("latest", &root).expect("bind");
        let car = mem_a.export_car(&root).expect("export car");
        let receipt = publish(&mem_a, "latest").expect("publish");

        let dir_b = temp_workdir("signed-share-b");
        let mem_b = MemCli::new(&dir_b);
        mem_b.follow(&receipt.agent_id).expect("follow signer");
        let imported = mem_b
            .import_signed_car(&car, "inbox", &receipt.agent_id, &receipt.signature)
            .expect("import signed car");
        assert_eq!(imported, root);

        let shared = mem_b.shared_with_me().expect("shared with me");
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].agent_id, receipt.agent_id);
        assert_eq!(shared[0].root, root);
        assert_eq!(shared[0].signature, receipt.signature);
        assert_eq!(shared[0].nickname, None);

        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn signed_share_rejects_unknown_signer() {
        let dir_a = temp_workdir("signed-share-reject-a");
        let mem_a = MemCli::new(&dir_a);
        let (join, _api_url) = configure_fake_ipfs_backend(&mem_a, &dir_a, 1);
        let root = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"root","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem_a.bind("latest", &root).expect("bind");
        let car = mem_a.export_car(&root).expect("export car");
        let receipt = publish(&mem_a, "latest").expect("publish");

        let dir_b = temp_workdir("signed-share-reject-b");
        let mem_b = MemCli::new(&dir_b);
        assert!(
            matches!(
                mem_b.import_signed_car(&car, "inbox", &receipt.agent_id, &receipt.signature),
                Err(Error::Io(_))
            ),
            "imports from unknown signers must be rejected until they are on the follow list"
        );

        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn signed_share_rejects_wrong_signature() {
        let dir_a = temp_workdir("signed-share-wrong-a");
        let mem_a = MemCli::new(&dir_a);
        let (join, _api_url) = configure_fake_ipfs_backend(&mem_a, &dir_a, 1);
        let root = mem_a
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"root","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem_a.bind("latest", &root).expect("bind");
        let car = mem_a.export_car(&root).expect("export car");
        let receipt = publish(&mem_a, "latest").expect("publish");

        let wrong_key_dir = temp_workdir("signed-share-wrong-key");
        let wrong_key = wrong_key_dir.join("identity.key");
        let wrong_identity = crate::identity::Identity::load_or_create(&wrong_key).expect("key");
        let wrong_signature = wrong_identity.sign(root.0.as_bytes());

        let dir_b = temp_workdir("signed-share-wrong-b");
        let mem_b = MemCli::new(&dir_b);
        mem_b.follow(&receipt.agent_id).expect("follow signer");
        assert!(
            matches!(
                mem_b.import_signed_car(&car, "inbox", &receipt.agent_id, &wrong_signature),
                Err(Error::Io(_))
            ),
            "imports with the wrong signature must be rejected"
        );

        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
        let _ = std::fs::remove_dir_all(&wrong_key_dir);
    }

    #[test]
    fn latest_share_pointer_updates_for_new_shares() {
        let dir = temp_workdir("latest-pointer");
        let mem = MemCli::new(&dir);
        let (join, _api_url) = configure_fake_ipfs_backend(&mem, &dir, 2);
        let agent_id = mem.agent_id().expect("agent id").0;

        let first = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"first","kind":"project"}"#.to_string(),
            })
            .expect("put first");
        mem.bind("latest", &first).expect("bind first");
        let receipt1 = publish(&mem, "latest").expect("publish first");
        let pointer_name = format!("latest-share-{agent_id}");
        let pointer1 = mem.resolve(&pointer_name).expect("pointer 1");

        let second = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"second","kind":"project"}"#.to_string(),
            })
            .expect("put second");
        mem.bind("latest", &second).expect("bind second");
        let receipt2 = publish(&mem, "latest").expect("publish second");
        let pointer2 = mem.resolve(&pointer_name).expect("pointer 2");

        assert_ne!(
            pointer1, pointer2,
            "the mutable latest pointer must advance"
        );
        let record = mem
            .get(&CidOrName::Cid(pointer2.clone()))
            .expect("pointer record");
        let Record::Live { body_json, .. } = record else {
            panic!("expected a live pointer record");
        };
        let (pointer_agent_id, root, signature) =
            parse_share_pointer(&body_json).expect("parse pointer");
        assert_eq!(pointer_agent_id, receipt2.agent_id);
        assert_eq!(root, second);
        assert_eq!(signature, receipt2.signature);
        assert_eq!(receipt1.agent_id, receipt2.agent_id);
        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn car_manifest_counts_blocks_and_bytes() {
        let dir = temp_workdir("car-manifest");
        let mem = MemCli::new(&dir);
        let m1 = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"sized","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        let cp = mem.checkpoint("snap", &m1, None).expect("checkpoint");

        let (cids, bytes) = mem.export_car_manifest(&cp).expect("manifest");
        assert_eq!(cids.len(), mem.walk(&cp).expect("walk").len());
        assert!(bytes > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_backends_includes_the_free_ipfs_backend() {
        let dir = temp_workdir("backends");
        let mem = MemCli::new(&dir);
        let backends = mem.list_backends().expect("list backends");
        assert!(
            backends.iter().any(|backend| backend.name == "ipfs"),
            "the free local Kubo backend should be compiled in: {backends:?}"
        );
        assert!(
            backends
                .iter()
                .any(|backend| backend.requirements_summary().contains("IPFS_API")),
            "backend requirements should be displayed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn share_without_a_configured_backend_is_a_typed_error() {
        let dir = temp_workdir("share-nobackend");
        let mem = MemCli::new(&dir);
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"unshared","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem.bind("latest", &cid).expect("bind");
        let mut cfg = mem.config().expect("config");
        cfg.publishing.backend = "bogus".to_string();
        cfg.save_to_project_root(&dir).expect("save config");
        assert!(
            matches!(publish(&mem, "latest"), Err(Error::BackendDown(_))),
            "publishing with an unconfigured backend must surface as BackendDown"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_share_requires_explicit_public_publication() {
        let dir = temp_workdir("legacy-share-refused");
        let mem = MemCli::new(&dir);
        assert!(matches!(
            mem.share("latest"),
            Err(Error::ExplicitPublicPublishRequired)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn personal_checkpoint_roots_are_locked_by_default() {
        let dir = temp_workdir("default-checkpoint-lock");
        let mem = MemCli::new(&dir);
        let root = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"private by default","kind":"project"}"#.to_string(),
            })
            .expect("put");
        let checkpoint = mem.checkpoint("session", &root, None).expect("checkpoint");
        let lock = mem
            .locks()
            .expect("locks")
            .into_iter()
            .find(|lock| lock.root == checkpoint.0)
            .expect("default checkpoint lock");
        assert_eq!(lock.reason, crate::egress::LockReason::DefaultPersonal);
        assert!(matches!(
            mem.write_reviewed_plaintext_car(
                &mem.build_egress_plan_for_target_and_backend(
                    &root.0,
                    crate::egress::EgressOperation::PlaintextCarExport,
                    "local-file",
                    &dir.join("blocked.car").display().to_string(),
                    "plaintext-portable",
                )
                .expect("plan"),
                &dir.join("blocked.car"),
            ),
            Err(Error::PublicationBlocked { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reviewed_plaintext_export_is_exact_destination_and_guarded() {
        let dir = temp_workdir("reviewed-plaintext-export");
        let mem = MemCli::new(&dir);
        let root = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"portable","kind":"project"}"#.to_string(),
            })
            .expect("put");
        // Decision 0026: fenced from egress by default — clear before exporting.
        mem.set_password("pw").expect("password");
        mem.clear_for_egress(&root, "test", "pw").expect("clear");
        let output = dir.join("reviewed.car");
        let plan = mem
            .build_egress_plan_for_target_and_backend(
                &root.0,
                crate::egress::EgressOperation::PlaintextCarExport,
                "local-file",
                &output.display().to_string(),
                "plaintext-portable",
            )
            .expect("plan");
        assert!(matches!(
            mem.write_reviewed_plaintext_car(&plan, &dir.join("changed.car")),
            Err(Error::EgressPlanChanged(_))
        ));
        let bytes = mem
            .write_reviewed_plaintext_car(&plan, &output)
            .expect("reviewed export");
        assert!(bytes > 0);
        assert_eq!(std::fs::metadata(&output).unwrap().len(), bytes);
        assert!(mem
            .security_events()
            .unwrap()
            .iter()
            .any(|event| event.action == "egress_approved" && event.root == root.0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn share_with_a_down_node_is_a_backend_error() {
        let dir = temp_workdir("share-nodedown");
        let mem = MemCli::new(&dir);
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"will not reach a node","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem.bind("latest", &cid).expect("bind");
        let mut cfg = mem.config().expect("config");
        cfg.publishing.backend = "ipfs".to_string();
        cfg.publishing.ipfs_api = "http://127.0.0.1:5999/api/v0".to_string();
        cfg.save_to_project_root(&dir).expect("save config");
        let result = publish(&mem, "latest");
        assert!(
            matches!(result, Err(Error::BackendDown(_))),
            "a down node must surface as BackendDown, got {result:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn public_publish_aborts_if_the_backend_target_changes_after_review() {
        let dir = temp_workdir("publish-target-change");
        let mem = MemCli::new(&dir);
        let root = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"reviewed","kind":"project"}"#.to_string(),
            })
            .expect("put");
        mem.bind("latest", &root).expect("bind");
        let plan = mem
            .build_egress_plan_for_target("latest", crate::egress::EgressOperation::PublicPublish)
            .expect("plan");
        let mut cfg = mem.config().expect("config");
        cfg.publishing.ipfs_api = "http://127.0.0.1:5998/api/v0".to_string();
        cfg.save_to_project_root(&dir).expect("save config");
        assert!(matches!(
            mem.publish_public(&plan),
            Err(Error::EgressPlanChanged(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn share_writes_a_local_receipt_and_posts_car_to_the_node() {
        let dir = temp_workdir("share-node");
        let mem = MemCli::new(&dir);
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"publish me","kind":"project"}"#.to_string(),
            })
            .expect("put_node");
        mem.bind("latest", &cid).expect("bind");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake node");
        let addr = listener.local_addr().expect("addr");
        let api_url = format!("http://{addr}/api/v0");

        let join = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).expect("read request");
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request);
            assert!(
                request_text.contains("POST /api/v0/dag/import?pin-roots=true HTTP/1.1"),
                "unexpected request: {request_text}"
            );
            let headers_end = request
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .expect("headers end")
                + 4;
            let header_text = String::from_utf8_lossy(&request[..headers_end]);
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(k, v)| {
                        if k.eq_ignore_ascii_case("content-length") {
                            v.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                })
                .expect("content length");
            let already = request.len().saturating_sub(headers_end);
            let remaining = content_length.saturating_sub(already);
            if remaining > 0 {
                let mut body = vec![0u8; remaining];
                stream.read_exact(&mut body).expect("read body");
            }
            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
            stream.write_all(response).expect("write response");
        });

        let mut cfg = mem.config().expect("config");
        cfg.publishing.backend = "ipfs".to_string();
        cfg.publishing.ipfs_api = api_url;
        cfg.save_to_project_root(&dir).expect("save config");

        let receipt = publish(&mem, "latest").expect("publish");
        assert_eq!(receipt.backend, "ipfs");
        assert_eq!(receipt.root, cid.0);
        assert!(receipt.gateway_url.contains(&cid.0));

        let receipt_trail = dir.join(".concierge").join("publish-receipts.jsonl");
        let trail = std::fs::read_to_string(&receipt_trail).expect("receipt trail");
        assert!(trail.contains(r#""backend":"ipfs""#));
        let receipts = mem.publish_receipts().expect("read receipts");
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].root, cid.0);
        join.join().expect("fake node thread");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_id_is_stable_across_restart_via_binding() {
        let dir = temp_workdir("agentid");
        let first = MemCli::new(&dir).agent_id().expect("agent id");
        // A fresh binding over the same working dir = a restart; the key persists.
        let second = MemCli::new(&dir)
            .agent_id()
            .expect("agent id after restart");
        assert_eq!(
            first, second,
            "the AgentID must be the same node after a restart"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn follow_and_nickname_persist_via_binding() {
        let dir = temp_workdir("social");
        let mem = MemCli::new(&dir);
        mem.follow("agent-xyz").expect("follow");
        mem.set_nickname("agent-xyz", "Friend").expect("nickname");
        let book = mem.social_book().expect("book");
        assert!(book.is_following("agent-xyz"));
        assert_eq!(book.nickname_of("agent-xyz"), Some(&"Friend".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn post_and_read_message_roundtrips_and_verifies() {
        let dir = temp_workdir("msg-roundtrip");
        let mem = MemCli::new(&dir);
        let cid = mem
            .post_message("conservation", "protect the wetlands")
            .expect("post");
        let env = mem.read_message(&cid).expect("read + verify");
        assert_eq!(env.payload, "protect the wetlands");
        assert_eq!(env.id, "conservation");
        assert_eq!(env.clock, 1);
        assert_eq!(env.key, mem.agent_id().unwrap().0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_works_even_when_the_parent_does_not_verify() {
        // Regression: a legacy/corrupt thread head (e.g. written by an earlier build
        // whose key/format differs) must not permanently block sending. The send path
        // links by the parent's clock+sig via `read_message_link` (no verification),
        // while `read_message` still rejects the tampered node.
        let dir = temp_workdir("msg-bad-parent");
        let mem = MemCli::new(&dir);
        let cid1 = mem.post_message("r", "one").expect("post 1");

        // Forge a tampered copy of the head (payload changed → signature no longer
        // matches) and make it the thread's latest, simulating the corrupt node.
        let mut env = mem.read_message_link(&cid1).expect("read link");
        env.payload = "tampered".to_string();
        let text = serde_json::to_string(&env).expect("serialize tampered env");
        let bad = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({ "text": text, "kind": "reference" })
                    .to_string(),
            })
            .expect("store tampered node");
        mem.bind(&room_latest_name("r"), &bad).expect("point head at bad node");

        // The tampered head fails verification...
        assert!(
            mem.read_message(&bad).is_err(),
            "a tampered parent must not verify"
        );
        // ...but a new message still appends, linking onto the (unverified) parent.
        let cid2 = mem
            .post_message("r", "two")
            .expect("send must succeed despite an unverifiable parent");
        let env2 = mem.read_message(&cid2).expect("the new message itself verifies");
        assert_eq!(env2.payload, "two");
        assert_eq!(
            env2.clock,
            env.clock + 1,
            "links onto the bad parent's clock"
        );
        assert_eq!(env2.next, vec![env.sig], "links onto the bad parent's signature");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn room_thread_assembles_in_chronological_order() {
        let dir = temp_workdir("msg-thread");
        let mem = MemCli::new(&dir);
        mem.post_message("r", "one").expect("1");
        mem.post_message("r", "two").expect("2");
        mem.post_message("r", "three").expect("3");
        let thread = mem.room_thread("r").expect("thread");
        let payloads: Vec<_> = thread.iter().map(|(_, e)| e.payload.clone()).collect();
        assert_eq!(payloads, ["one", "two", "three"]);
        assert_eq!(thread[0].1.clock, 1);
        assert_eq!(
            thread[2].1.clock, 3,
            "Lamport clock increments along the chain"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn message_thread_coheres_across_installs() {
        // Install A authors a 2-message chain; install B receives both envelopes
        // and must reassemble the same thread — even though `mem`'s `created_at`
        // gives the messages different *block* CIDs on each install. Linking is by
        // signature, which is identical everywhere.
        let dir_a = temp_workdir("xinstall-a");
        let a = MemCli::new(&dir_a);
        a.post_message("r", "first").expect("post 1");
        a.post_message("r", "second").expect("post 2");
        let envelopes = a.room_message_envelopes("r").expect("envelopes");
        assert_eq!(envelopes.len(), 2);

        let dir_b = temp_workdir("xinstall-b");
        let b = MemCli::new(&dir_b);
        for env_json in &envelopes {
            assert!(
                b.store_inbound_message(env_json, true)
                    .expect("store")
                    .is_some(),
                "B accepts A's signed message"
            );
        }
        let payloads: Vec<_> = b
            .room_thread("r")
            .expect("thread")
            .into_iter()
            .map(|(_, e)| e.payload)
            .collect();
        assert_eq!(
            payloads,
            ["first", "second"],
            "B reassembles the chain via signature links, not install-specific CIDs"
        );

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn room_thread_traverses_forked_next_links() {
        let dir = temp_workdir("msg-fork");
        let mem = MemCli::new(&dir);
        let identity = mem.identity().expect("identity");
        let author = identity.agent_id().0;

        let base = MessageEnvelope {
            id: "r".to_string(),
            payload: "base".to_string(),
            next: Vec::new(),
            refs: Vec::new(),
            clock: 1,
            key: author.clone(),
            sig: String::new(),
        };
        let mut base = base;
        base.sig = identity.sign(&base.signing_bytes());
        let base_cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": serde_json::to_string(&base).expect("serialize base"),
                    "kind": "reference",
                })
                .to_string(),
            })
            .expect("put base");

        let mut left = MessageEnvelope {
            id: "r".to_string(),
            payload: "left".to_string(),
            next: vec![base_cid.0.clone()],
            refs: Vec::new(),
            clock: 2,
            key: author.clone(),
            sig: String::new(),
        };
        left.sig = identity.sign(&left.signing_bytes());
        let left_cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": serde_json::to_string(&left).expect("serialize left"),
                    "kind": "reference",
                })
                .to_string(),
            })
            .expect("put left");

        let mut right = MessageEnvelope {
            id: "r".to_string(),
            payload: "right".to_string(),
            next: vec![base_cid.0.clone()],
            refs: Vec::new(),
            clock: 3,
            key: author.clone(),
            sig: String::new(),
        };
        right.sig = identity.sign(&right.signing_bytes());
        let right_cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": serde_json::to_string(&right).expect("serialize right"),
                    "kind": "reference",
                })
                .to_string(),
            })
            .expect("put right");

        let mut merge = MessageEnvelope {
            id: "r".to_string(),
            payload: "merge".to_string(),
            next: vec![left_cid.0.clone(), right_cid.0.clone()],
            refs: Vec::new(),
            clock: 4,
            key: author,
            sig: String::new(),
        };
        merge.sig = identity.sign(&merge.signing_bytes());
        let merge_cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({
                    "text": serde_json::to_string(&merge).expect("serialize merge"),
                    "kind": "reference",
                })
                .to_string(),
            })
            .expect("put merge");
        mem.bind("room-latest-r", &merge_cid).expect("bind merge");

        let thread = mem.room_thread("r").expect("thread");
        let payloads: Vec<_> = thread.iter().map(|(_, e)| e.payload.clone()).collect();
        assert_eq!(payloads, ["base", "left", "right", "merge"]);
        assert_eq!(thread.last().unwrap().1.payload, "merge");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ai_send_lever_blocks_ai_in_a_humans_only_room() {
        let dir = temp_workdir("msg-lever");
        // Mark this install as an AI.
        let cfg = Config {
            identity: crate::config::IdentityConfig {
                kind: "ai".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.save_to_project_root(&dir).expect("save cfg");
        let mem = MemCli::new(&dir);
        mem.set_room_ai_send("townhall", "off")
            .expect("humans-only");
        assert!(
            mem.post_message("townhall", "let me jump in").is_err(),
            "an AI cannot post to a Human-only room"
        );
        assert!(
            mem.post_message("open-room", "hello").is_ok(),
            "an open room still accepts the AI"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mention_gated_room_requires_an_at_mention_for_ai() {
        let dir = temp_workdir("msg-mention");
        let cfg = Config {
            identity: crate::config::IdentityConfig {
                kind: "ai".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        cfg.save_to_project_root(&dir).expect("save cfg");
        let mem = MemCli::new(&dir);
        mem.set_room_ai_send("brainstorm", "on_mention")
            .expect("mention mode");
        assert!(
            mem.post_message("brainstorm", "hello there").is_err(),
            "an AI without an @ mention must be blocked"
        );
        assert!(
            mem.post_message("brainstorm", "hello @brainstorm").is_ok(),
            "an AI with an @ mention can speak in mention-gated mode"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn muted_author_is_hidden_in_thread_but_still_in_the_dag() {
        let dir = temp_workdir("msg-mute");
        let mem = MemCli::new(&dir);
        let cid = mem.post_message("r", "from me").expect("post");
        let me = mem.agent_id().unwrap().0;
        mem.mute_in_room("r", &me).expect("mute");
        assert!(
            mem.room_thread("r").expect("thread").is_empty(),
            "muted author is hidden in the thread view"
        );
        assert_eq!(
            mem.read_message(&cid).expect("read by cid").payload,
            "from me",
            "but the message is still in the DAG (mute != deafen)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contact_and_naming_state_preserves_concurrent_and_fresh_updates() {
        let dir = temp_workdir("state-race");
        let mem = MemCli::new(&dir);
        let mut joins = Vec::new();
        for index in 0..16 {
            let mem = mem.clone();
            joins.push(std::thread::spawn(move || {
                mem.add_contact(&format!("contact-{index}")).unwrap();
            }));
        }
        for join in joins {
            join.join().unwrap();
        }
        assert_eq!(mem.approved_contacts().unwrap().len(), 16);

        let peer = Identity::generate();
        let mut newer = ContactCard::new(&peer.agent_id(), "new", 20).unwrap();
        newer.sign(&peer);
        mem.import_card(&serde_json::to_string(&newer).unwrap())
            .unwrap();
        let mut older = ContactCard::new(&peer.agent_id(), "old", 10).unwrap();
        older.sign(&peer);
        assert!(mem
            .import_card(&serde_json::to_string(&older).unwrap())
            .is_err());
        assert_eq!(
            mem.card_of(&peer.agent_id().0)
                .unwrap()
                .unwrap()
                .display_name,
            "new"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn naming_metadata_limits_fail_closed() {
        let dir = temp_workdir("naming-limits");
        let mem = MemCli::new(&dir);
        let peer = Identity::generate();
        let mut card =
            ContactCard::new(&peer.agent_id(), &"x".repeat(MAX_CONTACT_NAME_BYTES + 1), 1).unwrap();
        card.sign(&peer);
        assert!(mem
            .import_card(&serde_json::to_string(&card).unwrap())
            .is_err());
        assert!(mem
            .update_my_card(
                Some(&"x".repeat(MAX_CONTACT_NAME_BYTES + 1)),
                None,
                None,
                None
            )
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reviewed_external_site_deploy_rejects_folder_changes_before_network_egress() {
        let dir = temp_workdir("site-deploy-review");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        mem.set_deploy_credentials(
            "github",
            r#"{"token":"token","owner":"owner","repo":"repo","branch":"gh-pages"}"#,
        )
        .unwrap();
        let site = dir.join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "<h1>reviewed</h1>").unwrap();
        let reviewed = mem
            .review_site_deploy("site", site.to_str().unwrap(), "site", "github")
            .unwrap();
        std::fs::write(site.join("index.html"), "<h1>changed</h1>").unwrap();
        let error = mem.publish_site(&reviewed, "pw").unwrap_err().to_string();
        assert!(error.contains("changed after review"), "{error}");
        assert!(mem.publish_receipts().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn deploy_credentials_reject_symlinked_vault_files() {
        use std::os::unix::fs::symlink;

        let dir = temp_workdir("deploy-credential-symlink");
        let mem = MemCli::new(&dir);
        mem.set_deploy_credentials(
            "github",
            r#"{"token":"token","owner":"owner","repo":"repo","branch":"gh-pages"}"#,
        )
        .unwrap();
        let path = mem.deploy_credentials_path().unwrap();
        std::fs::remove_file(&path).unwrap();
        let outside = dir.join("outside.json");
        std::fs::write(&outside, "{}").unwrap();
        symlink(&outside, &path).unwrap();
        assert!(mem.deploy_credentials().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_site_receipt_verification_binds_the_exact_reviewed_plan() {
        let dir = temp_workdir("site-deploy-receipt");
        let mem = MemCli::new(&dir);
        let identity = mem.identity().unwrap();
        let site = dir.join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "<h1>site</h1>").unwrap();
        let files = crate::deploy::walk_files(&site).unwrap();
        let plan = crate::deploy::SiteDeployPlan::from_files(
            "site",
            &site,
            "site",
            "github",
            "https://api.github.com/repos/o/r/branches/gh-pages",
            &files,
        )
        .unwrap();
        let url = "https://o.github.io/r/";
        let signed = format!(
            "{}\n{}\n{}\n{}",
            plan.manifest_digest, plan.destination, plan.platform, url
        );
        let receipt = PublishReceipt {
            root: format!("external-manifest:{}", plan.manifest_digest),
            backend: plan.platform.clone(),
            unix_time: now_secs(),
            gateway_url: url.to_string(),
            agent_id: identity.agent_id().0,
            signature: identity.sign(signed.as_bytes()),
            ipns_name: None,
            site_name: Some(plan.name.clone()),
        };
        assert!(mem.verify_external_site_receipt(&receipt, &plan).unwrap());
        let mut changed = plan.clone();
        changed.destination.push_str("/other");
        assert!(!mem
            .verify_external_site_receipt(&receipt, &changed)
            .unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
