use super::*;
use concierge_core::{cid_link, Node, YaraScanner};
use concierge_net::content_message_id;

/// A one-shot callback that completes an OAuth exchange from the returned code.
type OAuthFinish = Box<dyn FnOnce(&str) -> Result<String, String> + Send>;

pub(super) fn handle_mutation(
    mem: &MemCli,
    options: &GuiOptions,
    path: &str,
    body: &str,
) -> Response {
    let response = match path {
        "/api/ingest" => mutation_ingest(mem, options, body),
        "/api/ingest-path" => mutation_ingest_path(mem, body),
        "/api/lock" => mutation_lock(mem, body),
        "/api/unlock" => mutation_unlock(mem, body),
        "/api/clear-for-egress" => mutation_clear_for_egress(mem, body),
        "/api/refence" => mutation_refence(mem, body),
        "/api/claude-code/attach" => mutation_claude_code_attach(mem, true),
        "/api/claude-code/detach" => mutation_claude_code_attach(mem, false),
        "/api/aider/attach" => mutation_aider_attach(mem, true),
        "/api/aider/detach" => mutation_aider_attach(mem, false),
        "/api/codex/attach" => mutation_codex_attach(mem, true),
        "/api/codex/detach" => mutation_codex_attach(mem, false),
        "/api/gemini/attach" => mutation_gemini_attach(mem, true),
        "/api/gemini/detach" => mutation_gemini_attach(mem, false),
        "/api/continue/attach" => mutation_continue_attach(mem, true),
        "/api/continue/detach" => mutation_continue_attach(mem, false),
        "/api/antigravity/attach" => mutation_antigravity_attach(mem, true),
        "/api/antigravity/detach" => mutation_antigravity_attach(mem, false),
        "/api/openclaw/attach" => mutation_openclaw_attach(mem, true),
        "/api/openclaw/detach" => mutation_openclaw_attach(mem, false),
        "/api/cline/attach" => mutation_cline_attach(mem, true),
        "/api/cline/detach" => mutation_cline_attach(mem, false),
        "/api/cursor/attach" => mutation_cursor_attach(mem, true),
        "/api/cursor/detach" => mutation_cursor_attach(mem, false),
        "/api/opendevin/attach" => mutation_opendevin_attach(mem, true),
        "/api/opendevin/detach" => mutation_opendevin_attach(mem, false),
        "/api/copilot/attach" => mutation_copilot_attach(mem, true),
        "/api/copilot/detach" => mutation_copilot_attach(mem, false),
        "/api/claude-code/ingest" => claude_code_ingest(mem),
        "/api/aider/ingest" => aider_ingest(mem),
        "/api/codex/ingest" => codex_ingest(mem),
        "/api/gemini/ingest" => gemini_ingest(mem),
        "/api/continue/ingest" => continue_ingest(mem),
        "/api/antigravity/ingest" => antigravity_ingest(mem),
        "/api/openclaw/ingest" => openclaw_ingest(mem),
        "/api/cline/ingest" => cline_ingest(mem),
        "/api/cursor/ingest" => cursor_ingest(mem),
        "/api/opendevin/ingest" => opendevin_ingest(mem),
        "/api/copilot/ingest" => copilot_ingest(mem),
        "/api/sidekick/enable" => mutation_sidekick(mem, true),
        "/api/sidekick/disable" => mutation_sidekick(mem, false),
        "/api/update/check" => mutation_update_check(mem),
        "/api/update/apply" => mutation_update_apply(mem),
        "/api/update/rules/refresh" => mutation_update_rules_refresh(mem),
        "/api/update/rules/pause" => mutation_update_rules_pause(mem, body),
        "/api/update/rules/pin" => mutation_update_rules_pin(mem, body),
        "/api/update/rules/source" => mutation_update_rules_source(mem, body),
        "/api/brain/model" => mutation_brain_model(mem, body),
        "/api/set-password" => mutation_set_password(mem, body),
        "/api/authorize-publish" => mutation_authorize_publish(mem, options, body),
        "/api/convert-private" => mutation_convert_private(mem, options, body),
        "/api/message" => mutation_post_message(mem, options, body),
        "/api/thread/delete" => mutation_thread_delete(mem, body),
        "/api/site/deploy-plan" => mutation_site_deploy_plan(mem, options, body),
        "/api/site/publish" => mutation_publish_site(mem, options, body),
        "/api/site/checkpoint/save" => mutation_save_checkpoint(mem, body),
        "/api/deploy/credentials" => mutation_deploy_credentials(mem, body),
        "/api/deploy/test" => mutation_deploy_test(mem, body),
        "/api/deploy/cloudflare/oauth-start" => mutation_cf_oauth_start(mem),
        "/api/deploy/firebase/oauth-start" => mutation_fb_oauth_start(mem),
        "/api/youtube/oauth-start" => mutation_yt_oauth_start(mem),
        "/api/youtube/disconnect" => mutation_yt_disconnect(mem),
        "/api/youtube/upload" => mutation_yt_upload(mem, body),
        "/api/pin/credentials" => mutation_pin_credentials(mem, body),
        "/api/pin/test" => mutation_pin_test(mem, body),
        "/api/site/pin" => mutation_pin_site(mem, body),
        "/api/record/pin" => mutation_pin_record(mem, body),
        "/api/record/unpin" => mutation_unpin_record(mem, body),
        "/api/git/commit" => mutation_git_commit(mem, body),
        "/api/bookmarks/sync" => mutation_bookmarks_sync(mem),
        "/api/wallet/setup" => mutation_wallet_setup(body),
        "/api/wallet/link" => mutation_wallet_link(mem, body),
        "/api/wallet/unlink" => mutation_wallet_unlink(mem, body),
        "/api/wallet/settings" => mutation_wallet_settings(mem, body),
        "/api/wallet/proposals/resolve" => mutation_wallet_resolve(mem, body),
        "/api/mcp/write" => mutation_mcp_write(mem, body),
        "/api/canvas/open" => mutation_canvas_open(mem, options, body),
        "/api/canvas/write" => mutation_canvas_write(mem, options, body),
        "/api/canvas/pwa" => mutation_canvas_pwa(mem, options, body),
        "/api/canvas/new" => mutation_canvas_new(mem, body),
        "/api/canvas/delete" => mutation_canvas_delete(mem, body),
        "/api/blender/connect" => mutation_blender_connect(),
        "/api/canvas/signal" => mutation_canvas_signal(mem, options, body),
        "/api/canvas/snapshot" => mutation_canvas_snapshot(mem, body),
        "/api/requests/accept" => mutation_request_decision(mem, body, true),
        "/api/requests/decline" => mutation_request_decision(mem, body, false),
        "/api/contacts/remove" => mutation_contact_remove(mem, body),
        "/api/petname" => mutation_petname(mem, body),
        "/api/profile" => mutation_profile(mem, body),
        "/api/compact" => mutation_compact(mem, options),
        "/api/network/create" => mutation_network_create(mem, body),
        "/api/network/revoke" => mutation_network_revoke(mem, body),
        "/api/network/rotate" => mutation_network_rotate(mem, body),
        "/api/network/pair/offer" => mutation_pair_offer(mem),
        "/api/network/pair/respond" => mutation_pair_respond(mem, body),
        "/api/network/pair/phrase" => mutation_pair_phrase(body),
        "/api/network/pair/approve" => mutation_pair_approve(mem, body),
        "/api/network/pair/accept" => mutation_pair_accept(mem, body),
        _ => Response::not_found(),
    };
    // Surface every action in the System Console so the user sees what the concierge
    // does. Noisy/secret paths are handled by their own handlers (chat, search) or
    // skipped here (live-canvas WebRTC signalling fires many times a second).
    if let Some(label) = mutation_label(path) {
        if response.status < 400 {
            options.log("ok", label.to_string());
        } else {
            options.log("wn", format!("{label} — declined ({})", response.status));
        }
    }
    response
}

