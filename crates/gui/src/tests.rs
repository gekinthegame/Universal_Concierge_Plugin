#[cfg(test)]
mod tests {
    use super::*;
    use concierge_core::{cid_link, naming::ContactCard, CoreBinding, GcPolicy, Identity, Node};
    use std::io::{Read, Write};
    use std::path::Path;

    fn store() -> (tempfile::TempDir, MemCli) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mem = MemCli::new(dir.path());
        (dir, mem)
    }

    fn canvas_project(mem: &MemCli, name: &str) -> std::path::PathBuf {
        let path = mem.store_dir().unwrap().join("canvas").join(name);
        std::fs::create_dir_all(&path).expect("canvas project dir");
        path
    }

    fn body(response: &Response) -> String {
        String::from_utf8_lossy(&response.body).into_owned()
    }

    fn put_named(mem: &MemCli, name: &str, text: &str) -> Cid {
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: serde_json::json!({ "text": text, "kind": "project" }).to_string(),
            })
            .expect("put");
        mem.bind(name, &cid).expect("bind");
        cid
    }

    fn configure_fake_ipfs_backend(
        mem: &MemCli,
        dir: &Path,
        expected_requests: usize,
    ) -> std::thread::JoinHandle<()> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake node");
        let addr = listener.local_addr().expect("addr");
        let api_url = format!("http://{addr}/api/v0");
        let join = std::thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let read = stream.read(&mut buf).expect("read request");
                    request.extend_from_slice(&buf[..read]);
                    if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let headers_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .expect("headers end")
                    + 4;
                let header_text = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        line.split_once(':').and_then(|(key, value)| {
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                    })
                    .expect("content length");
                let remaining = content_length.saturating_sub(request.len() - headers_end);
                if remaining > 0 {
                    let mut body = vec![0u8; remaining];
                    stream.read_exact(&mut body).expect("read body");
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .expect("write response");
            }
        });
        let mut config = mem.config().expect("config");
        config.publishing.backend = "ipfs".to_string();
        config.publishing.ipfs_api = api_url;
        config.save_to_project_root(dir).expect("save config");
        join
    }

    #[test]
    fn index_is_the_safe_live_explorer_shell() {
        let (_dir, mem) = store();
        let response = handle(&mem, "/", "");
        let page = body(&response);
        assert_eq!(response.status, 200);
        assert!(page.contains("Visual Memory Explorer"));
        // CAR export is CLI-only now (the redundant GUI button was removed).
        assert!(!page.contains("Export-CAR"));
        assert!(
            !page.contains("innerHTML"),
            "store data must never enter innerHTML"
        );
    }

    #[test]
    fn names_record_and_meta_endpoints_return_live_data() {
        let (_dir, mem) = store();
        let cid = put_named(&mem, "latest", "<img src=x onerror=alert(1)>");
        let names = body(&handle(&mem, "/api/names", ""));
        let record = body(&handle(&mem, "/api/record", "name=latest"));
        let options = GuiOptions::new(
            "hermes-model".to_string(),
            "/tmp/store".to_string(),
            false,
            None,
        );
        let meta = body(&handle_with_options(&mem, &options, "/api/meta", ""));
        assert!(names.contains("latest"));
        // The Names timeline needs a date, a kind, and a human description per
        // binding — not just the raw name/CID — to fold records by month/day.
        let names_value: serde_json::Value = serde_json::from_str(&names).expect("names json");
        let entry = &names_value.as_array().expect("array")[0];
        assert!(entry.get("created_at").and_then(|v| v.as_u64()).unwrap() > 0);
        assert_eq!(entry.get("kind").and_then(|v| v.as_str()), Some("memory"));
        assert!(entry
            .get("preview")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("<img src=x onerror=alert(1)>"));
        assert!(record.contains(&cid.0));
        assert!(record.contains("<img src=x onerror=alert(1)>"));
        assert!(meta.contains("hermes-model"));
        assert!(meta.contains("/tmp/store"));
    }

    #[test]
    fn a_fence_is_an_egress_badge_not_a_local_view_hider() {
        let (_dir, mem) = store();
        let cid = put_named(&mem, "secret", "sensitive text");
        let record = mem.get(&CidOrName::Cid(cid.clone())).unwrap();

        // Decision 0026: fenced from egress by default, nothing cleared.
        let privacy = PrivacyOverlay {
            cleared_roots: BTreeSet::new(),
            cleared_cids: BTreeSet::new(),
            known_public: BTreeSet::new(),
            quarantined: BTreeSet::new(),
        };
        let (node, _) = node_and_links_from_record(&mem, &privacy, &BTreeSet::new(), &cid, &record);

        // A fence is an EGRESS safeguard, not a local-view control — the user sees
        // their own data on their own device. So content + metadata are fully
        // visible locally …
        assert_eq!(node["preview"], "sensitive text");
        assert_ne!(node["kind"], "locked", "real kind shown locally");
        assert!(node["created_at"].as_i64().is_some(), "timestamp visible");
        // … and the fence surfaces only as a badge (the default, not cleared).
        assert_eq!(node["fenced"], true);
        assert_eq!(node["cleared"], false);
    }

    #[test]
    fn forest_groups_sessions_into_calendar_tiers() {
        let (_dir, mem) = store();
        // Two session-named events under one session. The forest groups by
        // session (store → year → month → day → session), not by record.
        let e1 = put_named(&mem, "host:test:session:S1:event:E1", "first");
        let e2 = put_named(&mem, "host:test:session:S1:event:E2", "second");

        let forest = body(&handle(&mem, "/api/graph", ""));
        let today = concierge_core::utc_today();
        let year = &today[0..4];
        let month = &today[0..7];

        assert!(forest.contains("\"cid\":\"store:root\""));
        assert!(
            forest.contains(&format!("year:{year}")),
            "year tier present"
        );
        assert!(
            forest.contains(&format!("month:{month}")),
            "month tier present"
        );
        assert!(forest.contains(&format!("day:{today}")), "day tier present");
        assert!(forest.contains("\"relation\":\"year\""));
        assert!(forest.contains("\"relation\":\"day\""));
        // The leaf is the SESSION, not the individual records.
        assert!(
            forest.contains("\"relation\":\"session\""),
            "session relation"
        );
        assert!(
            forest.contains("\"kind\":\"session\""),
            "session leaf present"
        );
        // Individual event records are not drawn — the Records tab goes deeper.
        assert!(
            !forest.contains(&e1.0) && !forest.contains(&e2.0),
            "records are not drawn as graph leaves"
        );
    }

    #[test]
    fn graph_checkpoint_stats_and_guarded_car_preview_cover_the_plan_views() {
        let (_dir, mem) = store();
        let root = put_named(&mem, "root", "explore me");
        let public = put_named(&mem, "public", "safe export");
        let checkpoint = mem.checkpoint("head", &root, None).expect("checkpoint");
        mem.bind("latest", &checkpoint).expect("latest");
        mem.set_password("pw").expect("password");

        let graph = body(&handle(&mem, "/api/graph", "name=latest"));
        let checkpoints = body(&handle(&mem, "/api/checkpoints", ""));
        let stats = body(&handle(&mem, "/api/stats", "name=latest"));
        let public_plan = mem
            .build_egress_plan_for_target_and_backend(
                "public",
                concierge_core::EgressOperation::PlaintextCarExport,
                "browser-download",
                "browser-download",
                "plaintext-portable",
            )
            .expect("public plan");
        let plan_response = handle(&mem, "/api/egress-plan", "name=public");
        let locked_car = handle(&mem, "/api/export-car", "name=latest");
        let car = handle(&mem, "/api/export-car", "name=public");
        let unreviewed = handle(&mem, "/api/export-car", "name=public");
        let missing_target = handle(&mem, "/api/export-car", "");

        assert!(graph.contains(&checkpoint.0));
        assert!(graph.contains(&root.0));
        assert!(graph.contains("checkpoint_root"));
        assert!(checkpoints.contains("\"label\":\"head\""));
        assert!(stats.contains("\"car_size\":"));
        assert!(stats.contains("\"pin_status\":"));
        // Phase B: stats always reports publishing readiness (opt-in signal).
        assert!(stats.contains("\"publishing_ready\":"));
        assert_eq!(locked_car.status, 400);
        assert_eq!(missing_target.status, 400);
        assert_eq!(unreviewed.status, 400);
        assert_eq!(plan_response.status, 200);
        assert!(body(&plan_response).contains(&public_plan.manifest_digest));
        assert!(body(&plan_response).contains("\"review_token\":"));
        assert_eq!(car.status, 400);
        assert_ne!(public, checkpoint);
    }

    #[test]
    fn publishing_reads_as_opt_in_when_no_node_is_running() {
        // Phase B: an absent publishing node is a normal "not set up yet" state,
        // surfaced as opt-in guidance — never a startup failure or error status.
        let (dir, mem) = store();
        // Pin the backend at a guaranteed-closed local port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let dead_port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut config = mem.config().expect("config");
        config.publishing.ipfs_api = format!("http://127.0.0.1:{dead_port}/api/v0");
        config
            .save_to_project_root(dir.path())
            .expect("save config");

        let response = handle(&mem, "/api/stats", "");
        assert_eq!(
            response.status, 200,
            "stats must never fail when the node is down"
        );
        let stats = body(&response);
        assert!(stats.contains("\"publishing_ready\":false"));
        assert!(stats.contains("\"reachable\":false"));
        assert!(stats.contains("publishing is optional"));
    }

    #[test]
    fn network_map_surfaces_membership_capabilities_and_revocation() {
        let (_dir, mem) = store();
        let opts = GuiOptions::default();

        // Empty before any network exists.
        assert!(body(&handle(&mem, "/api/network", "")).contains("\"networks\":[]"));

        // Found a network from the Data Platter (no CLI).
        let created = handle_mutation(
            &mem,
            &opts,
            "/api/network/create",
            r#"{"name":"research-team"}"#,
        );
        assert_eq!(created.status, 200);
        let map = body(&created);
        assert!(map.contains("research-team"));
        assert!(map.contains("\"is_root\":true"), "this device founded it");
        assert!(map.contains("\"membership_epoch\":0"));
        assert!(map.contains("\"descriptor_valid\":true"));
        assert!(
            map.contains("\"valid\":true"),
            "the founding device's membership/capabilities verify"
        );

        // Revoke a subject → the epoch advances and the subject is listed revoked.
        let subject = "aaaa1111bbbb2222cccc3333dddd4444eeee5555ffff6666aaaa7777bbbb8888";
        let revoked = handle_mutation(
            &mem,
            &opts,
            "/api/network/revoke",
            &format!(r#"{{"subject":"{subject}"}}"#),
        );
        assert_eq!(revoked.status, 200);
        let after = body(&revoked);
        assert!(
            after.contains("\"membership_epoch\":1"),
            "revocation advanced the epoch"
        );
        assert!(
            after.contains(subject),
            "the revoked subject is surfaced in the map"
        );
    }

    #[test]
    fn network_rotate_requires_the_ciphertext_root_and_password_in_the_body() {
        // The rotation crypto is proven in core; here we check the endpoint guards
        // its required fields and never takes the password in the URL.
        let (_dir, mem) = store();
        let opts = GuiOptions::default();
        assert_eq!(
            handle_mutation(&mem, &opts, "/api/network/rotate", r#"{"password":"pw"}"#).status,
            400
        );
        assert_eq!(
            handle_mutation(
                &mem,
                &opts,
                "/api/network/rotate",
                r#"{"ciphertext_root":"bafyX"}"#
            )
            .status,
            400
        );
        // Well-formed but unknown root → a clean error, not a panic.
        let resp = handle_mutation(
            &mem,
            &opts,
            "/api/network/rotate",
            r#"{"ciphertext_root":"bafyUNKNOWN","password":"pw"}"#,
        );
        assert_ne!(resp.status, 200);
    }

    #[test]
    fn studio_publish_checkpoints_record_list_and_restore() {
        let (dir, mem) = store();
        // A "published" folder with its index.html.
        let folder = dir.path().join("pub");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("index.html"), "<h1>v1</h1>").unwrap();
        record_site_checkpoint(
            &mem,
            "my-site",
            folder.to_str().unwrap(),
            Some("k51test"),
            "bafytest",
            "https://ipfs.io/ipns/k51test",
        )
        .unwrap();

        // Listed (timestamped, with the stable IPNS), newest first.
        let listed = body(&handle(&mem, "/api/site/checkpoints", ""));
        let v: serde_json::Value = serde_json::from_str(&listed).unwrap();
        let first = &v["checkpoints"][0];
        assert_eq!(first["site"].as_str(), Some("my-site"), "{listed}");
        assert_eq!(first["ipns"].as_str(), Some("k51test"));
        let ts = first["ts"].as_u64().expect("timestamp present");

        // Restorable: the saved HTML comes back for re-editing.
        let restored = body(&handle(
            &mem,
            "/api/site/checkpoint",
            &format!("site=my-site&ts={ts}"),
        ));
        assert!(
            restored.contains("<h1>v1</h1>"),
            "restores saved html: {restored}"
        );

        // A non-numeric ts is rejected (no path games).
        assert_eq!(
            handle(&mem, "/api/site/checkpoint", "site=my-site&ts=evil").status,
            400
        );
    }

    #[test]
    fn compact_runs_gc_and_reports_what_it_reclaimed() {
        let (_dir, mem) = store();
        // A named node survives; an unnamed put is a reclaimable orphan.
        put_named(&mem, "keep", "a kept memory under a live name");
        mem.put_node(&Node {
            kind: "memory".to_string(),
            fields_json: r#"{"text":"orphan","kind":"project"}"#.to_string(),
        })
        .expect("put orphan");

        let opts = GuiOptions::default();
        let resp = handle_mutation(&mem, &opts, "/api/compact", "{}");
        assert_eq!(resp.status, 200, "{}", body(&resp));
        let parsed: serde_json::Value = serde_json::from_str(&body(&resp)).unwrap();
        assert!(
            parsed["removed"].is_u64(),
            "reports a reclaimed count: {parsed}"
        );
        assert!(
            parsed["kept"].as_u64().unwrap() >= 1,
            "the named node is kept: {parsed}"
        );
        // The live graph is intact after compaction.
        assert_eq!(handle(&mem, "/api/names", "").status, 200);
    }

    #[test]
    fn tombstone_record_returns_a_receipt_instead_of_an_error() {
        let (_dir, mem) = store();
        let cid = mem
            .put_node(&Node {
                kind: "memory".to_string(),
                fields_json: r#"{"text":"orphan","kind":"project"}"#.to_string(),
            })
            .expect("put");
        mem.gc(&GcPolicy {
            keep_checkpoints: Some(0),
        })
        .expect("gc");
        let response = handle(&mem, "/api/record", &format!("cid={}", cid.0));
        assert_eq!(response.status, 200);
        assert!(body(&response).contains("\"live\":false"));
    }

    #[test]
    fn thread_endpoint_includes_policy_participants_and_hidden_cids() {
        let (_dir, mem) = store();
        let cid = mem
            .post_message("conservation", "protect the wetlands")
            .expect("post");
        let response = handle(&mem, "/api/thread", "room=conservation");
        let text = body(&response);
        assert!(text.contains("protect the wetlands"));
        assert!(text.contains(&cid.0));
        assert!(text.contains("\"ai_send\":\"on\""));
        // Moderator badge data (Phase 8 §3/§4): Guardian status + synthesis flag.
        assert!(
            text.contains("\"guardian\":\"active\""),
            "Guardian badge present"
        );
        assert!(
            text.contains("\"synthesis_candidate\":false"),
            "short thread is not a candidate"
        );
        assert!(text.contains("\"message_count\":1"));
        // Phase N · Phase I — social legibility: a self-authored message is `Local`,
        // carries a structural-importance count, and the follow-lens flag.
        assert!(
            text.contains("\"trust_tier\":\"local\""),
            "own message is the Local tier"
        );
        assert!(text.contains("\"trust_label\":\"Local\""));
        assert!(
            text.contains("\"importance\":0"),
            "an orphan message ties nothing together yet"
        );
        assert!(text.contains("\"followed\":false"));
    }

    #[test]
    fn malformed_unicode_query_never_panics() {
        let (_dir, mem) = store();
        let response = handle(&mem, "/api/record", "name=%a%C3%A9");
        assert_eq!(response.status, 500);
    }

    #[test]
    fn missing_parameters_and_unknown_paths_have_specific_statuses() {
        let (_dir, mem) = store();
        assert_eq!(handle(&mem, "/api/record", "").status, 400);
        assert_eq!(handle(&mem, "/api/thread", "").status, 400);
        assert_eq!(handle(&mem, "/nope", "").status, 404);
    }

    #[test]
    fn socket_responses_use_correct_reason_phrases_and_bound_headers() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            serve_connection(&mem, &options, stream).expect("serve");
        });
        let mut client = TcpStream::connect(addr).expect("connect");
        client
            .write_all(b"GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read");
        server.join().expect("join");
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(response.contains("Content-Security-Policy:"));
        assert!(response.contains("frame-ancestors 'none'"));
        assert!(response.contains("X-Frame-Options: DENY"));
        assert!(response.contains("Cache-Control: no-store"));
        assert!(!response.contains("Access-Control-Allow-Origin"));
        assert!(!response.contains("Set-Cookie"));
    }

    #[test]
    fn canvas_preview_is_opaque_origin_and_frameable() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let site = canvas_project(&mem, "preview");
        std::fs::write(
            site.join("index.html"),
            "<script>document.body.textContent='ok'</script>",
        )
        .expect("write preview");
        let token = preview_token(&site.canonicalize().expect("canonical site"));
        options.preview_dirs.lock().expect("preview lock").insert(
            token.clone(),
            site.canonicalize().expect("canonical site"),
        );

        let preview = canvas_preview_serve(&mem, &options, &format!("{token}/index.html"));
        assert_eq!(preview.status, 200);
        assert!(preview.embeddable);
        assert!(preview
            .csp
            .expect("preview csp")
            .contains("sandbox allow-scripts"));
        // The opaque-origin iframe loads ES modules (vendored Three.js, importmaps) in CORS mode;
        // these headers let those cross-origin (null→loopback) fetches succeed. Without them the
        // module scripts are blocked. Only the preview static files carry CORS — never the API.
        let cors = |name: &str| {
            preview
                .headers
                .iter()
                .any(|(k, v)| k == name && (v == "*" || v == "cross-origin"))
        };
        assert!(
            cors("Access-Control-Allow-Origin"),
            "preview must allow cross-origin module fetches from the null-origin iframe"
        );
        assert!(cors("Cross-Origin-Resource-Policy"));

        let page = body(&handle(&mem, "/", ""));
        assert!(page.contains(r#"sandbox="allow-scripts""#));
        assert!(!page.contains("allow-same-origin"));
        assert!(!page.contains("allow-popups"));
        assert!(!page.contains("allow-forms"));
    }

    #[test]
    fn canvas_write_saves_into_the_open_folder_and_reads_back() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let site = canvas_project(&mem, "write");
        let folder = site.to_string_lossy().to_string();

        // Open the folder → token (the unified writeable canvas registers it for preview).
        let open = body(&mutation_canvas_open(
            &mem,
            &options,
            &serde_json::json!({ "folder": folder }).to_string(),
        ));
        let open: serde_json::Value = serde_json::from_str(&open).expect("open json");
        let token = open["token"].as_str().expect("token").to_string();

        // Write index.html through the editor seam, then read it straight back.
        let write = mutation_canvas_write(
            &mem,
            &options,
            &serde_json::json!({ "token": token, "path": "index.html", "content": "<h1>hi</h1>" })
                .to_string(),
        );
        assert_eq!(write.status, 200);
        assert_eq!(
            std::fs::read_to_string(site.join("index.html")).expect("read index"),
            "<h1>hi</h1>"
        );
        let file = body(&canvas_file_get(
            &mem,
            &options,
            &format!("token={token}&path=index.html"),
        ));
        let file: serde_json::Value = serde_json::from_str(&file).expect("file json");
        assert_eq!(file["content"].as_str(), Some("<h1>hi</h1>"));

        // A nested path creates its parent inside the folder.
        let nested = mutation_canvas_write(
            &mem,
            &options,
            &serde_json::json!({ "token": token, "path": "css/site.css", "content": "body{}" })
                .to_string(),
        );
        assert_eq!(nested.status, 200);
        assert!(site.join("css").join("site.css").is_file());
    }

    #[test]
    fn canvas_write_streams_binary_chunks_at_offsets() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let site = canvas_project(&mem, "stream");
        let folder = site.to_string_lossy().to_string();
        let open = body(&mutation_canvas_open(
            &mem,
            &options,
            &serde_json::json!({ "folder": folder }).to_string(),
        ));
        let token = serde_json::from_str::<serde_json::Value>(&open).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        // Stream two base64 chunks at offsets 0 and 3 ("ABC"=QUJD, "DEF"=REVG) → "ABCDEF".
        for (b64, pos) in [("QUJD", 0u64), ("REVG", 3)] {
            let w = mutation_canvas_write(
                &mem,
                &options,
                &serde_json::json!({ "token": token, "path": "v.bin", "content": b64, "base64": true, "pos": pos })
                    .to_string(),
            );
            assert_eq!(w.status, 200);
        }
        assert_eq!(std::fs::read(site.join("v.bin")).unwrap(), b"ABCDEF");
        // A fresh pos:0 chunk truncates first (a new render starts clean): "X"=WA==.
        let w = mutation_canvas_write(
            &mem,
            &options,
            &serde_json::json!({ "token": token, "path": "v.bin", "content": "WA==", "base64": true, "pos": 0 })
                .to_string(),
        );
        assert_eq!(w.status, 200);
        assert_eq!(std::fs::read(site.join("v.bin")).unwrap(), b"X");
    }

    #[test]
    fn canvas_pwa_makes_the_project_installable() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let site = canvas_project(&mem, "pwa");
        let folder = site.to_string_lossy().to_string();
        std::fs::write(
            site.join("index.html"),
            "<!doctype html><html><head><title>My Game</title></head><body><h1>hi</h1></body></html>",
        )
        .unwrap();

        let open = body(&mutation_canvas_open(
            &mem,
            &options,
            &serde_json::json!({ "folder": folder }).to_string(),
        ));
        let token = serde_json::from_str::<serde_json::Value>(&open).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = mutation_canvas_pwa(&mem, &options, &serde_json::json!({ "token": token }).to_string());
        assert_eq!(resp.status, 200);

        // Manifest + service worker + (non-empty) icons are written.
        let manifest = std::fs::read_to_string(site.join("manifest.json")).expect("manifest");
        assert!(manifest.contains("\"My Game\""));
        assert!(manifest.contains("standalone"));
        assert!(site.join("service-worker.js").is_file());
        assert!(std::fs::metadata(site.join("icon-512.png")).unwrap().len() > 100);
        assert!(std::fs::metadata(site.join("icon-192.png")).unwrap().len() > 100);

        // index.html now links the manifest, registers the worker, and has the iOS icon.
        let html = std::fs::read_to_string(site.join("index.html")).unwrap();
        assert!(html.contains("rel=\"manifest\""));
        assert!(html.contains("serviceWorker"));
        assert!(html.contains("apple-touch-icon"));

        // Idempotent: a second pass reports already-done and doesn't double-inject.
        let again = body(&mutation_canvas_pwa(
            &mem,
            &options,
            &serde_json::json!({ "token": token }).to_string(),
        ));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&again).unwrap()["already"].as_bool(),
            Some(true)
        );
        let html2 = std::fs::read_to_string(site.join("index.html")).unwrap();
        assert_eq!(html2.matches("rel=\"manifest\"").count(), 1);
    }

    #[test]
    fn canvas_new_scaffolds_a_website_and_an_app() {
        let (_dir, mem) = store();

        // Website: index.html + style.css, no PWA files.
        let web = body(&mutation_canvas_new(
            &mem,
            &serde_json::json!({ "name": "My Site", "kind": "website" }).to_string(),
        ));
        let web: serde_json::Value = serde_json::from_str(&web).unwrap();
        let web_path = std::path::PathBuf::from(web["path"].as_str().unwrap());
        assert!(web_path.join("index.html").is_file());
        assert!(web_path.join("style.css").is_file());
        assert!(!web_path.join("manifest.json").exists());
        assert!(std::fs::read_to_string(web_path.join("index.html"))
            .unwrap()
            .contains("My Site"));

        // App/game: staged as an installable PWA from the start.
        let app = body(&mutation_canvas_new(
            &mem,
            &serde_json::json!({ "name": "My Game", "kind": "app" }).to_string(),
        ));
        let app: serde_json::Value = serde_json::from_str(&app).unwrap();
        let app_path = std::path::PathBuf::from(app["path"].as_str().unwrap());
        assert!(app_path.join("manifest.json").is_file());
        assert!(app_path.join("service-worker.js").is_file());
        assert!(app_path.join("app.js").is_file());
        assert!(std::fs::metadata(app_path.join("icon-512.png")).unwrap().len() > 100);
        let ahtml = std::fs::read_to_string(app_path.join("index.html")).unwrap();
        assert!(ahtml.contains("rel=\"manifest\"") && ahtml.contains("serviceWorker"));

        // Movie/animation: a self-contained browser animation — GSAP + Lottie bundled in.
        let movie = body(&mutation_canvas_new(
            &mem,
            &serde_json::json!({ "name": "My Film", "kind": "movie" }).to_string(),
        ));
        let movie: serde_json::Value = serde_json::from_str(&movie).unwrap();
        let movie_path = std::path::PathBuf::from(movie["path"].as_str().unwrap());
        assert!(std::fs::metadata(movie_path.join("gsap.min.js")).unwrap().len() > 1000);
        assert!(std::fs::metadata(movie_path.join("lottie.min.js")).unwrap().len() > 1000);
        assert!(movie_path.join("animation.js").is_file());
        assert!(movie_path.join("capture.js").is_file());
        assert!(std::fs::metadata(movie_path.join("webm-muxer.js")).unwrap().len() > 1000);
        assert!(movie_path.join("README.md").is_file());
        let ajs = std::fs::read_to_string(movie_path.join("animation.js")).unwrap();
        // Deterministic, seekable, full-length rendering (not a fixed-time clip).
        assert!(ajs.contains("__duration") && ajs.contains("__seek"));
        let mhtml = std::fs::read_to_string(movie_path.join("index.html")).unwrap();
        assert!(mhtml.contains("<canvas id=\"stage\"") && mhtml.contains("webm-muxer.js"));

        // A Game / 3D project ships the Babylon engine + the character controller + the seekable,
        // capturable scene — one substrate for a scene, a game, OR a movie.
        let game = body(&mutation_canvas_new(
            &mem,
            &serde_json::json!({ "name": "Voxel World", "kind": "game" }).to_string(),
        ));
        let game: serde_json::Value = serde_json::from_str(&game).unwrap();
        let game_path = std::path::PathBuf::from(game["path"].as_str().unwrap());
        assert!(
            std::fs::metadata(game_path.join("babylon.js")).unwrap().len() > 1_000_000,
            "Game project must bundle the full Babylon engine"
        );
        assert!(game_path.join("CharacterController.js").is_file());
        assert!(game_path.join("capture.js").is_file() && game_path.join("webm-muxer.js").is_file());
        let ghtml = std::fs::read_to_string(game_path.join("index.html")).unwrap();
        // Cinematic + deterministic-seekable scene wired to the video exporter.
        assert!(ghtml.contains("BABYLON.Engine") && ghtml.contains("babylon.js"));
        assert!(ghtml.contains("window.__seek") && ghtml.contains("goToFrame"));
        assert!(ghtml.contains("TONEMAPPING_ACES"));

        // A duplicate name is refused (no silent overwrite).
        let dup = mutation_canvas_new(
            &mem,
            &serde_json::json!({ "name": "My Site", "kind": "website" }).to_string(),
        );
        assert_eq!(dup.status, 400);

        // Delete removes the project folder; traversal/escape names are refused.
        let escape = mutation_canvas_delete(
            &mem,
            &serde_json::json!({ "name": "../secret" }).to_string(),
        );
        assert_eq!(escape.status, 400);
        assert!(web_path.is_dir());
        let del = mutation_canvas_delete(&mem, &serde_json::json!({ "name": "My-Site" }).to_string());
        assert_eq!(del.status, 200);
        assert!(!web_path.exists());
    }

    #[test]
    fn canvas_write_refuses_to_escape_the_folder() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let site = canvas_project(&mem, "escape");
        let folder = site.to_string_lossy().to_string();
        let open = body(&mutation_canvas_open(
            &mem,
            &options,
            &serde_json::json!({ "folder": folder }).to_string(),
        ));
        let open: serde_json::Value = serde_json::from_str(&open).expect("open json");
        let token = open["token"].as_str().expect("token").to_string();

        // A traversal path is rejected and nothing is written above the folder.
        let escape = mutation_canvas_write(
            &mem,
            &options,
            &serde_json::json!({ "token": token, "path": "../escape.html", "content": "x" })
                .to_string(),
        );
        assert!(escape.status >= 400);
        assert!(!site.parent().unwrap().join("escape.html").exists());

        // An unknown token is rejected outright.
        let bogus = mutation_canvas_write(
            &mem,
            &options,
            &serde_json::json!({ "token": "deadbeef", "path": "index.html", "content": "x" })
                .to_string(),
        );
        assert!(bogus.status >= 400);
    }

    #[test]
    fn canvas_open_refuses_folders_outside_the_canvas_root() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::fs::write(outside.path().join("index.html"), "<h1>outside</h1>").unwrap();

        let response = mutation_canvas_open(
            &mem,
            &options,
            &serde_json::json!({ "folder": outside.path().to_string_lossy() }).to_string(),
        );

        assert_eq!(response.status, 403);
        assert!(options.preview_dirs.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn canvas_write_and_pwa_refuse_final_symlink_targets() {
        use std::os::unix::fs::symlink;

        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let site = canvas_project(&mem, "symlink-write");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let outside_index = outside.path().join("outside-index.html");
        std::fs::write(&outside_index, "original").unwrap();
        symlink(&outside_index, site.join("index.html")).unwrap();

        let open = body(&mutation_canvas_open(
            &mem,
            &options,
            &serde_json::json!({ "folder": site.to_string_lossy() }).to_string(),
        ));
        let token = serde_json::from_str::<serde_json::Value>(&open).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();

        let write = mutation_canvas_write(
            &mem,
            &options,
            &serde_json::json!({ "token": token, "path": "index.html", "content": "changed" })
                .to_string(),
        );
        assert_eq!(write.status, 403);
        assert_eq!(std::fs::read_to_string(&outside_index).unwrap(), "original");

        let pwa = mutation_canvas_pwa(&mem, &options, &serde_json::json!({ "token": token }).to_string());
        assert!(pwa.status >= 400);
        assert_eq!(std::fs::read_to_string(&outside_index).unwrap(), "original");
    }

    #[test]
    fn pairing_wizard_round_trip_between_two_devices() {
        let (_dir_a, mem_a) = store();
        let (_dir_b, mem_b) = store();
        let opts = GuiOptions::default();
        let json = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();

        // Device A mints a one-use offer (auto-creating its network).
        let offer_v = json(&body(&handle_mutation(
            &mem_a,
            &opts,
            "/api/network/pair/offer",
            "{}",
        )));
        assert_eq!(offer_v["ok"], true, "{offer_v}");
        let offer = offer_v["offer"].clone();

        // Device B proves possession → response + safety phrase.
        let respond = json(&body(&handle_mutation(
            &mem_b,
            &opts,
            "/api/network/pair/respond",
            &serde_json::json!({ "offer": offer }).to_string(),
        )));
        assert_eq!(respond["ok"], true, "{respond}");
        let response = respond["response"].clone();
        let phrase_b = respond["phrase"].as_str().unwrap().to_string();
        assert!(!phrase_b.is_empty());

        // Device A derives the same phrase from offer + response (the SAS check).
        let phrase = json(&body(&handle_mutation(
            &mem_a,
            &opts,
            "/api/network/pair/phrase",
            &serde_json::json!({ "offer": offer, "response": response }).to_string(),
        )));
        assert_eq!(
            phrase["phrase"].as_str().unwrap(),
            phrase_b,
            "both devices must show the same phrase"
        );

        // Device A approves a full-access grant for B's device.
        let grant_v = json(&body(&handle_mutation(
            &mem_a,
            &opts,
            "/api/network/pair/approve",
            &serde_json::json!({ "response": response, "scope": "full" }).to_string(),
        )));
        assert_eq!(grant_v["ok"], true, "{grant_v}");

        // Device B accepts → becomes a scoped member.
        let accept = json(&body(&handle_mutation(
            &mem_b,
            &opts,
            "/api/network/pair/accept",
            &serde_json::json!({ "grant": grant_v["grant"].clone() }).to_string(),
        )));
        assert_eq!(accept["ok"], true, "{accept}");
        assert!(accept["capabilities"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn publish_refuses_a_folder_that_is_not_a_website() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();

        // A non-web project (no index.html) is rejected with a clear message — not published.
        let proj = tempfile::tempdir().expect("proj tempdir");
        std::fs::write(proj.path().join("main.rs"), "fn main(){}").expect("write source");
        let refused = handle_mutation(
            &mem,
            &options,
            "/api/site/deploy-plan",
            &serde_json::json!({
                "name": "myapp",
                "folder": proj.path().to_string_lossy(),
                "platform": "ipfs",
            })
            .to_string(),
        );
        assert_eq!(refused.status, 400);
        assert!(body(&refused).contains("Only websites"));

        // Adding an index.html clears the gate (it gets past the web-entry check).
        std::fs::write(proj.path().join("index.html"), "<h1>hi</h1>").expect("write index");
        let allowed = handle_mutation(
            &mem,
            &options,
            "/api/site/deploy-plan",
            &serde_json::json!({
                "name": "myapp",
                "folder": proj.path().to_string_lossy(),
                "platform": "ipfs",
            })
            .to_string(),
        );
        assert!(!body(&allowed).contains("Only websites"));
    }

    #[test]
    fn canvas_draft_returns_the_ai_staged_folder_for_prefill() {
        let (_dir, mem) = store();
        // No draft yet → null folder.
        let empty = body(&canvas_draft_get(&mem));
        let empty: serde_json::Value = serde_json::from_str(&empty).expect("draft json");
        assert!(empty["folder"].is_null());

        // The MCP write tools stage into <store>/canvas/draft/ — the bridge surfaces it.
        let draft = mem
            .store_dir()
            .expect("store dir")
            .join("canvas")
            .join("draft");
        std::fs::create_dir_all(&draft).expect("draft dir");
        std::fs::write(draft.join("index.html"), "<h1>ai</h1>").expect("write draft");
        let staged = body(&canvas_draft_get(&mem));
        let staged: serde_json::Value = serde_json::from_str(&staged).expect("draft json");
        let canonical_draft = draft.canonicalize().expect("canonical draft");
        assert_eq!(
            staged["folder"].as_str(),
            Some(canonical_draft.to_string_lossy().as_ref())
        );
        assert_eq!(staged["html"].as_str(), Some("<h1>ai</h1>"));
    }

    #[test]
    fn remote_canvas_and_cards_require_the_approved_transport_peer() {
        let (_dir, mem) = store();
        let identity = Identity::generate();
        let agent_id = identity.agent_id().0;
        let peer_id = peer_id_from_ed25519_hex(&agent_id)
            .expect("peer id")
            .to_string();
        assert!(!approved_agent_matches_peer(&mem, &agent_id, &peer_id));
        mem.add_contact(&agent_id).expect("approve contact");
        assert!(approved_agent_matches_peer(&mem, &agent_id, &peer_id));
        assert!(!approved_agent_matches_peer(&mem, &agent_id, "forged-peer"));

        let mut card = ContactCard::new(&identity.agent_id(), "approved", 1).expect("card");
        card.sign(&identity);
        let card_json = serde_json::to_string(&card).expect("card json");
        assert_eq!(
            approved_contact_card_author(&mem, &card_json, &peer_id),
            Some(agent_id)
        );
        assert!(approved_contact_card_author(&mem, &card_json, "forged-peer").is_none());
    }

    #[test]
    fn canvas_signaling_and_discovery_registries_are_bounded() {
        let mut canvas = HashMap::new();
        for index in 0..(MAX_CANVAS_SESSIONS + 1) {
            let accepted = queue_canvas_signal(
                &mut canvas,
                serde_json::json!({ "session": format!("session-{index}"), "from": "a", "to": "b" }),
            );
            assert_eq!(accepted, index < MAX_CANVAS_SESSIONS);
        }
        for index in 0..(MAX_CANVAS_SIGNAL_QUEUE + 10) {
            assert!(queue_canvas_signal(
                &mut canvas,
                serde_json::json!({ "session": "session-0", "from": "a", "to": "b", "index": index }),
            ));
        }
        assert_eq!(canvas["session-0"].len(), MAX_CANVAS_SIGNAL_QUEUE);

        let now = 1_000;
        let mut peers = std::collections::BTreeMap::new();
        for index in 0..(MAX_DISCOVERY_PEERS + 20) {
            peers.insert(
                format!("peer-{index:04}"),
                PeerInfo {
                    peer_id: format!("peer-{index:04}"),
                    status: "discovered",
                    source: "test".to_string(),
                    relayed: false,
                    addresses: Vec::new(),
                    last_seen: now,
                },
            );
        }
        prune_discovery_peers(&mut peers, now);
        assert_eq!(peers.len(), MAX_DISCOVERY_PEERS);
    }

    #[test]
    fn oversized_headers_receive_431_without_unbounded_reads() {
        let (_dir, mem) = store();
        let options = GuiOptions::default();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            serve_connection(&mem, &options, stream).expect("serve");
        });
        let mut client = TcpStream::connect(addr).expect("connect");
        let request = format!(
            "GET / HTTP/1.1\r\nX-Large: {}\r\n\r\n",
            "x".repeat(MAX_HEADER_BYTES)
        );
        client.write_all(request.as_bytes()).expect("write");
        let mut response = String::new();
        client.read_to_string(&mut response).expect("read");
        server.join().expect("join");
        assert!(response.starts_with("HTTP/1.1 431 Request Header Fields Too Large\r\n"));
    }

    // ---- Phase D: loopback gate + privacy mutations --------------------------

    fn options_with_csrf(token: &str) -> GuiOptions {
        GuiOptions {
            csrf_token: token.to_string(),
            ..GuiOptions::default()
        }
    }

    #[test]
    fn window_heartbeat_and_closing_track_presence_and_bypass_the_rate_limiter() {
        let (_dir, mem) = store();
        let options = options_with_csrf("tok");
        let beat = |id: &str, path: &str| {
            post(
                path,
                &format!("{{\"id\":\"{id}\"}}"),
                Some("127.0.0.1:4173"),
                Some("http://127.0.0.1:4173"),
                Some("tok"),
            )
        };

        // A heartbeat is accepted and records the window as present.
        let res = route_request(&mem, &options, &beat("win-a", "/api/heartbeat"));
        assert_eq!(res.status, 200);
        {
            let presence = options.clients.lock().unwrap();
            assert!(presence.seen_any);
            assert!(presence.last_seen.contains_key("win-a"));
        }

        // Heartbeats are NOT rate-limited (the page pings every few seconds): far more than
        // MUTATION_RATE_MAX in a row all succeed, where a normal mutation would 429.
        for _ in 0..(MUTATION_RATE_MAX + 5) {
            assert_eq!(
                route_request(&mem, &options, &beat("win-a", "/api/heartbeat")).status,
                200
            );
        }

        // A second window is tracked independently; closing one leaves the other present.
        route_request(&mem, &options, &beat("win-b", "/api/heartbeat"));
        route_request(&mem, &options, &beat("win-a", "/api/closing"));
        {
            let presence = options.clients.lock().unwrap();
            assert!(!presence.last_seen.contains_key("win-a"));
            assert!(presence.last_seen.contains_key("win-b"));
        }

        // Closing the last window empties the set — the watchdog's signal to shut down.
        route_request(&mem, &options, &beat("win-b", "/api/closing"));
        assert!(options.clients.lock().unwrap().last_seen.is_empty());

        // A missing id is rejected, not silently tracked.
        let bad = route_request(
            &mem,
            &options,
            &post(
                "/api/heartbeat",
                "{}",
                Some("127.0.0.1:4173"),
                Some("http://127.0.0.1:4173"),
                Some("tok"),
            ),
        );
        assert_eq!(bad.status, 400);
    }

    fn post(
        path: &str,
        body: &str,
        host: Option<&str>,
        origin: Option<&str>,
        csrf: Option<&str>,
    ) -> ParsedRequest {
        let mut headers = HashMap::new();
        if let Some(host) = host {
            headers.insert("host".to_string(), host.to_string());
        }
        if let Some(origin) = origin {
            headers.insert("origin".to_string(), origin.to_string());
        }
        if let Some(csrf) = csrf {
            headers.insert("x-csrf-token".to_string(), csrf.to_string());
        }
        headers.insert("content-type".to_string(), "application/json".to_string());
        ParsedRequest {
            method: "POST".to_string(),
            target: path.to_string(),
            headers,
            body: body.to_string(),
        }
    }

    #[test]
    fn semantic_search_returns_ranked_hits_for_a_query() {
        let (_dir, mem) = store();
        let rustdoc = put_named(
            &mem,
            "rustdoc",
            "the rust borrow checker enforces ownership and lifetimes",
        );
        put_named(
            &mem,
            "cooking",
            "sourdough fermentation needs a live starter and time",
        );
        let body = body(&handle(
            &mem,
            "/api/search",
            "q=rust%20ownership&budget=2000&depth=summary",
        ));
        assert!(body.contains("\"indexed\":"), "reports index size");
        assert!(body.contains("\"items\":"), "returns a ranked item list");
        assert!(
            body.contains(&rustdoc.0),
            "the rust node is retrieved for a rust query: {body}"
        );
    }

    #[test]
    fn system_console_activity_feed_records_what_the_concierge_does() {
        let (_dir, mem) = store();
        put_named(
            &mem,
            "rustdoc",
            "the rust borrow checker enforces ownership and lifetimes",
        );
        let options = GuiOptions::default();

        // Before any work the feed is empty, but the embedder is always reported
        // (declared, not yet loaded) so the console can show the model immediately.
        let initial = body(&handle_with_options(&mem, &options, "/api/activity", ""));
        assert!(
            initial.contains("\"embedder\":"),
            "always reports the embedder: {initial}"
        );
        assert!(
            initial.contains("\"built\":false"),
            "no model loaded until the first search: {initial}"
        );

        // A search loads the embedder, indexes, and retrieves — each is surfaced.
        let _ = handle_with_options(
            &mem,
            &options,
            "/api/search",
            "q=rust%20ownership&budget=2000",
        );
        let after = body(&handle_with_options(&mem, &options, "/api/activity", ""));
        assert!(
            after.contains("embedder ready"),
            "embedder load shown: {after}"
        );
        assert!(after.contains("indexed"), "indexing shown: {after}");
        assert!(after.contains("retrieve"), "retrieval shown: {after}");
        assert!(
            after.contains("\"built\":true"),
            "embedder now reports as loaded: {after}"
        );

        // Incremental polling: ?since=<next_seq> returns only newer lines.
        let parsed: serde_json::Value = serde_json::from_str(&after).unwrap();
        let next_seq = parsed["next_seq"].as_u64().unwrap();
        let tail = body(&handle_with_options(
            &mem,
            &options,
            "/api/activity",
            &format!("since={next_seq}"),
        ));
        let tail_parsed: serde_json::Value = serde_json::from_str(&tail).unwrap();
        assert_eq!(
            tail_parsed["entries"].as_array().unwrap().len(),
            0,
            "no new activity since the last poll: {tail}"
        );
    }

    #[test]
    fn semantic_search_requires_a_query() {
        let (_dir, mem) = store();
        assert_eq!(handle(&mem, "/api/search", "q=").status, 400);
        assert_eq!(handle(&mem, "/api/search", "").status, 400);
    }

    #[test]
    fn messenger_lists_and_revokes_approved_peers() {
        let (_dir, mem) = store();
        let peer = "ab".repeat(32); // a 64-hex username (AgentID)
        mem.add_contact(&peer).unwrap();

        let listed = body(&handle(&mem, "/api/contacts", ""));
        assert!(
            listed.contains(&peer),
            "the approved peer is listed: {listed}"
        );
        assert!(
            listed.contains("\"room\""),
            "each peer carries its 1:1 thread id"
        );

        // Revoke through the same loopback mutation gate the UI uses.
        let options = options_with_csrf("tok");
        let remove = route_request(
            &mem,
            &options,
            &post(
                "/api/contacts/remove",
                &format!("{{\"username\":\"{peer}\"}}"),
                Some("127.0.0.1"),
                Some("http://127.0.0.1"),
                Some("tok"),
            ),
        );
        assert_eq!(remove.status, 200, "{}", body(&remove));
        assert!(body(&remove).contains("\"removed\":true"));

        let after = body(&handle(&mem, "/api/contacts", ""));
        assert!(
            !after.contains(&peer),
            "peer is gone after removal: {after}"
        );
    }

    #[test]
    fn claude_code_capture_is_opt_in_and_toggles_on_attach() {
        let (_dir, mem) = store();
        // Phase C: capture is consent-gated — off until the user attaches.
        assert!(!claude_code_attached(&mem));
        let status = body(&handle(&mem, "/api/claude-code/status", ""));
        assert!(status.contains("\"attached\":false"));

        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");

        let attach = route_request(
            &mem,
            &options,
            &post("/api/claude-code/attach", "{}", local, origin, Some("tok")),
        );
        assert_eq!(attach.status, 200, "{}", body(&attach));
        assert!(body(&attach).contains("\"attached\":true"));
        assert!(claude_code_attached(&mem), "consent persisted to the store");
        assert!(body(&handle(&mem, "/api/claude-code/status", "")).contains("\"attached\":true"));

        // Detaching is the safe direction; no password needed.
        let detach = route_request(
            &mem,
            &options,
            &post("/api/claude-code/detach", "{}", local, origin, Some("tok")),
        );
        assert!(body(&detach).contains("\"attached\":false"));
        assert!(!claude_code_attached(&mem));
    }

    #[test]
    fn capture_offsets_persist_across_relaunch() {
        let (_dir, mem) = store();
        // No file yet → empty.
        assert!(load_capture_offsets(&mem).is_empty());
        // Save a couple of offsets, then load them back (simulating a relaunch).
        let mut offsets = std::collections::HashMap::new();
        offsets.insert(std::path::PathBuf::from("/p/a.jsonl"), 128u64);
        offsets.insert(std::path::PathBuf::from("/p/b.jsonl"), 4096u64);
        save_capture_offsets(&mem, &offsets);
        let reloaded = load_capture_offsets(&mem);
        assert_eq!(reloaded.get(std::path::Path::new("/p/a.jsonl")), Some(&128));
        assert_eq!(
            reloaded.get(std::path::Path::new("/p/b.jsonl")),
            Some(&4096)
        );
    }

    #[test]
    fn status_reports_current_project_session_count() {
        let (_dir, mem) = store();
        // The field is always present so the banner can foreground this project.
        let status = body(&handle(&mem, "/api/claude-code/status", ""));
        assert!(status.contains("\"current_project_sessions\":"));
        assert!(status.contains("\"session_count\":"));
    }

    #[test]
    fn claude_code_attach_requires_csrf_like_every_mutation() {
        let (_dir, mem) = store();
        let options = options_with_csrf("tok");
        let no_token = route_request(
            &mem,
            &options,
            &post(
                "/api/claude-code/attach",
                "{}",
                Some("127.0.0.1"),
                Some("http://127.0.0.1"),
                None,
            ),
        );
        assert_eq!(no_token.status, 403, "attach must be CSRF-gated");
        assert!(!claude_code_attached(&mem));
    }

    #[test]
    fn loopback_gate_blocks_cross_site_missing_csrf_and_bad_host() {
        let (_dir, mem) = store();
        put_named(&mem, "latest", "lock me");
        mem.set_password("pw").expect("password");
        let options = options_with_csrf("tok");
        let body = r#"{"target":"latest"}"#;
        let local = Some("127.0.0.1:4173");
        let local_origin = Some("http://127.0.0.1:4173");

        // A fully valid same-origin request with the CSRF token locks the root.
        let ok = route_request(
            &mem,
            &options,
            &post("/api/lock", body, local, local_origin, Some("tok")),
        );
        assert_eq!(ok.status, 200, "valid same-origin POST should lock");

        // Each missing/forged credential is forbidden.
        let no_csrf = route_request(
            &mem,
            &options,
            &post("/api/lock", body, local, local_origin, None),
        );
        let bad_csrf = route_request(
            &mem,
            &options,
            &post("/api/lock", body, local, local_origin, Some("nope")),
        );
        let cross_origin = route_request(
            &mem,
            &options,
            &post(
                "/api/lock",
                body,
                local,
                Some("http://evil.example"),
                Some("tok"),
            ),
        );
        let rebinding_host = route_request(
            &mem,
            &options,
            &post(
                "/api/lock",
                body,
                Some("evil.example"),
                local_origin,
                Some("tok"),
            ),
        );
        for blocked in [&no_csrf, &bad_csrf, &cross_origin, &rebinding_host] {
            assert_eq!(blocked.status, 403, "credential check must forbid");
        }
    }

    #[test]
    fn get_cannot_reach_a_mutation_route_and_rebinding_host_is_forbidden() {
        let (_dir, mem) = store();
        let options = options_with_csrf("tok");
        // GET on a mutation path is simply not a route (read router owns GET).
        let mut get_lock = post("/api/lock", "", Some("127.0.0.1"), None, None);
        get_lock.method = "GET".to_string();
        assert_eq!(route_request(&mem, &options, &get_lock).status, 404);

        // A read with a non-loopback Host (DNS rebinding) is forbidden.
        let mut rebinding = post("/", "", Some("attacker.example"), None, None);
        rebinding.method = "GET".to_string();
        assert_eq!(route_request(&mem, &options, &rebinding).status, 403);

        let mut missing_host = post("/", "", None, None, None);
        missing_host.method = "GET".to_string();
        assert_eq!(route_request(&mem, &options, &missing_host).status, 403);
    }

    #[test]
    fn mutations_are_disabled_when_no_csrf_token_is_configured() {
        let (_dir, mem) = store();
        put_named(&mem, "latest", "x");
        // Default options have an empty token => every POST is refused.
        let options = GuiOptions::default();
        let request = post(
            "/api/lock",
            r#"{"target":"latest"}"#,
            Some("127.0.0.1"),
            Some("http://127.0.0.1"),
            Some(""),
        );
        assert_eq!(route_request(&mem, &options, &request).status, 403);
    }

    #[test]
    fn password_is_never_echoed_by_mutation_responses() {
        let (_dir, mem) = store();
        let fenced = put_named(&mem, "fenced", "x");
        mem.lock_subgraph(&fenced, "fence").expect("lock");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");
        let secret = "hunter2-very-secret";

        let set = route_request(
            &mem,
            &options,
            &post(
                "/api/set-password",
                &format!(r#"{{"password":"{secret}","confirm_password":"{secret}"}}"#),
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(set.status, 200);
        assert!(
            !body(&set).contains(secret),
            "set-password must not echo the password"
        );

        // A wrong-password egress-unlock attempt fails 401 and never reflects the input.
        let wrong = route_request(
            &mem,
            &options,
            &post(
                "/api/unlock",
                r#"{"target":"fenced","password":"WRONG"}"#,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(wrong.status, 401);
        assert!(!body(&wrong).contains("WRONG"));
    }

    #[test]
    fn password_confirmation_and_first_gui_lock_fail_closed() {
        let (_dir, mem) = store();
        put_named(&mem, "latest", "lock me");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");

        let premature_lock = route_request(
            &mem,
            &options,
            &post(
                "/api/lock",
                r#"{"target":"latest"}"#,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(premature_lock.status, 400);
        assert!(mem.locks().expect("locks").is_empty());

        let mismatch = route_request(
            &mem,
            &options,
            &post(
                "/api/set-password",
                r#"{"password":"one","confirm_password":"two"}"#,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(mismatch.status, 400);
        assert!(!mem.password_is_set().expect("password state"));
    }

    #[test]
    fn authorize_publish_requires_acknowledgement_then_password() {
        let (_dir, mem) = store();
        let root = put_named(&mem, "secret", "classified");
        mem.lock_subgraph(&root, "private").expect("lock");
        mem.set_password("pw").expect("password");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");
        let plan = mem
            .build_egress_plan_for_target("secret", EgressOperation::PublicPublish)
            .unwrap();
        let review_token = options.cache_review(plan.clone()).expect("cache review");

        // No acknowledgement => 400 before any password handling.
        let no_ack_body = serde_json::json!({
            "review_token": &review_token,
            "password": "pw",
        })
        .to_string();
        let no_ack = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &no_ack_body,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(no_ack.status, 400);

        // Acknowledged but wrong password => 401, no grant minted, still blocked.
        let wrong_body = serde_json::json!({
            "review_token": &review_token,
            "password": "WRONG",
            "acknowledge_irreversible": true,
        })
        .to_string();
        let wrong_pw = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &wrong_body,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(wrong_pw.status, 401);
        // No grant was minted by the failed attempt: the root is still blocked.
        let plan = mem
            .build_egress_plan_for_target("secret", EgressOperation::PublicPublish)
            .unwrap();
        assert!(matches!(
            mem.publish_public(&plan),
            Err(Error::PublicationBlocked { .. })
        ));
    }

    #[test]
    fn the_fence_badges_a_subgraph_for_egress_without_hiding_local_view() {
        let (_dir, mem) = store();
        let content = mem
            .put_blob(b"hidden-body-value", "text/plain")
            .expect("put blob");
        let secret = mem
            .put_node(&Node {
                kind: "file_ref".to_string(),
                fields_json: serde_json::json!({
                    "path": "docs/notes.txt",
                    "size": 17,
                    "content": cid_link(&content).expect("content link"),
                })
                .to_string(),
            })
            .expect("put");
        mem.bind("secret", &secret).expect("bind");
        let checkpoint = mem
            .checkpoint("private", &secret, None)
            .expect("checkpoint");
        mem.bind("latest", &checkpoint).expect("bind checkpoint");

        let locked_record = body(&handle(&mem, "/api/record", &format!("cid={}", secret.0)));
        let locked_graph = body(&handle(
            &mem,
            "/api/graph",
            &format!("cid={}", checkpoint.0),
        ));
        let privacy = body(&handle(&mem, "/api/privacy", &format!("cid={}", secret.0)));
        // Content is fully visible locally — the fence guards egress, not viewing …
        assert!(locked_record.contains("docs/notes.txt"));
        assert!(locked_graph.contains("docs/notes.txt"));
        // … and surfaces only as a fence badge (the default under Decision 0026).
        assert!(locked_record.contains("\"locked\":true"));
        assert!(locked_graph.contains("\"fenced\":true"));
        assert!(locked_graph.contains("\"cleared\":false"));
        // The egress-side privacy summary still reports what is fenced from export.
        assert!(privacy.contains("\"fenced\":true"));
        assert!(privacy.contains("\"reachable_node_count\":2"));
        assert!(privacy.contains("\"file_count\":1"));
        assert!(privacy.contains("\"blocked_file_count\":1"));
    }

    #[test]
    fn locked_room_messages_stay_visible_locally_with_an_egress_badge() {
        let (_dir, mem) = store();
        let cid = mem
            .post_message("private-room", "hidden-message-body")
            .expect("post");
        mem.lock_subgraph(&cid, "private room").expect("lock");
        let thread = body(&handle(&mem, "/api/thread", "room=private-room"));
        // The body is shown locally — the lock only fences it from egress …
        assert!(thread.contains("hidden-message-body"));
        // … and surfaces as a lock badge on the message.
        assert!(thread.contains("\"locked\":true"));
    }

    #[test]
    fn gui_publishes_clear_and_locked_exact_reviewed_plans() {
        let (dir, mem) = store();
        mem.set_password("pw").expect("password");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");

        let clear = put_named(&mem, "clear", "public body");
        let clear_backend = configure_fake_ipfs_backend(&mem, dir.path(), 1);
        let clear_plan = mem
            .build_egress_plan_for_target("clear", EgressOperation::PublicPublish)
            .expect("clear plan");
        let clear_token = options
            .cache_review(clear_plan.clone())
            .expect("cache clear review");
        let clear_body = serde_json::json!({
            "review_token": clear_token,
            "password": "pw",
            "acknowledge_irreversible": true,
        })
        .to_string();
        let clear_response = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &clear_body,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(clear_response.status, 200, "{}", body(&clear_response));
        clear_backend.join().expect("clear backend");

        let locked = put_named(&mem, "locked", "locked body");
        mem.lock_subgraph(&locked, "private").expect("lock");
        let locked_backend = configure_fake_ipfs_backend(&mem, dir.path(), 1);
        let locked_plan = mem
            .build_egress_plan_for_target("locked", EgressOperation::PublicPublish)
            .expect("locked plan");
        let locked_token = options
            .cache_review(locked_plan.clone())
            .expect("cache locked review");
        let locked_body = serde_json::json!({
            "review_token": locked_token,
            "password": "pw",
            "acknowledge_irreversible": true,
        })
        .to_string();
        let locked_response = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &locked_body,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(locked_response.status, 200, "{}", body(&locked_response));
        assert!(body(&locked_response).contains("\"authorization_consumed\":true"));
        locked_backend.join().expect("locked backend");

        let privacy = body(&handle(&mem, "/api/privacy", &format!("cid={}", locked.0)));
        assert!(privacy.contains("\"known_public\":true"));
        let graph = body(&handle(&mem, "/api/graph", &format!("cid={}", locked.0)));
        assert!(graph.contains("\"known_public\":true"));
        let current = mem
            .build_egress_plan_for_target("locked", EgressOperation::PublicPublish)
            .expect("current locked plan");
        assert!(matches!(
            mem.publish_public(&current),
            Err(Error::PublicationBlocked { .. })
        ));
        assert_ne!(clear, locked);
    }

    #[test]
    fn authorize_publish_rejects_a_modified_reviewed_plan() {
        let (_dir, mem) = store();
        let root = put_named(&mem, "secret", "classified");
        mem.set_password("pw").expect("password");
        mem.lock_subgraph(&root, "private").expect("lock");
        let mut reviewed = mem
            .build_egress_plan_for_target("secret", EgressOperation::PublicPublish)
            .expect("plan");
        reviewed.byte_size += 1;
        let options = options_with_csrf("tok");
        let review_token = options
            .cache_review(reviewed)
            .expect("cache modified review");
        let request_body = serde_json::json!({
            "review_token": review_token,
            "password": "pw",
            "acknowledge_irreversible": true,
        })
        .to_string();
        let response = route_request(
            &mem,
            &options,
            &post(
                "/api/authorize-publish",
                &request_body,
                Some("127.0.0.1"),
                Some("http://127.0.0.1"),
                Some("tok"),
            ),
        );
        assert_eq!(response.status, 409);
    }

    #[test]
    fn loopback_gate_requires_host_matching_origin_json_and_rate_limits() {
        let (_dir, mem) = store();
        let options = options_with_csrf("tok");
        let valid = post(
            "/api/nope",
            "{}",
            Some("127.0.0.1:4173"),
            Some("http://127.0.0.1:4173"),
            Some("tok"),
        );

        let missing_host = post(
            "/api/nope",
            "{}",
            None,
            Some("http://127.0.0.1:4173"),
            Some("tok"),
        );
        assert_eq!(route_request(&mem, &options, &missing_host).status, 403);

        let mismatched_origin = post(
            "/api/nope",
            "{}",
            Some("127.0.0.1:4173"),
            Some("http://127.0.0.1:4174"),
            Some("tok"),
        );
        assert_eq!(
            route_request(&mem, &options, &mismatched_origin).status,
            403
        );

        let mut wrong_type = valid;
        wrong_type
            .headers
            .insert("content-type".to_string(), "text/plain".to_string());
        assert_eq!(route_request(&mem, &options, &wrong_type).status, 415);

        for _ in 0..MUTATION_RATE_MAX {
            assert_eq!(
                route_request(
                    &mem,
                    &options,
                    &post(
                        "/api/nope",
                        "{}",
                        Some("127.0.0.1:4173"),
                        Some("http://127.0.0.1:4173"),
                        Some("tok"),
                    ),
                )
                .status,
                404
            );
        }
        assert_eq!(
            route_request(
                &mem,
                &options,
                &post(
                    "/api/nope",
                    "{}",
                    Some("127.0.0.1:4173"),
                    Some("http://127.0.0.1:4173"),
                    Some("tok"),
                ),
            )
            .status,
            429
        );
    }

    #[test]
    fn browser_shell_contains_phase_d_secret_and_state_safeguards() {
        let (_dir, mem) = store();
        let page = ["/", "/app.js", "/wallet.js", "/studio.js"]
            .into_iter()
            .map(|path| body(&handle(&mem, path, "")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!page.contains(r#"autocomplete = "off""#));
        assert!(page.contains(r#"autocomplete = "new-password""#));
        assert!(page.contains(r#"autocomplete = "current-password""#));
        assert!(page.contains("finally { input.value = \"\"; }"));
        assert!(page.contains("review_token: plan.review_token"));
        assert!(page.contains("Exact CID manifest"));
        assert!(page.contains("cleared-root"));
        assert!(page.contains("partial-cleared"));
        assert!(page.contains("known-public"));
    }

    #[test]
    fn meta_exposes_a_csrf_token_for_the_page() {
        let (_dir, mem) = store();
        let options = options_with_csrf("page-token");
        let meta = body(&handle_with_options(&mem, &options, "/api/meta", ""));
        assert!(meta.contains("page-token"));
        assert!(meta.contains("\"password_set\""));
    }

    #[test]
    fn app_starts_window_lifecycle_after_loading_csrf_token() {
        assert!(APP_JS.contains("csrfToken = meta.csrf_token || \"\";\n  startLifecycle();"));
    }

    #[test]
    fn convert_private_is_gated_password_protected_and_surfaces_in_privacy() {
        let (_dir, mem) = store();
        put_named(&mem, "latest", "secret content");
        let options = options_with_csrf("tok");
        let local = Some("127.0.0.1");
        let origin = Some("http://127.0.0.1");

        let set = route_request(
            &mem,
            &options,
            &post(
                "/api/set-password",
                r#"{"password":"pw","confirm_password":"pw"}"#,
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(set.status, 200);

        let review = handle_with_options(
            &mem,
            &options,
            "/api/egress-plan",
            "op=private&name=latest&namespace=team%3Awetlands&recipients=agent-recipient",
        );
        assert_eq!(review.status, 200);
        let review: serde_json::Value = serde_json::from_slice(&review.body).unwrap();
        let review_token = review["review_token"].as_str().unwrap();

        // Missing CSRF is forbidden by the gate (never reaches the handler).
        let no_csrf = route_request(
            &mem,
            &options,
            &post(
                "/api/convert-private",
                &serde_json::json!({
                    "review_token": review_token,
                    "password": "pw",
                    "acknowledge_private": true,
                })
                .to_string(),
                local,
                origin,
                None,
            ),
        );
        assert_eq!(no_csrf.status, 403);

        // Destination and recipient review must be explicitly acknowledged.
        let no_acknowledgement = route_request(
            &mem,
            &options,
            &post(
                "/api/convert-private",
                &serde_json::json!({
                    "review_token": review_token,
                    "password": "pw",
                })
                .to_string(),
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(no_acknowledgement.status, 400);

        // Wrong password is rejected (authentication failed).
        let wrong = route_request(
            &mem,
            &options,
            &post(
                "/api/convert-private",
                &serde_json::json!({
                    "review_token": review_token,
                    "password": "WRONG",
                    "acknowledge_private": true,
                })
                .to_string(),
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(wrong.status, 401);

        // A valid request converts and the privacy endpoint then shows the copy.
        let ok = route_request(
            &mem,
            &options,
            &post(
                "/api/convert-private",
                &serde_json::json!({
                    "review_token": review_token,
                    "password": "pw",
                    "acknowledge_private": true,
                })
                .to_string(),
                local,
                origin,
                Some("tok"),
            ),
        );
        assert_eq!(ok.status, 200);
        assert!(body(&ok).contains("ciphertext_root"));
        assert!(body(&ok).contains("\"capability\""));

        let privacy = body(&handle(&mem, "/api/privacy", "name=latest"));
        assert!(privacy.contains("encrypted_private_copy"));
        assert!(privacy.contains("\"baf"));
        assert!(!privacy.contains("read_key"));
        let graph = body(&handle(&mem, "/api/graph", "name=latest"));
        assert!(graph.contains("\"encrypted_private\":true"));
    }

    #[test]
    fn no_running_gui_means_no_reuse() {
        let (_dir, mem) = store();
        assert!(running_gui_port(&mem).is_none());
    }

    #[test]
    fn stale_lockfile_does_not_match_a_dead_server() {
        let (_dir, mem) = store();
        // A lockfile pointing at a port nothing serves must not be reused.
        let path = mem.store_dir().unwrap().join("gui.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"pid":999999,"port":59123}"#).unwrap();
        assert!(running_gui_port(&mem).is_none());
    }

    #[test]
    fn pick_free_port_returns_a_bindable_port() {
        let port = pick_free_port(48910);
        // Whatever it returns must actually be bindable now.
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }

    #[test]
    fn wallet_browser_detection_is_callable_and_any_path_is_real() {
        // Environment-dependent: just prove it can't panic and never returns a
        // non-existent path (the shell launcher relies on that), for both browsers.
        for path in [brave_path(), opera_path()].into_iter().flatten() {
            assert!(path.exists(), "a detected browser path must exist");
        }
        if let Some((kind, path)) = wallet_browser() {
            assert!(path.exists());
            assert!(matches!(kind, WalletBrowser::Brave | WalletBrowser::Opera));
        }
    }
}