/// A human label for the System Console, or `None` to keep an action off the feed
/// (high-frequency signalling, or paths whose own handler already logs richer detail).
fn mutation_label(path: &str) -> Option<&'static str> {
    Some(match path {
        "/api/ingest" => "ingested host-neutral events",
        "/api/ingest-path" => "ingested a file of events",
        "/api/lock" => "locked a node from egress",
        "/api/unlock" => "unlocked a node",
        "/api/clear-for-egress" => "cleared a node for egress (password-gated)",
        "/api/refence" => "re-fenced a node",
        "/api/claude-code/attach" => "attached the Claude Code adapter",
        "/api/claude-code/detach" => "detached the Claude Code adapter",
        "/api/aider/attach" => "attached the Aider adapter",
        "/api/aider/detach" => "detached the Aider adapter",
        "/api/codex/attach" => "attached the Codex adapter",
        "/api/codex/detach" => "detached the Codex adapter",
        "/api/gemini/attach" => "attached the Gemini adapter",
        "/api/gemini/detach" => "detached the Gemini adapter",
        "/api/continue/attach" => "attached the Continue adapter",
        "/api/continue/detach" => "detached the Continue adapter",
        "/api/antigravity/attach" => "attached the Antigravity adapter",
        "/api/antigravity/detach" => "detached the Antigravity adapter",
        "/api/openclaw/attach" => "attached the OpenClaw adapter",
        "/api/openclaw/detach" => "detached the OpenClaw adapter",
        "/api/cline/attach" => "attached the Cline adapter",
        "/api/cline/detach" => "detached the Cline adapter",
        "/api/cursor/attach" => "attached the Cursor adapter",
        "/api/cursor/detach" => "detached the Cursor adapter",
        "/api/opendevin/attach" => "attached the OpenDevin adapter",
        "/api/opendevin/detach" => "detached the OpenDevin adapter",
        "/api/copilot/attach" => "attached the Copilot adapter",
        "/api/copilot/detach" => "detached the Copilot adapter",
        "/api/sidekick/enable" => "enabling Sidekick (private Kubo node + on-node embedder)",
        "/api/sidekick/disable" => "disabled Sidekick",
        "/api/update/check" => "checked for an app update",
        "/api/update/apply" => "downloaded & staged an app update",
        "/api/update/rules/refresh" => "refreshed the signed safety rules",
        "/api/update/rules/pause" => "toggled the auto-rules kill switch",
        "/api/update/rules/pin" => "pinned a rules publisher key",
        "/api/update/rules/source" => "updated the rules IPNS source",
        "/api/brain/model" => "selected the Brain's active model",
        "/api/set-password" => "set the store password",
        "/api/authorize-publish" => "authorized a public publish (egress)",
        "/api/convert-private" => "converted a node to private (encrypted)",
        "/api/site/publish" => "published a website",
        "/api/site/checkpoint/save" => "saved a Studio checkpoint",
        "/api/deploy/credentials" => "saved deploy credentials (0600, on-device)",
        "/api/deploy/test" => "tested a publishing connection",
        "/api/deploy/cloudflare/oauth-start" => "started Cloudflare one-click login",
        "/api/deploy/firebase/oauth-start" => "started Firebase one-click login",
        "/api/youtube/oauth-start" => "started YouTube one-click login",
        "/api/youtube/disconnect" => "disconnected YouTube uploads",
        "/api/youtube/upload" => "started a YouTube upload (sends the video to YouTube)",
        "/api/pin/credentials" => "saved pinning-service credentials (0600, on-device)",
        "/api/pin/test" => "tested a pinning-service connection",
        "/api/site/pin" => "pinned a website to a pinning service",
        "/api/record/pin" => "pinned a record to a pinning service",
        "/api/record/unpin" => "stopped keeping a record hot on this node",
        "/api/git/commit" => "committed a project to GitHub",
        "/api/bookmarks/sync" => "synced wallet-browser bookmarks into memory",
        "/api/wallet/setup" => "opened the browser's wallet setup",
        "/api/wallet/link" => "linked a wallet to your AgentID",
        "/api/wallet/unlink" => "unlinked a wallet",
        "/api/wallet/settings" => "updated wallet settings",
        "/api/wallet/proposals/resolve" => "resolved an AI transaction proposal",
        "/api/mcp/write" => "toggled MCP write tools",
        "/api/canvas/snapshot" => "snapshotted the canvas",
        "/api/canvas/pwa" => "made the project installable as a mobile app (PWA)",
        "/api/canvas/new" => "started a fresh Studio project",
        "/api/canvas/delete" => "deleted a saved Studio project",
        "/api/blender/connect" => "connected Blender (BlenderMCP) to the host AI",
        "/api/requests/accept" => "accepted a contact request",
        "/api/requests/decline" => "declined a contact request",
        "/api/contacts/remove" => "removed an approved peer",
        "/api/thread/delete" => "deleted a message thread",
        "/api/claude-code/ingest" => "started a Claude Code history backfill",
        "/api/aider/ingest" => "started an Aider history backfill",
        "/api/codex/ingest" => "started a Codex history backfill",
        "/api/gemini/ingest" => "started a Gemini history backfill",
        "/api/continue/ingest" => "started a Continue history backfill",
        "/api/antigravity/ingest" => "started an Antigravity history backfill",
        "/api/openclaw/ingest" => "started an OpenClaw history backfill",
        "/api/cline/ingest" => "started a Cline history backfill",
        "/api/cursor/ingest" => "started a Cursor history backfill",
        "/api/opendevin/ingest" => "started an OpenDevin history backfill",
        "/api/copilot/ingest" => "started a Copilot history backfill",
        "/api/petname" => "set a petname",
        "/api/profile" => "updated your contact card",
        "/api/network/create" => "created a network / certificate",
        "/api/network/revoke" => "revoked network access",
        "/api/network/rotate" => "rotated a capability key",
        "/api/network/pair/offer" => "minted a device-pairing offer",
        "/api/network/pair/approve" => "approved a device pairing (scoped grant)",
        "/api/network/pair/accept" => "joined a network (pairing accepted)",
        // /api/message logs its own delivered/queued line; /api/canvas/* signalling is too noisy.
        _ => return None,
    })
}

/// Rotate a private graph's capability key (Phase N · Phase G) after a revocation,
/// so the revoked holder's old key cannot decrypt the re-rooted ciphertext. Password
/// travels in the loopback body (same pattern as convert-private), never the URL.
fn mutation_network_rotate(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let ciphertext_root = match body_str(&value, "ciphertext_root") {
        Ok(root) => root,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(pw) => pw,
        Err(response) => return response,
    };
    match mem.rotate_private_capability(ciphertext_root, password) {
        Ok(result) => Response::json(
            serde_json::json!({
                "old_ciphertext_root": result.old_ciphertext_root,
                "new_ciphertext_root": result.new_ciphertext_root,
                "capability_epoch": result.capability_epoch,
                "block_count": result.block_count,
            })
            .to_string(),
        ),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Post a direct private chat message into a local room thread (RoomBook). The
/// message is authored locally and appended to the room; the client re-fetches the
/// thread via `/api/thread`. Bodies travel in the loopback POST body, never the URL.
fn mutation_post_message(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let room = match body_str(&value, "room") {
        Ok(room) => room.trim(),
        Err(response) => return response,
    };
    let text = match body_str(&value, "body") {
        Ok(text) => text.trim(),
        Err(response) => return response,
    };
    if room.is_empty() || text.is_empty() {
        return Response::bad_request("recipient and message are required");
    }
    // A 64-hex "to" is a username (a direct message); anything else is a room name
    // (a group thread). Direct messages are stored under a shared dm-room id and
    // delivered to the recipient's personal inbox topic.
    if looks_like_username(room) {
        let me = match mem.identity() {
            Ok(identity) => identity.agent_id().0,
            Err(error) => return Response::error(error.to_string()),
        };
        if room == me {
            return Response::bad_request("cannot send a direct message to yourself");
        }
        let dm_room = dm_room_id(&me, room);
        let cid = match mem.post_message(&dm_room, text) {
            Ok(cid) => cid,
            Err(error) => return Response::error(error.to_string()),
        };
        // Initiating a conversation implies trust: approve the recipient so their
        // replies are accepted into the thread (not held as a request).
        let _ = mem.add_contact(room);
        let delivered = deliver_to_user(mem, options, room, &cid);
        return Response::json(
            serde_json::json!({
                "ok": true, "room": dm_room, "cid": cid.0,
                "delivered": delivered, "direct": true,
            })
            .to_string(),
        );
    }
    let cid = match mem.post_message(room, text) {
        Ok(cid) => cid,
        Err(error) => return Response::error(error.to_string()),
    };
    // Group room: publish the signed envelope to the room's gossipsub topic.
    let delivered = deliver_message(mem, options, room, &cid);
    Response::json(
        serde_json::json!({ "ok": true, "room": room, "cid": cid.0, "delivered": delivered })
            .to_string(),
    )
}

/// Deliver a direct message to a username: ensure the node is up, locate the peer
/// globally via the DHT (mDNS covers the LAN), and publish the signed envelope to
/// the recipient's inbox topic. Best-effort — if the peer is offline/unreachable
/// the message is still recorded locally (store-and-forward is a later stage).
fn deliver_to_user(mem: &MemCli, options: &GuiOptions, target_username: &str, cid: &Cid) -> bool {
    if let Err(error) = ensure_chat_node(mem, options) {
        eprintln!("chat node unavailable: {error}");
        return false;
    }
    let Ok(env) = mem.read_message(cid) else {
        return false;
    };
    let Ok(bytes) = serde_json::to_string(&env) else {
        return false;
    };
    let Some(peer) = peer_id_from_ed25519_hex(target_username) else {
        return false;
    };
    // Queue for store-and-forward retry: if the peer is offline now, the retry
    // loop re-sends until they ack (the ack clears the entry). Keyed by the same
    // content id the transport reports back on delivery.
    let bytes = bytes.into_bytes();
    let message_id = content_message_id(&bytes);
    let _ = mem.queue_outbound(
        &message_id,
        target_username,
        &String::from_utf8_lossy(&bytes),
    );
    if let Ok(guard) = options.chat.lock() {
        if let Some(chat) = guard.as_ref() {
            // Locate the peer (DHT for global; mDNS already covers the LAN), then
            // deliver point-to-point over the concierge-only protocol.
            let _ = chat.node.find_peer(peer);
            return chat.node.send_dm(peer, bytes).is_ok();
        }
    }
    false
}

/// `/api/mcp/status`: whether the host AI's MCP write tools are enabled (the GUI
/// toggle). Read-only by default (Decision 0028).
pub(super) fn mcp_status_json(mem: &MemCli) -> CoreResult<String> {
    Ok(serde_json::json!({ "write_enabled": mem.mcp_write_enabled() }).to_string())
}

/// `POST /api/mcp/write`: flip the MCP write-tools toggle. The MCP server re-reads
/// this per request, so it takes effect on the AI's next tool call.
fn mutation_mcp_write(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let enabled = value
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match mem.set_mcp_write_enabled(enabled) {
        Ok(()) => {
            Response::json(serde_json::json!({ "ok": true, "write_enabled": enabled }).to_string())
        }
        Err(error) => Response::error(error.to_string()),
    }
}

/// `POST /api/update/check`: query the release feed for a newer app build (network).
/// Returns the `ReleaseInfo` if one is available, or `null` when already current.
fn mutation_update_check(mem: &MemCli) -> Response {
    match mem.update_check() {
        Ok(release) => match serde_json::to_string(&serde_json::json!({ "release": release })) {
            Ok(body) => Response::json(body),
            Err(e) => Response::error(e.to_string()),
        },
        Err(error) => Response::error(error.to_string()),
    }
}

/// `POST /api/update/apply`: download + stage the newer build (network). The staged
/// binary is swapped in on next launch. Returns the `StagedUpdate`, or `null` if none.
fn mutation_update_apply(mem: &MemCli) -> Response {
    match mem.update_apply() {
        Ok(staged) => match serde_json::to_string(&serde_json::json!({ "staged": staged })) {
            Ok(body) => Response::json(body),
            Err(e) => Response::error(e.to_string()),
        },
        Err(error) => Response::error(error.to_string()),
    }
}

/// `POST /api/update/rules/refresh`: refresh the signed safety-rule set over IPNS
/// from the pinned publisher (network). Returns the `RefreshOutcome`.
fn mutation_update_rules_refresh(mem: &MemCli) -> Response {
    match mem.rules_refresh() {
        Ok(outcome) => match serde_json::to_string(&outcome) {
            Ok(body) => Response::json(body),
            Err(e) => Response::error(e.to_string()),
        },
        Err(error) => Response::error(error.to_string()),
    }
}

/// `POST /api/update/rules/pause`: the kill switch — freeze (or resume) automatic
/// rule refreshes. Body: `{ "paused": bool }`.
fn mutation_update_rules_pause(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let paused = value
        .get("paused")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match mem.rules_set_paused(paused) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true, "paused": paused }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// `POST /api/update/rules/pin`: trust only signatures from this publisher key.
/// Body: `{ "key": "<hex>" }`.
fn mutation_update_rules_pin(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let key = match body_str(&value, "key") {
        Ok(k) => k.trim(),
        Err(response) => return response,
    };
    if key.is_empty() {
        return Response::bad_request("publisher key is required");
    }
    match mem.rules_pin(key) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// `POST /api/update/rules/source`: set the IPNS latest pointer used for rules.
/// Body: `{ "ipns": "<k51...>" }`.
fn mutation_update_rules_source(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let ipns = match body_str(&value, "ipns") {
        Ok(k) => k.trim(),
        Err(response) => return response,
    };
    if ipns.is_empty() {
        return Response::bad_request("rules IPNS name is required");
    }
    match mem.rules_set_source(ipns) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// `POST /api/brain/model`: persist which model the Brain routes to. Body:
/// `{ "model": "<id>" }`. An empty string clears the selection, so we read the field
/// directly (not via `body_str`, which rejects empty strings).
fn mutation_brain_model(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let model = match value.get("model").and_then(|item| item.as_str()) {
        Some(model) => model,
        None => return Response::bad_request("missing required field"),
    };
    match mem.brain_set_model(model) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// `/api/sites`: the user's published websites (the Planet Pattern registry).
pub(super) fn sites_json(mem: &MemCli) -> CoreResult<String> {
    let sites: Vec<serde_json::Value> = mem
        .site_list()?
        .into_iter()
        .map(|site| {
            serde_json::json!({
                "name": site.name,
                "ipns": site.ipns,
                "dir": site.dir,
                "last_cid": site.last_cid,
                "published_at": site.published_at,
                "url": format!("https://ipfs.io/ipns/{}", site.ipns),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "sites": sites,
        "kubo_installed": concierge_core::kubo_installed(),
    })
    .to_string())
}

/// A site name is also the public Kubo IPNS key name — keep it to safe characters.
pub(super) fn valid_site_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `/api/site/publish`: publish (or update) a folder as a website. Password-gated
/// egress (the password travels in the loopback body, never the URL). Publishing
/// is the deliberate act; the AI only *staged* the folder.
/// Non-secret deploy-credential status (which platforms are configured + their
/// public fields). Tokens/passwords are NEVER serialized to the GUI.
pub(super) fn deploy_status_json(mem: &MemCli) -> CoreResult<String> {
    Ok(mem.deploy_status()?.to_string())
}

/// Non-secret pinning-service status (which services are configured + their endpoints).
/// Tokens are NEVER serialized to the GUI.
pub(super) fn pin_status_json(mem: &MemCli) -> CoreResult<String> {
    Ok(mem.pin_status()?.to_string())
}

/// Non-secret YouTube connection state (connected/channel/expiry — never a token).
/// Also reports whether this build can upload at all, so the GUI can show a clear
/// "uploads aren't configured in this build" state instead of a dead Connect button.
pub(super) fn youtube_status_json(mem: &MemCli) -> CoreResult<String> {
    let status = mem.youtube_status()?;
    Ok(serde_json::json!({
        "configured": concierge_core::youtube::oauth_configured(),
        "connected": status.connected,
        "channel": status.channel,
        "expires_at": status.expires_at,
    })
    .to_string())
}

/// The local upload history (newest last). Receipts carry no secrets.
pub(super) fn youtube_receipts_json(mem: &MemCli) -> CoreResult<String> {
    let receipts = mem.youtube_receipts()?;
    serde_json::to_string(&receipts).map_err(|e| Error::Io(format!("serialize receipts: {e}")))
}

/// Store (or clear) the credentials for one external host. The token/password
/// stays on-device (0600); only `{platform, fields}` comes in. Sending `fields:
/// null` clears that platform.
fn mutation_deploy_credentials(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let platform = match body_str(&value, "platform") {
        Ok(p) => p.trim(),
        Err(response) => return response,
    };
    let fields = value
        .get("fields")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    match mem.set_deploy_credentials(platform, &fields.to_string()) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// "Test connection" in the connect walk-through: verify a platform's token live
/// against its API. Tests unsaved `fields` if given, else the stored credentials.
/// Always 200 — the pass/fail + account/error is data the modal renders inline.
fn mutation_deploy_test(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let platform = match body_str(&value, "platform") {
        Ok(p) => p.trim().to_string(),
        Err(response) => return response,
    };
    let fields = value
        .get("fields")
        .filter(|v| !v.is_null())
        .map(|v| v.to_string());
    match mem.verify_deploy_credentials(&platform, fields.as_deref()) {
        Ok(account) => {
            Response::json(serde_json::json!({ "ok": true, "account": account }).to_string())
        }
        Err(error) => Response::json(
            serde_json::json!({ "ok": false, "error": error.to_string() }).to_string(),
        ),
    }
}

// ── One-click OAuth (PKCE) for Cloudflare + Firebase ────────────────────────────
// `oauth-start` opens a localhost callback listener + returns the authorize URL; the
// GUI opens it, the user approves in their browser, and the listener captures the code,
// exchanges it for a token, auto-detects the account/site, and saves it — no pasting.

#[derive(Clone, Default)]
struct OAuthProgress {
    status: String, // "idle" | "pending" | "connected" | "error"
    message: String,
    account: String,
}
fn oauth_progress() -> &'static std::sync::Mutex<HashMap<String, OAuthProgress>> {
    static PROGRESS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, OAuthProgress>>> =
        std::sync::OnceLock::new();
    PROGRESS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}
fn oauth_set(provider: &str, status: &str, message: &str, account: &str) {
    if let Ok(mut map) = oauth_progress().lock() {
        map.insert(
            provider.to_string(),
            OAuthProgress {
                status: status.to_string(),
                message: message.to_string(),
                account: account.to_string(),
            },
        );
    }
}

/// Non-secret OAuth progress for one provider, polled by the modal.
pub(super) fn oauth_status_json(provider: &str) -> String {
    let p = oauth_progress()
        .lock()
        .ok()
        .and_then(|m| m.get(provider).cloned())
        .unwrap_or_default();
    serde_json::json!({ "status": p.status, "message": p.message, "account": p.account })
        .to_string()
}

/// Drive one provider's login: open the authorize URL (returned), then on the callback
/// run `finish(code)` (provider-specific exchange + save) on a background thread.
fn oauth_run(
    provider: &str,
    authorize_url: String,
    listener: std::net::TcpListener,
    expected_state: String,
    finish: OAuthFinish,
) -> Response {
    oauth_set(
        provider,
        "pending",
        "Waiting for you to approve in the browser…",
        "",
    );
    let provider = provider.to_string();
    std::thread::spawn(move || oauth_listen(provider, listener, expected_state, finish));
    Response::json(serde_json::json!({ "authorize_url": authorize_url }).to_string())
}

fn mutation_cf_oauth_start(mem: &MemCli) -> Response {
    let start = concierge_core::deploy::cloudflare_oauth_start();
    // Reserve the exact callback port Cloudflare registered for the Wrangler client.
    let listener = match std::net::TcpListener::bind(concierge_core::deploy::CF_OAUTH_CALLBACK_ADDR)
    {
        Ok(listener) => listener,
        Err(error) => {
            return Response::error(format!(
                "couldn't open the local login listener on {} — is `wrangler` already logging in? ({error})",
                concierge_core::deploy::CF_OAUTH_CALLBACK_ADDR
            ))
        }
    };
    let mem = mem.clone();
    let verifier = start.verifier.clone();
    let finish = Box::new(move |code: &str| -> Result<String, String> {
        let token = concierge_core::deploy::cloudflare_oauth_exchange(code, &verifier)?;
        let account = concierge_core::deploy::cloudflare_list_account_id(&token.access_token)
            .unwrap_or_default();
        mem.save_cloudflare_oauth(
            &token.access_token,
            token.refresh_token.as_deref(),
            token.expires_at,
            &account,
        )
        .map_err(|e| e.to_string())?;
        Ok(account)
    });
    oauth_run(
        "cloudflare",
        start.authorize_url,
        listener,
        start.state,
        finish,
    )
}

fn mutation_fb_oauth_start(mem: &MemCli) -> Response {
    // Google "Desktop" clients allow a loopback redirect on any port — bind an ephemeral one.
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => {
            return Response::error(format!("couldn't open the local login listener: {error}"))
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let redirect = format!("http://127.0.0.1:{port}");
    let start = concierge_core::deploy::firebase_oauth_start(&redirect);
    let mem = mem.clone();
    let verifier = start.verifier.clone();
    let finish = Box::new(move |code: &str| -> Result<String, String> {
        let token = concierge_core::deploy::firebase_oauth_exchange(code, &verifier, &redirect)?;
        let site =
            concierge_core::deploy::firebase_default_site(&token.access_token).unwrap_or_default();
        mem.save_firebase_oauth(
            &token.access_token,
            token.refresh_token.as_deref(),
            token.expires_at,
            &site,
        )
        .map_err(|e| e.to_string())?;
        Ok(if site.is_empty() {
            "connected".to_string()
        } else {
            site
        })
    });
    oauth_run(
        "firebase",
        start.authorize_url,
        listener,
        start.state,
        finish,
    )
}

// ── YouTube uploads (Google PKCE) ───────────────────────────────────────────────
// Connect mirrors the Firebase ephemeral-loopback OAuth flow exactly (it reuses the
// generic `oauth_run`/`oauth_listen` helpers with a YouTube-specific `finish`). Upload
// is long-running, so it follows the spawn-thread + in-memory progress-map + poll
// pattern (cloned from `oauth_progress`) instead of blocking the request thread.

/// Begin the YouTube one-click login. Returns `{ authorize_url }` for the GUI to open;
/// the loopback listener then exchanges the code and saves the token off-thread.
fn mutation_yt_oauth_start(mem: &MemCli) -> Response {
    if !concierge_core::youtube::oauth_configured() {
        return Response::error(
            "YouTube uploads aren't configured in this build (no Google client baked in)."
                .to_string(),
        );
    }
    // Google "Desktop" clients allow a loopback redirect on any port — bind an ephemeral one.
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => {
            return Response::error(format!("couldn't open the local login listener: {error}"))
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let redirect = format!("http://127.0.0.1:{port}");
    let start = mem.youtube_oauth_start(&redirect);
    let mem = mem.clone();
    let verifier = start.verifier.clone();
    let finish = Box::new(move |code: &str| -> Result<String, String> {
        let token = concierge_core::youtube::oauth_exchange(code, &verifier, &redirect)?;
        mem.youtube_save_oauth(token, None)
            .map_err(|e| e.to_string())?;
        let channel = mem
            .youtube_status()
            .ok()
            .and_then(|s| s.channel)
            .unwrap_or_default();
        Ok(if channel.is_empty() {
            "connected".to_string()
        } else {
            channel
        })
    });
    oauth_run(
        "youtube",
        start.authorize_url,
        listener,
        start.state,
        finish,
    )
}

/// `POST /api/youtube/disconnect`: forget the saved Google token on this device.
fn mutation_yt_disconnect(mem: &MemCli) -> Response {
    match mem.youtube_disconnect() {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// In-memory progress for the (single) active upload, polled by the modal. Carries no
/// secrets — cloned from `oauth_progress`'s OnceLock<Mutex<HashMap>> pattern.
#[derive(Clone, Default)]
struct UploadProgress {
    status: String, // "idle" | "uploading" | "complete" | "error"
    percent: u8,
    bytes_sent: u64,
    bytes_total: u64,
    message: String,
    video_url: String,
}
fn youtube_progress() -> &'static std::sync::Mutex<HashMap<String, UploadProgress>> {
    static PROGRESS: std::sync::OnceLock<std::sync::Mutex<HashMap<String, UploadProgress>>> =
        std::sync::OnceLock::new();
    PROGRESS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}
fn youtube_progress_set(p: UploadProgress) {
    if let Ok(mut map) = youtube_progress().lock() {
        map.insert("upload".to_string(), p);
    }
}

/// Non-secret upload progress for the active job, polled by the modal.
pub(super) fn youtube_upload_status_json() -> String {
    let p = youtube_progress()
        .lock()
        .ok()
        .and_then(|m| m.get("upload").cloned())
        .unwrap_or_default();
    let status = if p.status.is_empty() {
        "idle".to_string()
    } else {
        p.status
    };
    serde_json::json!({
        "status": status,
        "percent": p.percent,
        "bytes_sent": p.bytes_sent,
        "bytes_total": p.bytes_total,
        "message": p.message,
        "video_url": p.video_url,
    })
    .to_string()
}

/// `POST /api/youtube/upload`: parse the review-screen body as an `UploadRequest`,
/// then spawn a thread that runs the (long) upload while updating the progress map.
/// Returns immediately with `{ started: true }` — the request thread never blocks on
/// the transfer. The GUI polls `/api/youtube/upload-status` for progress.
fn mutation_yt_upload(mem: &MemCli, body: &str) -> Response {
    let req: concierge_core::UploadRequest = match serde_json::from_str(body) {
        Ok(req) => req,
        Err(error) => return Response::bad_request(&format!("invalid upload request: {error}")),
    };
    youtube_progress_set(UploadProgress {
        status: "uploading".to_string(),
        message: "Starting upload…".to_string(),
        ..Default::default()
    });
    let mem = mem.clone();
    std::thread::spawn(move || {
        let mut progress = |sent: u64, total: u64| {
            let percent = if total > 0 {
                ((sent.min(total) as u128 * 100) / total as u128) as u8
            } else {
                0
            };
            youtube_progress_set(UploadProgress {
                status: "uploading".to_string(),
                percent,
                bytes_sent: sent,
                bytes_total: total,
                message: format!("Uploading… {percent}%"),
                video_url: String::new(),
            });
        };
        match mem.youtube_upload(&req, &mut progress) {
            Ok(receipt) => youtube_progress_set(UploadProgress {
                status: "complete".to_string(),
                percent: 100,
                bytes_sent: 0,
                bytes_total: 0,
                message: "Upload complete.".to_string(),
                video_url: receipt.video_url,
            }),
            Err(error) => youtube_progress_set(UploadProgress {
                status: "error".to_string(),
                message: error.to_string(),
                ..Default::default()
            }),
        }
    });
    Response::json(serde_json::json!({ "started": true }).to_string())
}

/// Wait (up to 5 min) for the browser to redirect to the localhost callback, capture
/// the code, run `finish` (exchange + persist), and report the outcome via the status.
fn oauth_listen(
    provider: String,
    listener: std::net::TcpListener,
    expected_state: String,
    finish: OAuthFinish,
) {
    use std::io::{Read, Write};
    listener.set_nonblocking(true).ok();
    let deadline = now_unix() + 300;
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if now_unix() > deadline {
                    oauth_set(&provider, "error", "Login timed out — try again.", "");
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(error) => {
                oauth_set(&provider, "error", &format!("login listener: {error}"), "");
                return;
            }
        }
    };
    stream.set_nonblocking(false).ok();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .ok();
    let mut buf = [0u8; 8192];
    let read = stream.read(&mut buf).unwrap_or(0);
    let request = String::from_utf8_lossy(&buf[..read]);
    let query = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|target| target.split_once('?'))
        .map(|(_, q)| q.to_string())
        .unwrap_or_default();
    let params = parse_query(&query);

    let reply = |stream: &mut std::net::TcpStream, heading: &str, ok: bool| {
        let color = if ok { "#00e5ff" } else { "#ff2a55" };
        let body = format!(
            "<!doctype html><meta charset=utf-8><body style='font:15px system-ui;background:#0a0b1a;color:#e0e6ff;display:grid;place-items:center;height:100vh;margin:0;text-align:center'><div><h2 style='color:{color}'>{heading}</h2><p style='color:#939abf'>You can close this tab and return to the Concierge.</p></div>"
        );
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    };

    if let Some(error) = params.get("error") {
        reply(&mut stream, "Login cancelled", false);
        oauth_set(
            &provider,
            "error",
            &format!("login was declined: {error}"),
            "",
        );
        return;
    }
    let code = params.get("code").cloned().unwrap_or_default();
    let state = params.get("state").cloned().unwrap_or_default();
    if code.is_empty() || state != expected_state {
        reply(&mut stream, "Login could not be verified", false);
        oauth_set(
            &provider,
            "error",
            "Login response didn't validate (state mismatch).",
            "",
        );
        return;
    }
    reply(&mut stream, "✓ Connected — finishing up", true);
    oauth_set(&provider, "pending", "Finishing sign-in…", "");
    match finish(&code) {
        Ok(account) => oauth_set(&provider, "connected", "Connected.", &account),
        Err(error) => oauth_set(&provider, "error", &error, ""),
    }
}

/// Save (or clear) one pinning service's `{endpoint, token}` on-device (0600).
fn mutation_pin_credentials(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match body_str(&value, "service") {
        Ok(s) => s.trim(),
        Err(response) => return response,
    };
    let fields = value
        .get("fields")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    match mem.set_pin_credentials(service, &fields.to_string()) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// "Test connection" for a pinning service: list pins to verify the token. Always 200;
/// the pass/fail + label/error is data the modal renders inline.
fn mutation_pin_test(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let service = match body_str(&value, "service") {
        Ok(s) => s.trim().to_string(),
        Err(response) => return response,
    };
    let fields = value
        .get("fields")
        .filter(|v| !v.is_null())
        .map(|v| v.to_string());
    match mem.verify_pin_credentials(&service, fields.as_deref()) {
        Ok(account) => {
            Response::json(serde_json::json!({ "ok": true, "account": account }).to_string())
        }
        Err(error) => Response::json(
            serde_json::json!({ "ok": false, "error": error.to_string() }).to_string(),
        ),
    }
}

/// `/api/site/pin`: publish the site to IPFS AND pin its root CID to an always-on
/// pinning service, so the `/ipns/<k51>` link stays reachable even when this node is
/// offline. Password-gated. Snapshots the result into Checkpoints with its live URL.
fn mutation_pin_site(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let name = match body_str(&value, "name") {
        Ok(n) => n.trim().to_string(),
        Err(response) => return response,
    };
    if !valid_site_name(&name) {
        return Response::bad_request("site name must be letters, digits, - _ . (max 64)");
    }
    let folder = match body_str(&value, "folder") {
        Ok(f) => f.trim().to_string(),
        Err(response) => return response,
    };
    let service = match body_str(&value, "service") {
        Ok(s) => s.trim().to_string(),
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(p) => p.to_string(),
        Err(response) => return response,
    };
    match mem.pin_site(&name, &folder, &service, &password) {
        Ok(receipt) => {
            let url = format!("https://ipfs.io/ipns/{}/", receipt.ipns_name);
            // Snapshot in Checkpoints with the live URL (so it gets a copyable link too).
            let _ = record_site_checkpoint(
                mem,
                &name,
                &folder,
                Some(receipt.ipns_name.as_str()),
                &receipt.cid,
                &url,
            );
            Response::json(
                serde_json::json!({
                    "ok": true,
                    "service": receipt.service,
                    "status": receipt.status,
                    "cid": receipt.cid,
                    "ipns": receipt.ipns_name,
                    "url": url,
                    "request_id": receipt.request_id,
                })
                .to_string(),
            )
        }
        Err(error) => Response::error(error.to_string()),
    }
}

/// `/api/record/pin`: pin a single record (by CID) to an always-on pinning service.
/// `private=true` encrypts the subgraph first and blind-pins opaque ciphertext;
/// `private=false` pins the plaintext (public — anyone with the CID can read it).
/// Password-gated (pinning is egress).
fn mutation_pin_record(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let cid = match body_str(&value, "cid") {
        Ok(c) => c.trim().to_string(),
        Err(response) => return response,
    };
    let service = match body_str(&value, "service") {
        Ok(s) => s.trim().to_string(),
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(p) => p.to_string(),
        Err(response) => return response,
    };
    let private = value
        .get("private")
        .and_then(|v| v.as_bool())
        .unwrap_or(true); // records default to private (encrypted blind-pin)
    match mem.pin_record(&cid, &service, private, &password) {
        Ok(receipt) => {
            let url = if receipt.private || receipt.service == "node" {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(format!("https://ipfs.io/ipfs/{}", receipt.cid))
            };
            Response::json(
                serde_json::json!({
                    "ok": true,
                    "service": receipt.service,
                    "status": receipt.status,
                    "cid": receipt.cid,
                    "source_cid": receipt.source_cid,
                    "private": receipt.private,
                    "url": url,
                    "request_id": receipt.request_id,
                })
                .to_string(),
            )
        }
        Err(error) => Response::error(error.to_string()),
    }
}

/// `/api/record/unpin`: stop keeping a record hot on this node (unpin from the private
/// node + drop from the ledger). The original record is untouched.
fn mutation_unpin_record(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let cid = match body_str(&value, "cid") {
        Ok(c) => c.trim().to_string(),
        Err(response) => return response,
    };
    match mem.unpin_hot(&cid) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// `/api/git/commit`: stage + commit + push an entire Studio project folder to GitHub
/// (creating the repo if needed). Password-gated (egress). `private` defaults to true.
fn mutation_git_commit(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let folder = match body_str(&value, "folder") {
        Ok(f) => f.trim().to_string(),
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(p) => p.to_string(),
        Err(response) => return response,
    };
    let repo = value
        .get("repo")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let message = value
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let private = value
        .get("private")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    match mem.commit_project_github(&folder, &repo, &message, private, &password) {
        Ok(receipt) => Response::json(
            serde_json::json!({
                "ok": true,
                "repo_url": receipt.repo_url,
                "branch": receipt.branch,
                "committed": receipt.committed,
                "created_repo": receipt.created_repo,
                "private": receipt.private,
            })
            .to_string(),
        ),
        Err(error) => Response::error(error.to_string()),
    }
}

/// `/api/blender/connect`: register **BlenderMCP** with the host AI (Claude Code) so it
/// can drive Blender for Movie/Animation projects — `claude mcp add -s user blender --
/// uvx blender-mcp`. Best-effort + idempotent; reports what's missing (Claude CLI / uv)
/// and always returns the add-on install steps. Modifies only the host AI's MCP config.
fn mutation_blender_connect() -> Response {
    use std::process::Command;
    const ADDON: &str = "Install the Blender add-on: in Blender → Edit → Preferences → Add-ons → Install… → choose addon.py (Concierge repo: vendor/blender-mcp/addon.py, or blendermcp.org). Enable “Interface: Blender MCP”, then in the 3D viewport press N → BlenderMCP → Connect to MCP server. Restart Claude Code.";
    let reply = |connected: bool, message: &str| {
        Response::json(
            serde_json::json!({ "ok": true, "connected": connected, "message": message, "addon": ADDON })
                .to_string(),
        )
    };

    let listed = match Command::new("claude").args(["mcp", "list"]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => {
            return reply(
                false,
                "Claude Code (the `claude` CLI) wasn't found. Install Claude Code, then run: concierge-plugin blender-setup",
            )
        }
    };
    if listed
        .lines()
        .any(|l| l.trim_start().starts_with("blender:"))
    {
        return reply(
            true,
            "Blender (BlenderMCP) is already connected to Claude Code.",
        );
    }
    let has_uvx = Command::new("uvx")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_uvx {
        return reply(
            false,
            "Install uv (the Python runner BlenderMCP uses): https://docs.astral.sh/uv/ — then run: concierge-plugin blender-setup",
        );
    }
    match Command::new("claude")
        .args(["mcp", "add", "-s", "user", "blender", "--", "uvx", "blender-mcp"])
        .output()
    {
        Ok(out) if out.status.success() => reply(
            true,
            "Connected Blender (BlenderMCP) to Claude Code. Restart Claude Code, then install the Blender add-on.",
        ),
        _ => reply(
            false,
            "Couldn't auto-register Blender. Run manually: claude mcp add -s user blender -- uvx blender-mcp",
        ),
    }
}

/// Whether the publish node is reachable from outside the LAN (so shared links load
/// for others), with its public addresses.
pub(super) fn reachability_json(mem: &MemCli) -> CoreResult<String> {
    serde_json::to_string(&mem.public_reachability()?)
        .map_err(|e| Error::Io(format!("serialize reachability: {e}")))
}

/// Pillar A: pull the wallet browser's (Brave/Opera) bookmarks into memory. Returns
/// how many *new* bookmarks were ingested (deduped by URL).
/// Summary of a freshly-appended record, in the same shape `/api/names` emits — so
/// the UI can insert just this one row into the tree (IPLD is append-only) instead of
/// re-deriving the whole view. Shared by every write that returns new records.
fn appended_record(
    cid: &Cid,
    kind: &str,
    preview: &str,
    linked: bool,
    name: &str,
) -> serde_json::Value {
    serde_json::json!({ "cid": cid.0, "kind": kind, "preview": preview, "linked": linked, "names": [name] })
}

fn mutation_bookmarks_sync(mem: &MemCli) -> Response {
    match mem.sync_browser_bookmarks() {
        Ok(added) => {
            let records: Vec<serde_json::Value> = added
                .iter()
                .map(|(cid, name, preview)| appended_record(cid, "memory", preview, false, name))
                .collect();
            Response::json(
                serde_json::json!({ "ok": true, "added": added.len(), "records": records })
                    .to_string(),
            )
        }
        Err(error) => Response::error(error.to_string()),
    }
}

/// Open one of the wallet browser's internal pages from the Concierge — wallet
/// onboarding (`brave://wallet`) or the full wallet settings
/// (`brave://settings/wallet`). Web pages can't navigate to `brave://`, so the
/// Concierge process launches it. `target` ∈ {"wallet","settings"} (default wallet).
fn mutation_wallet_setup(body: &str) -> Response {
    let target = parse_body(body)
        .ok()
        .and_then(|v| v.get("target").and_then(|t| t.as_str()).map(str::to_string))
        .unwrap_or_else(|| "wallet".to_string());
    match wallet_browser() {
        Some((WalletBrowser::Brave, exe)) => {
            let url = match target.as_str() {
                "settings" => "brave://settings/wallet",
                _ => "brave://wallet",
            };
            // Open in a separate, compact window (not a tab). Brave blocks internal
            // `brave://` pages in chromeless `--app` mode, so this is a normal window;
            // --window-size is best-effort (honored when it starts a fresh instance).
            match Command::new(&exe)
                .args(["--new-window", "--window-size=480,840", url])
                .spawn()
            {
                Ok(_) => Response::json(serde_json::json!({ "ok": true }).to_string()),
                Err(error) => Response::json(
                    serde_json::json!({ "ok": false, "error": format!("could not open Brave: {error}") }).to_string(),
                ),
            }
        }
        Some((WalletBrowser::Opera, _)) => Response::json(
            serde_json::json!({ "ok": false, "error": "In Opera, open the Crypto Wallet from the sidebar." }).to_string(),
        ),
        None => Response::json(
            serde_json::json!({ "ok": false, "error": "No wallet browser detected — open the Concierge in Brave or Opera." }).to_string(),
        ),
    }
}

/// Wallet state for the Concierge Wallet tab — verified links + on-device settings.
/// No keys (the browser holds those); links are public attestations.
pub(super) fn wallet_json(mem: &MemCli) -> CoreResult<String> {
    serde_json::to_string(&mem.wallet_state()?)
        .map_err(|e| Error::Io(format!("serialize wallet state: {e}")))
}

/// Record a verified `WalletLink`: the browser wallet signed our AgentID; we recover
/// the signer and confirm it matches the claimed address.
fn mutation_wallet_link(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let address = match body_str(&value, "address") {
        Ok(a) => a.trim().to_string(),
        Err(response) => return response,
    };
    let signature = match body_str(&value, "signature") {
        Ok(s) => s.trim().to_string(),
        Err(response) => return response,
    };
    let chain = value.get("chain").and_then(|v| v.as_str()).unwrap_or("evm");
    match mem.link_wallet(&address, chain, &signature) {
        Ok(link) => Response::json(
            serde_json::json!({ "ok": true, "address": link.address, "chain": link.chain })
                .to_string(),
        ),
        Err(error) => Response::error(error.to_string()),
    }
}

fn mutation_wallet_unlink(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let address = match body_str(&value, "address") {
        Ok(a) => a.trim().to_string(),
        Err(response) => return response,
    };
    match mem.unlink_wallet(&address) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Persist the wallet settings (agent_access / spend_cap / allowlist / preferred_chain).
fn mutation_wallet_settings(mem: &MemCli, body: &str) -> Response {
    // The body *is* the settings object; WalletSettings is `#[serde(default)]`.
    match mem.set_wallet_settings(body) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Pending AI transaction proposals for the Wallet tab to surface for approval.
pub(super) fn wallet_proposals_json(mem: &MemCli) -> CoreResult<String> {
    serde_json::to_string(&mem.pending_wallet_proposals()?)
        .map_err(|e| Error::Io(format!("serialize proposals: {e}")))
}

/// Record the user's decision on a proposal ("approved" + tx hash, or "rejected").
fn mutation_wallet_resolve(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let id = match body_str(&value, "id") {
        Ok(id) => id.trim().to_string(),
        Err(response) => return response,
    };
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("rejected");
    let tx_hash = value.get("tx_hash").and_then(|v| v.as_str()).unwrap_or("");
    match mem.resolve_wallet_proposal(&id, status, tx_hash) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

fn site_deploy_fields(value: &serde_json::Value) -> Result<(&str, &str, &str, &str), Response> {
    let name = body_str(value, "name")?.trim();
    let folder = body_str(value, "folder")?.trim();
    let kind = value
        .get("kind")
        .and_then(|item| item.as_str())
        .unwrap_or("site");
    let platform = value
        .get("platform")
        .and_then(|item| item.as_str())
        .unwrap_or("ipfs");
    if !valid_site_name(name) {
        return Err(Response::bad_request(
            "site name must be letters, digits, - _ . (max 64)",
        ));
    }
    if folder.is_empty() {
        return Err(Response::bad_request("a folder path is required"));
    }
    // Only websites publish: the folder must have an index.html web entry point at its
    // root. Non-web projects (a CLI, a service, raw source) have no page to serve, so we
    // refuse rather than push something that won't load as a site.
    if !std::path::Path::new(folder).join("index.html").is_file() {
        return Err(Response::bad_request(
            "Only websites can be published. This folder has no index.html — the web entry \
point that loads first. Add an index.html and try again; non-web projects (apps, services, \
raw source) aren't publishable as sites.",
        ));
    }
    Ok((name, folder, kind, platform))
}

fn mutation_site_deploy_plan(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let (name, folder, kind, platform) = match site_deploy_fields(&value) {
        Ok(fields) => fields,
        Err(response) => return response,
    };
    match mem.review_site_deploy(name, folder, kind, platform) {
        Ok(plan) => match options.cache_site_deploy_review(plan.clone()) {
            Ok(review_token) => Response::json(
                serde_json::json!({ "review_token": review_token, "plan": plan }).to_string(),
            ),
            Err(error) => Response::error(error.to_string()),
        },
        Err(error) => Response::error(error.to_string()),
    }
}

fn mutation_publish_site(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    let review_token = match body_str(&value, "review_token") {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(reviewed) = options.reviewed_site_deploy(review_token) else {
        return Response::bad_request("deployment review token is missing or expired");
    };
    match mem.publish_site(&reviewed, password) {
        Ok(receipt) => {
            options.discard_review(review_token);
            // Snapshot this published version so the user can re-open it to update.
            let checkpoint_warning = record_site_checkpoint(
                mem,
                &reviewed.name,
                &reviewed.folder,
                receipt.ipns_name.as_deref(),
                &receipt.root,
                &receipt.gateway_url,
            )
            .err()
            .map(|error| error.to_string());
            Response::json(
                serde_json::json!({
                    "ok": true,
                    "name": receipt.site_name,
                    "ipns": receipt.ipns_name,
                    "cid": receipt.root,
                    "url": receipt.gateway_url,
                    "checkpoint_warning": checkpoint_warning,
                })
                .to_string(),
            )
        }
        Err(error) => Response::error(error.to_string()),
    }
}

/// `/api/site/checkpoint/save`: snapshot the current Studio draft as a checkpoint
/// at any time — no publish, no egress. Content-addresses the HTML (a real CID, in
/// Records), and stores a reopen-to-edit snapshot in the Studio checkpoint list.
/// Accepts `{name, html}` (Write mode) or `{name, folder}` (folder/preview mode).
pub(super) fn requests_json(mem: &MemCli) -> CoreResult<String> {
    let items: Vec<serde_json::Value> = mem
        .message_requests()?
        .into_iter()
        .map(|(username, count, preview)| {
            serde_json::json!({ "username": username, "count": count, "preview": preview })
        })
        .collect();
    Ok(serde_json::json!({ "requests": items }).to_string())
}

/// The approved peers — usernames whose direct messages we accept. Surfaced in the
/// Messenger tab so the user can see (and revoke) who can reach them. Each carries
/// the deterministic 1:1 thread id so the UI can open the conversation directly.
pub(super) fn contacts_json(mem: &MemCli) -> CoreResult<String> {
    let me = mem.identity().map(|id| id.agent_id().0).unwrap_or_default();
    let items: Vec<serde_json::Value> = mem
        .approved_contacts()?
        .into_iter()
        .map(|username| {
            let room = if me.is_empty() {
                String::new()
            } else {
                dm_room_id(&me, &username)
            };
            // Sovereign naming (Layers 1+2): resolve a display name + provenance.
            let resolved = mem.resolve_display(&username);
            let card = mem.card_of(&username).ok().flatten();
            serde_json::json!({
                "username": username,
                "room": room,
                "name": resolved.text,
                "name_source": resolved.source,
                "verified": resolved.verified,
                "avatar": card.as_ref().and_then(|c| c.avatar.clone()),
                "site_ipns": card.as_ref().and_then(|c| c.site_ipns.clone()),
            })
        })
        .collect();
    Ok(serde_json::json!({ "contacts": items }).to_string())
}

/// `GET /api/profile` — the user's own (signed) contact card, for the editor.
pub(super) fn profile_json(mem: &MemCli) -> CoreResult<String> {
    let card = mem.my_card()?;
    Ok(serde_json::json!({
        "did": card.did,
        "display_name": card.display_name,
        "bio": card.bio,
        "avatar": card.avatar,
        "site_ipns": card.site_ipns,
        "agent_id": mem.identity().map(|id| id.agent_id().0).unwrap_or_default(),
    })
    .to_string())
}

/// Compact the store: run GC to reclaim unreferenced (superseded) blocks and trim
/// the auto-checkpoint chain to the configured keep-count. Safe by construction —
/// only blocks no live name, kept checkpoint, or Decision can reach are removed,
/// and each removal records a tombstone receipt. Local maintenance, never egress.
fn mutation_compact(mem: &MemCli, options: &GuiOptions) -> Response {
    match mem.gc(&concierge_core::GcPolicy {
        keep_checkpoints: None,
    }) {
        Ok(report) => {
            options.log(
                "ok",
                format!(
                    "compacted store · reclaimed {} block(s), kept {}",
                    report.removed, report.kept
                ),
            );
            Response::json(
                serde_json::json!({ "removed": report.removed, "kept": report.kept }).to_string(),
            )
        }
        Err(error) => Response::error(error.to_string()),
    }
}

/// Set (or, with an empty name, clear) a local petname for an AgentID — Layer 1.
fn mutation_petname(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let agent_id = match body_str(&value, "agent_id") {
        Ok(a) => a.trim(),
        Err(response) => return response,
    };
    if agent_id.is_empty() {
        return Response::bad_request("agent_id is required");
    }
    let name = value
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .trim();
    let result = if name.is_empty() {
        mem.remove_nickname(agent_id)
    } else {
        mem.set_nickname(agent_id, name)
    };
    match result {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Edit the user's own contact card (Layer 2 self-asserted profile).
fn mutation_profile(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let field = |k: &str| value.get(k).and_then(|v| v.as_str());
    match mem.update_my_card(
        field("display_name"),
        field("bio"),
        field("avatar"),
        field("site_ipns"),
    ) {
        Ok(()) => to_response(profile_json(mem)),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Revoke approval for a peer (they go back to needing a request to reach you).
/// `/api/thread/delete`: forget a message thread (the room head pointer + policy).
/// The signed message nodes stay in the content-addressed store; only the thread
/// pointer is removed, so a stale or legacy thread disappears from the messenger.
fn mutation_thread_delete(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let room = match body_str(&value, "room") {
        Ok(room) => room.trim(),
        Err(response) => return response,
    };
    if room.is_empty() {
        return Response::bad_request("room is required");
    }
    match mem.delete_thread(room) {
        Ok(removed) => {
            Response::json(serde_json::json!({ "ok": true, "removed": removed }).to_string())
        }
        Err(error) => Response::error(error.to_string()),
    }
}

fn mutation_contact_remove(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let username = match body_str(&value, "username") {
        Ok(username) => username.trim(),
        Err(response) => return response,
    };
    if username.is_empty() {
        return Response::bad_request("username is required");
    }
    match mem.remove_contact(username) {
        Ok(removed) => {
            Response::json(serde_json::json!({ "ok": true, "removed": removed }).to_string())
        }
        Err(error) => Response::error(error.to_string()),
    }
}

/// Accept (approve sender + flush their held messages into the thread) or decline
/// (drop their held messages, stay blocked) a pending message request.
fn mutation_request_decision(mem: &MemCli, body: &str, accept: bool) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let username = match body_str(&value, "username") {
        Ok(username) => username.trim(),
        Err(response) => return response,
    };
    if username.is_empty() {
        return Response::bad_request("username is required");
    }
    if accept {
        match mem.accept_contact(username) {
            Ok(delivered) => Response::json(
                serde_json::json!({ "ok": true, "delivered": delivered }).to_string(),
            ),
            Err(error) => Response::error(error.to_string()),
        }
    } else {
        match mem.decline_contact(username) {
            Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
            Err(error) => Response::error(error.to_string()),
        }
    }
}

/// Best-effort peer delivery of a just-posted message: ensure the chat node is up
/// and publish the signed envelope to the room topic. Returns whether it was
/// handed to the transport — *not* whether a peer received it. The message is
/// already recorded locally, so offline / no-peer cases never fail the post.
fn deliver_message(mem: &MemCli, options: &GuiOptions, room: &str, cid: &Cid) -> bool {
    if let Err(error) = ensure_chat_node(mem, options) {
        eprintln!("chat node unavailable: {error}");
        return false;
    }
    let Ok(env) = mem.read_message(cid) else {
        return false;
    };
    let Ok(bytes) = serde_json::to_string(&env) else {
        return false;
    };
    if let Ok(guard) = options.chat.lock() {
        if let Some(chat) = guard.as_ref() {
            let _ = chat.node.subscribe(room);
            return chat.node.publish(room, bytes.into_bytes()).is_ok();
        }
    }
    false
}

/// Found a new private network from the Data Platter (Phase N · Phase H). Returns
/// the refreshed network map.
fn mutation_network_create(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let name = value
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        return Response::bad_request("network name is required");
    }
    match mem.create_network(name) {
        Ok(_) => to_response(network_json(mem)),
        Err(error) => Response::error(error.to_string()),
    }
}

// ── In-GUI pairing wizard ───────────────────────────────────────────────────────
// A guided offer → response → grant handshake (copied between the two machines, with a
// safety phrase to compare). Mirrors the `network pair/respond/approve/accept` CLI flow,
// so it works regardless of whether the two devices can reach each other yet.

/// Capabilities for a wizard scope preset over the whole store (`all` namespace).
fn pair_scope_caps(
    network_id: concierge_core::NetworkId,
    scope: &str,
) -> (concierge_core::Namespace, Vec<concierge_core::Operation>) {
    let namespace = concierge_core::Namespace::new(network_id, concierge_core::NamespaceScope::All);
    let ops = if scope == "read" {
        vec!["sync_read"]
    } else {
        vec!["sync_read", "sync_write"]
    };
    let operations = ops
        .into_iter()
        .filter_map(|tok| serde_json::from_value(serde_json::Value::String(tok.to_string())).ok())
        .collect();
    (namespace, operations)
}

/// Pull one of `offer` / `response` / `grant` out of the request body and deserialize it.
fn pair_field<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    key: &str,
) -> Result<T, Response> {
    match value.get(key) {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| Response::bad_request(&format!("invalid {key}: {e}"))),
        None => Err(Response::bad_request(&format!("missing {key}"))),
    }
}

/// Admin side, step 1: mint a one-use offer for this device's network (creating a
/// default network if none exists). The offer carries no secrets.
fn mutation_pair_offer(mem: &MemCli) -> Response {
    let descriptor = match mem.networks() {
        Ok(mut networks) if !networks.is_empty() => networks.remove(0),
        Ok(_) => match mem.create_network("home") {
            Ok(descriptor) => descriptor,
            Err(error) => return Response::error(error.to_string()),
        },
        Err(error) => return Response::error(error.to_string()),
    };
    // The rendezvous is where the new device will later look to sync; a sensible default
    // (the user supplies the actual peer address at sync time).
    match mem.create_pairing_offer(&descriptor.network_id, "/ip4/127.0.0.1/tcp/4001") {
        Ok(offer) => Response::json(
            serde_json::json!({ "ok": true, "network": descriptor.name, "offer": offer })
                .to_string(),
        ),
        Err(error) => Response::error(error.to_string()),
    }
}

/// New device, step 1: verify the offer, prove possession, return the response + phrase.
fn mutation_pair_respond(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let offer: concierge_core::PairingOffer = match pair_field(&value, "offer") {
        Ok(offer) => offer,
        Err(response) => return response,
    };
    if let Err(error) = offer.verify() {
        return Response::bad_request(&format!("offer rejected: {error}"));
    }
    let device = match mem.identity() {
        Ok(device) => device,
        Err(error) => return Response::error(error.to_string()),
    };
    let response = concierge_core::PairingResponse::create(&offer, &device);
    let phrase = concierge_core::confirmation_phrase(&offer, &response);
    Response::json(
        serde_json::json!({ "ok": true, "response": response, "phrase": phrase }).to_string(),
    )
}

/// Admin side, step 2: compute the safety phrase from the offer + the device's response
/// (so the admin can compare it before approving). No side effects.
fn mutation_pair_phrase(body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let offer: concierge_core::PairingOffer = match pair_field(&value, "offer") {
        Ok(offer) => offer,
        Err(response) => return response,
    };
    let response: concierge_core::PairingResponse = match pair_field(&value, "response") {
        Ok(response) => response,
        Err(response) => return response,
    };
    Response::json(
        serde_json::json!({ "phrase": concierge_core::confirmation_phrase(&offer, &response) })
            .to_string(),
    )
}

/// Admin side, step 3: issue the scoped grant for the responding device.
fn mutation_pair_approve(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let response: concierge_core::PairingResponse = match pair_field(&value, "response") {
        Ok(response) => response,
        Err(resp) => return resp,
    };
    let scope = value
        .get("scope")
        .and_then(|s| s.as_str())
        .unwrap_or("full");
    let Some(descriptor) = mem.networks().ok().and_then(|n| n.into_iter().next()) else {
        return Response::bad_request("no network on this device");
    };
    let (namespace, ops) = pair_scope_caps(descriptor.network_id, scope);
    match mem.complete_pairing(
        &response,
        &[(namespace, ops)],
        concierge_core::DEFAULT_CERT_TTL_SECS,
    ) {
        Ok(grant) => Response::json(serde_json::json!({ "ok": true, "grant": grant }).to_string()),
        Err(error) => Response::error(error.to_string()),
    }
}

/// New device, step 2: verify + persist the grant. The device is now a scoped member.
fn mutation_pair_accept(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let grant: concierge_core::PairingGrant = match pair_field(&value, "grant") {
        Ok(grant) => grant,
        Err(response) => return response,
    };
    match mem.accept_pairing_grant(&grant) {
        Ok(()) => Response::json(
            serde_json::json!({
                "ok": true,
                "network": grant.descriptor.name,
                "capabilities": grant.capabilities.len(),
            })
            .to_string(),
        ),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Revoke a subject from the Data Platter (advances the epoch; remaining members
/// must be re-granted). Returns the refreshed network map.
fn mutation_network_revoke(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let subject = value
        .get("subject")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();
    if subject.is_empty() {
        return Response::bad_request("subject id is required");
    }
    let Some(descriptor) = mem.networks().ok().and_then(|n| n.into_iter().next()) else {
        return Response::bad_request("no network on this device");
    };
    match mem.revoke(&descriptor.network_id, subject) {
        Ok(_) => to_response(network_json(mem)),
        Err(error) => Response::error(error.to_string()),
    }
}

/// Convert a reviewed plaintext root into a capability-encrypted private graph,
/// immediately consume the exact private-share grant, and return the read-only
/// bearer capability to the local Data Platter handoff.
fn mutation_convert_private(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let review_token = match body_str(&value, "review_token") {
        Ok(token) => token,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    if value
        .get("acknowledge_private")
        .and_then(|value| value.as_bool())
        != Some(true)
    {
        return Response::bad_request(
            "private sharing requires destination and recipient acknowledgement",
        );
    }
    let plan = match options.reviewed_private_plan(review_token) {
        Some(plan) => plan,
        None => {
            return Response::bad_request(
                "private-share review expired or was not created by this Data Platter",
            )
        }
    };
    if let Err(error) = mem.create_encrypt_and_share_private_grant(&plan, password) {
        return mutation_error(&error);
    }
    match mem.convert_and_share_private(&plan, password) {
        Ok(result) => {
            options.discard_review(review_token);
            Response::json(
                serde_json::json!({
                    "converted": true,
                    "ciphertext_root": result.conversion.ciphertext_root,
                    "plaintext_root": result.conversion.plaintext_root,
                    "block_count": result.conversion.block_count,
                    "plaintext_locked": result.conversion.plaintext_locked,
                    "destination_namespace": result.conversion.destination_namespace,
                    "recipients": result.conversion.recipients,
                    "capability": result.capability,
                    "egress_receipt": result.receipt,
                })
                .to_string(),
            )
        }
        Err(error) => mutation_error(&error),
    }
}

pub(super) fn parse_body(body: &str) -> Result<serde_json::Value, Response> {
    serde_json::from_str(body).map_err(|_| Response::bad_request("invalid JSON body"))
}

pub(super) fn body_str<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str, Response> {
    value
        .get(key)
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .ok_or_else(|| Response::bad_request("missing required field"))
}

/// Directory names skipped when ingesting a folder/repo: build output and VCS
/// internals, which are noise rather than content.
const INGEST_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".next",
    "dist",
    "build",
    ".venv",
    "__pycache__",
    ".cache",
];

/// Accumulator for a file/folder ingest: the manifest entries plus tallies.
#[derive(Default)]
struct PathIngest {
    files: usize,
    bytes: u64,
    ignored: usize,
    ignored_examples: Vec<String>,
    entries: Vec<serde_json::Value>,
}

fn remember_ignored(acc: &mut PathIngest, rel: &str, reason: impl AsRef<str>) {
    acc.ignored += 1;
    if acc.ignored_examples.len() < 10 {
        acc.ignored_examples
            .push(format!("{rel}: {}", reason.as_ref()));
    }
}

/// Extension → media type, covering the documents/images/video/audio the user
/// is likely to ingest. Unknown types fall back to `application/octet-stream`.
fn guess_media_type_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "md" | "markdown" | "rs" | "toml" | "json" | "jsonl" | "ndjson" | "js" | "mjs"
        | "ts" | "tsx" | "jsx" | "py" | "go" | "c" | "h" | "cc" | "cpp" | "hpp" | "java" | "kt"
        | "rb" | "sh" | "bash" | "zsh" | "yml" | "yaml" | "html" | "htm" | "css" | "scss"
        | "csv" | "tsv" | "log" | "xml" | "ini" | "cfg" | "conf" | "sql" | "lock" | "gitignore" => {
            "text/plain"
        }
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "heic" => "image/heic",
        "tiff" | "tif" => "image/tiff",
        "pdf" => "application/pdf",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        _ => "application/octet-stream",
    }
}

/// Store one file as a `blob` + `file_ref`, returning the `file_ref` CID. There
/// is no size cap — any file is ingested whole. Unreadable files (or
/// directories/special files that `read` rejects) are tallied as ignored and
/// return `None`. Note blobs are stored as JSON byte arrays (~4× on disk), so
/// very large files amplify on-disk storage accordingly.
fn ingest_one_file(
    mem: &MemCli,
    scanner: &YaraScanner,
    abs: &std::path::Path,
    rel: &str,
    acc: &mut PathIngest,
) -> CoreResult<Option<Cid>> {
    let bytes = match std::fs::read(abs) {
        Ok(bytes) => bytes,
        Err(_) => {
            remember_ignored(acc, rel, "unreadable");
            return Ok(None);
        }
    };
    let report = scanner.scan_bytes(&bytes)?;
    if !report.clean() {
        let rules = report
            .matches
            .iter()
            .map(|m| m.rule.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        remember_ignored(acc, rel, format!("blocked by YARA rule(s): {rules}"));
        return Ok(None);
    }
    let media_type = guess_media_type_path(rel);
    let blob = mem.put_blob(&bytes, media_type)?;
    let fields = serde_json::json!({
        "path": rel,
        "size": bytes.len() as u64,
        "media_type": media_type,
        "content": cid_link(&blob)?,
    });
    let file_ref = mem.put_node(&Node {
        kind: "file_ref".to_string(),
        fields_json: fields.to_string(),
    })?;
    acc.entries
        .push(serde_json::json!({ "path": rel, "file_ref": cid_link(&file_ref)? }));
    acc.files += 1;
    acc.bytes += bytes.len() as u64;
    Ok(Some(file_ref))
}

/// Recursively store every regular file under `dir`, skipping symlinks and the
/// `INGEST_SKIP_DIRS` denylist. Paths in the manifest are relative to `base`.
fn walk_dir(
    mem: &MemCli,
    scanner: &YaraScanner,
    dir: &std::path::Path,
    base: &std::path::Path,
    acc: &mut PathIngest,
) -> CoreResult<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(_) => return Ok(()),
    };
    let mut children: Vec<_> = read.filter_map(std::result::Result::ok).collect();
    children.sort_by_key(std::fs::DirEntry::file_name);
    for entry in children {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if file_type.is_dir() {
            if INGEST_SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk_dir(mem, scanner, &path, base, acc)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            ingest_one_file(mem, scanner, &path, &rel, acc)?;
        }
    }
    Ok(())
}

/// A stable, groupable binding name for an ingest: `import:<unix>-<basename>`.
fn import_binding_name(path: &std::path::Path) -> String {
    let base = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "import".to_string());
    let safe: String = base
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("import:{ts}-{safe}")
}

/// Ingest a file or folder from a path on disk. The GUI is loopback-only on the
/// user's own machine, so the server reads the path directly — this is what
/// makes whole repos and large media practical (no browser upload). A single
/// `.jsonl`/`.ndjson` file is treated as a harness session stream; anything else
/// is stored as content-addressed blobs under a walkable `ingest_run` root.
fn mutation_ingest_path(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let raw = match body_str(&value, "path") {
        Ok(path) => path.trim(),
        Err(response) => return response,
    };
    if raw.is_empty() {
        return Response::bad_request("provide an absolute path to a file or folder");
    }
    let path = std::path::PathBuf::from(raw);
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(error) => return Response::bad_request(&format!("cannot access {raw}: {error}")),
    };

    if meta.is_dir() {
        let mut acc = PathIngest::default();
        let scanner = match mem.active_yara_scanner() {
            Ok(scanner) => scanner,
            Err(error) => return mutation_error(&error),
        };
        if let Err(error) = walk_dir(mem, &scanner, &path, &path, &mut acc) {
            return mutation_error(&error);
        }
        if acc.entries.is_empty() {
            return Response::bad_request(&format!(
                "no ingestible files under {raw} ({} ignored)",
                acc.ignored
            ));
        }
        let manifest_fields = serde_json::json!({ "root_path": raw, "entries": acc.entries });
        let manifest = match mem.put_node(&Node {
            kind: "directory_manifest".to_string(),
            fields_json: manifest_fields.to_string(),
        }) {
            Ok(cid) => cid,
            Err(error) => return mutation_error(&error),
        };
        let manifest_link = match cid_link(&manifest) {
            Ok(link) => link,
            Err(error) => return mutation_error(&error),
        };
        let run_fields = serde_json::json!({
            "source_path": raw,
            "manifest": manifest_link,
            "file_count": acc.files as u64,
            "byte_count": acc.bytes,
            "ignored_count": acc.ignored as u64,
            "plugin_records": 0,
            "plugin_failures": 0,
            "per_file_plugin_records": {},
            "per_file_plugin_failures": {},
        });
        let run = match mem.put_node(&Node {
            kind: "ingest_run".to_string(),
            fields_json: run_fields.to_string(),
        }) {
            Ok(cid) => cid,
            Err(error) => return mutation_error(&error),
        };
        let name = import_binding_name(&path);
        if let Err(error) = mem.bind(&name, &run) {
            return mutation_error(&error);
        }
        let preview = ingest_preview(&path, raw);
        let linked = mem
            .outbound_links(&run)
            .map(|l| !l.is_empty())
            .unwrap_or(false);
        return Response::json(
            serde_json::json!({
                "ok": true, "kind": "folder", "root": run.0, "name": name,
                "files": acc.files, "bytes": acc.bytes,
                "ignored": acc.ignored, "ignored_examples": acc.ignored_examples,
                "records": [appended_record(&run, "ingest_run", &preview, linked, &name)],
            })
            .to_string(),
        );
    }

    // Single file. A JSONL/NDJSON file is a harness session stream.
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "jsonl" || ext == "ndjson" {
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) => return Response::bad_request(&format!("cannot open {raw}: {error}")),
        };
        let base_dir = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let report = concierge_adapter_jsonl::ingest(std::io::BufReader::new(file), mem, &base_dir);
        return Response::json(
            serde_json::json!({
                "ok": true, "kind": "session",
                "events": report.events, "nodes_written": report.nodes_written,
                "names_bound": report.names_bound, "checkpoints": report.checkpoints,
                "skipped": report.skipped.len(),
            })
            .to_string(),
        );
    }

    let rel = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| raw.to_string());
    let mut acc = PathIngest::default();
    let scanner = match mem.active_yara_scanner() {
        Ok(scanner) => scanner,
        Err(error) => return mutation_error(&error),
    };
    let file_ref = match ingest_one_file(mem, &scanner, &path, &rel, &mut acc) {
        Ok(Some(cid)) => cid,
        Ok(None) => {
            return Response::bad_request(&format!(
                "skipped {raw}: {}",
                acc.ignored_examples
                    .first()
                    .map(String::as_str)
                    .unwrap_or("unreadable or blocked")
            ));
        }
        Err(error) => return mutation_error(&error),
    };
    let name = import_binding_name(&path);
    if let Err(error) = mem.bind(&name, &file_ref) {
        return mutation_error(&error);
    }
    let preview = ingest_preview(&path, raw);
    let linked = mem
        .outbound_links(&file_ref)
        .map(|l| !l.is_empty())
        .unwrap_or(false);
    Response::json(
        serde_json::json!({
            "ok": true, "kind": "file", "root": file_ref.0, "name": name,
            "files": acc.files, "bytes": acc.bytes, "ignored": acc.ignored,
            "records": [appended_record(&file_ref, "file", &preview, linked, &name)],
        })
        .to_string(),
    )
}

/// A readable preview for an ingested path: its basename (falls back to the raw path).
fn ingest_preview(path: &std::path::Path, raw: &str) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| raw.to_string())
}

/// Ingest an uploaded JSONL event stream into the store. The body is
/// `{ "content": "<jsonl text>" }`. File paths inside `file_*` events resolve
/// against the mounted store directory; missing files are skipped, never fatal.
fn mutation_ingest(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let content = match body_str(&value, "content") {
        Ok(content) => content,
        Err(response) => return response,
    };
    let base_dir = std::path::PathBuf::from(&options.store_label);
    let report = concierge_adapter_jsonl::ingest(
        std::io::BufReader::new(content.as_bytes()),
        mem,
        &base_dir,
    );
    Response::json(
        serde_json::json!({
            "ok": true,
            "lines": report.lines,
            "events": report.events,
            "nodes_written": report.nodes_written,
            "checkpoints": report.checkpoints,
            "names_bound": report.names_bound,
            "blobs_written": report.blobs_written.len(),
            "skipped": report.skipped.len(),
        })
        .to_string(),
    )
}

fn mutation_lock(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let label = value
        .get("label")
        .and_then(|item| item.as_str())
        .unwrap_or("");

    // Bulk path: `{ "session": "<id>" }` locks every named record in one session.
    if let Some(session) = value
        .get("session")
        .and_then(|item| item.as_str())
        .filter(|session| !session.is_empty())
    {
        match mem.password_is_set() {
            Ok(true) => {}
            Ok(false) => {
                return Response::bad_request(
                    "set and confirm a store password before creating the first GUI lock",
                );
            }
            Err(error) => return mutation_error(&error),
        }
        let names = match mem.names() {
            Ok(names) => names,
            Err(error) => return mutation_error(&error),
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut locked_count = 0usize;
        for (name, cid) in names {
            if session_of(&name).as_deref() != Some(session) {
                continue;
            }
            if !seen.insert(cid.0.clone()) {
                continue;
            }
            if mem.lock_subgraph(&cid, label).is_ok() {
                locked_count += 1;
            }
        }
        return Response::json(
            serde_json::json!({
                "locked": true,
                "session": session,
                "locked_count": locked_count,
            })
            .to_string(),
        );
    }

    let target = match body_str(&value, "target") {
        Ok(target) => target,
        Err(response) => return response,
    };
    let root = match resolve_target_string(mem, target) {
        Ok(root) => root,
        Err(error) => return mutation_error(&error),
    };
    match mem.password_is_set() {
        Ok(true) => {}
        Ok(false) => {
            return Response::bad_request(
                "set and confirm a store password before creating the first GUI lock",
            );
        }
        Err(error) => return mutation_error(&error),
    }
    let plan = match mem.build_egress_plan(&root, EgressOperation::PublicPublish) {
        Ok(plan) => plan,
        Err(error) => return mutation_error(&error),
    };
    match mem.lock_subgraph(&root, label) {
        Ok(()) => Response::json(
            serde_json::json!({
                "locked": true,
                "root": root.0,
                "reachable_node_count": plan.block_count,
                "file_count": plan.file_paths.len(),
            })
            .to_string(),
        ),
        Err(error) => mutation_error(&error),
    }
}

/// Permanently lift a lock (the egress-unlock) after the store password — this is
/// what allows a previously-locked subgraph to be published/shared/exported. The
/// bulk `{ "session": "<id>" }` form lifts the lock on every record in the
/// session; the single form takes a `{ "target": "<cid|name>" }`. Locks only ever
/// guarded egress, never local viewing.
fn mutation_unlock(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };

    // Bulk path: unlock every locked record in one session.
    if let Some(session) = value
        .get("session")
        .and_then(|item| item.as_str())
        .filter(|session| !session.is_empty())
    {
        let names = match mem.names() {
            Ok(names) => names,
            Err(error) => return mutation_error(&error),
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut unlocked_count = 0usize;
        for (name, cid) in names {
            if session_of(&name).as_deref() != Some(session) {
                continue;
            }
            if !seen.insert(cid.0.clone()) {
                continue;
            }
            // A bad/rate-limited password fails the whole batch; otherwise skip
            // records that simply have no direct lock to remove.
            match mem.unlock_subgraph(&cid, password) {
                Ok(()) => unlocked_count += 1,
                Err(
                    error @ (Error::AuthenticationFailed | Error::AuthenticationRateLimited { .. }),
                ) => {
                    return mutation_error(&error);
                }
                Err(_) => {}
            }
        }
        return Response::json(
            serde_json::json!({
                "unlocked": true,
                "session": session,
                "unlocked_count": unlocked_count,
            })
            .to_string(),
        );
    }

    let target = match body_str(&value, "target") {
        Ok(target) => target,
        Err(response) => return response,
    };
    let root = match resolve_target_string(mem, target) {
        Ok(root) => root,
        Err(error) => return mutation_error(&error),
    };
    match mem.unlock_subgraph(&root, password) {
        Ok(()) => {
            Response::json(serde_json::json!({ "unlocked": true, "root": root.0 }).to_string())
        }
        Err(error) => mutation_error(&error),
    }
}

/// Decision 0026: everything is fenced from egress by default. Clearing a root
/// is the explicit, password-gated exception that lets it be published / shared /
/// exported. Takes `{ "target": "<cid|name>", "password": "…", "label"?: "…" }`.
/// The password is read straight into the core call and never echoed.
fn mutation_clear_for_egress(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let target = match body_str(&value, "target") {
        Ok(target) => target,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    let label = value
        .get("label")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    let root = match resolve_target_string(mem, target) {
        Ok(root) => root,
        Err(error) => return mutation_error(&error),
    };
    let plan = match mem.build_egress_plan(&root, EgressOperation::PublicPublish) {
        Ok(plan) => plan,
        Err(error) => return mutation_error(&error),
    };
    match mem.clear_for_egress(&root, label, password) {
        Ok(()) => Response::json(
            serde_json::json!({
                "cleared": true,
                "root": root.0,
                "reachable_node_count": plan.block_count,
                "file_count": plan.file_paths.len(),
            })
            .to_string(),
        ),
        Err(error) => mutation_error(&error),
    }
}

/// Restore the default fence on a previously-cleared root (the safe direction —
/// no password needed to make data *more* private). Takes `{ "target": "<cid|name>" }`.
fn mutation_refence(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let target = match body_str(&value, "target") {
        Ok(target) => target,
        Err(response) => return response,
    };
    let root = match resolve_target_string(mem, target) {
        Ok(root) => root,
        Err(error) => return mutation_error(&error),
    };
    match mem.refence(&root) {
        Ok(()) => {
            Response::json(serde_json::json!({ "refenced": true, "root": root.0 }).to_string())
        }
        Err(error) => mutation_error(&error),
    }
}

fn mutation_set_password(mem: &MemCli, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    let confirm_password = match body_str(&value, "confirm_password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    if password != confirm_password {
        return Response::bad_request("password confirmation does not match");
    }
    match mem.set_password(password) {
        Ok(()) => Response::json(serde_json::json!({ "ok": true }).to_string()),
        Err(error) => mutation_error(&error),
    }
}

/// Publish exactly the short-lived server-cached plan identified by the review
/// drawer token. Locked plans mint and immediately consume a one-shot grant;
/// clear plans still require the store password and the same exact-plan check.
fn mutation_authorize_publish(mem: &MemCli, options: &GuiOptions, body: &str) -> Response {
    let value = match parse_body(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let password = match body_str(&value, "password") {
        Ok(password) => password,
        Err(response) => return response,
    };
    if value
        .get("acknowledge_irreversible")
        .and_then(|v| v.as_bool())
        != Some(true)
    {
        return Response::bad_request("publication requires an irreversibility acknowledgement");
    }
    let review_token = match body_str(&value, "review_token") {
        Ok(token) => token,
        Err(response) => return response,
    };
    let plan = match options.reviewed_plan(review_token) {
        Some(plan) => plan,
        None => {
            return Response::bad_request("review expired or was not created by this Data Platter")
        }
    };
    if plan.operation != EgressOperation::PublicPublish {
        return Response::bad_request("reviewed plan is not a public publication");
    }
    let authorization = if plan.is_blocked() {
        mem.create_publish_grant(&plan, password).map(|_| ())
    } else {
        mem.verify_password(password)
    };
    if let Err(error) = authorization {
        return mutation_error(&error);
    }
    match mem.publish_public(&plan) {
        Ok(receipt) => {
            options.discard_review(review_token);
            Response::json(
                serde_json::json!({
                    "published": true,
                    "root": receipt.root,
                    "backend": receipt.backend,
                    "gateway_url": receipt.gateway_url,
                    "authorization_consumed": true,
                })
                .to_string(),
            )
        }
        Err(error) => mutation_error(&error),
    }
}

fn resolve_target_string(mem: &MemCli, target: &str) -> CoreResult<Cid> {
    if target.parse::<cid::Cid>().is_ok() {
        Ok(Cid(target.to_string()))
    } else {
        mem.resolve(target)
    }
}

/// Map a core error to an HTTP status, never leaking secret material.
fn mutation_error(error: &Error) -> Response {
    let (status, message): (u16, String) = match error {
        Error::AuthenticationFailed => (401, "store password authentication failed".to_string()),
        Error::AuthenticationRateLimited { retry_after_secs } => (
            429,
            format!("authentication rate limited; retry in {retry_after_secs}s"),
        ),
        Error::PublicationBlocked { .. }
        | Error::SensitiveContentBlocked { .. }
        | Error::SecurityPolicy(_)
        | Error::GrantIntegrity(_)
        | Error::ExplicitPublicPublishRequired => (403, error.to_string()),
        Error::EgressPlanChanged(_) => (409, error.to_string()),
        // A closed/wrong-password vault surfaces as an encryption error; treat it
        // as forbidden rather than a server fault.
        Error::Encryption(_) => (403, error.to_string()),
        Error::NameUnbound(_) | Error::CidNotFound(_) | Error::Tombstoned(_) => {
            (404, error.to_string())
        }
        Error::BackendDown(_) => (502, error.to_string()),
        Error::Unsupported { .. } => (400, error.to_string()),
        _ => (500, error.to_string()),
    };
    Response::json_error(status, &message)
}
