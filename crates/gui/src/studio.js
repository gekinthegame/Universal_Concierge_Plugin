// ── Studio: live website builder + publish + educational live-share ──────────
// Open a site folder; it renders live (multi-file) at /canvas-preview/<token>/ and
// hot-reloads as files change (you or your AI write them). Publish the folder to a
// stable IPNS site. "Share live" broadcasts the page to viewers for example displays.
// One unified writeable canvas: pick a project under the Concierge canvas folder, edit
// its files in the editor
// (saves straight to disk), and the preview always renders the live website. No more
// separate Write vs Folder modes — writing IS editing the open folder.
const cv = { token: null, folder: null, mtime: 0, poll: null, viewing: false, file: "index.html", collapsed: new Set(), lastFiles: [], lastHasIndex: false };
function cvStatus(t) { const el = byId("cv-status"); if (el) el.textContent = t; }
function randId() { return Math.random().toString(36).slice(2, 10); }
function cvBasename(p) { return (p || "").replace(/\/+$/, "").split("/").pop() || ""; }

// The preview always shows the live folder render; a fresh cache-buster on every
// refresh so edits (yours or the AI's) appear immediately.
let cvBust = 0;
function cvShowLive() {
  cv.viewing = false;
  if (!cv.token) { cvIdlePreview(); return; }
  byId("cv-preview").removeAttribute("srcdoc");
  byId("cv-preview").src = "/canvas-preview/" + cv.token + "/?t=" + (++cvBust);
}
// Before any folder is open, the preview is a gentle placeholder.
function cvIdlePreview() {
  byId("cv-preview").removeAttribute("src");
  byId("cv-preview").srcdoc = "<!doctype html><meta charset=utf-8><body style='font:14px system-ui;color:#888;display:grid;place-items:center;height:100vh;margin:0;text-align:center;padding:24px'><div>Open a saved canvas project, or just start building —<br>your app or site saves to the Concierge canvas folder and runs here, live.</div></body>";
}

// Editing the textarea saves the open file to disk (debounced); the preview then
// hot-reloads. With no folder open yet, the first keystroke creates and opens a
// default draft folder so you can simply start typing.
let cvSrcDebounce;
byId("cv-src").addEventListener("input", () => {
  clearTimeout(cvSrcDebounce);
  cvSrcDebounce = setTimeout(() => safely(cvCommitEditor), 300);
});
async function cvCommitEditor() {
  const content = byId("cv-src").value;
  if (!cv.token) {
    if (!content.trim()) return;                 // nothing to stage yet
    await cvEnsureFolder(content);               // create + open a draft folder seeded with this HTML
  } else {
    await postJson("/api/canvas/write", { token: cv.token, path: cv.file || "index.html", content });
  }
  cvShowLive();
  if (cvRtc.role === "host") cvBroadcast();
  cvStatus("saved · " + (cv.file || "index.html"));
}
// Create + open a default working folder under the store, so "just start typing" still
// works — now unified onto the folder canvas. Seeds index.html with `html`.
async function cvEnsureFolder(html) {
  if (cv.token) return;
  const name = byId("cv-name").value.trim() || "draft";
  const snap = await postJson("/api/canvas/snapshot", { session: name, html: html || "" });
  byId("cv-folder").value = snap.folder;
  cv.file = "index.html";
  await cvOpen({ keepEditor: true });
}

// The file list is a real file-tree explorer: folders nest and collapse, files open
// in the editor. Folders are open by default; clicking a folder collapses it.
function cvBuildTree(files) {
  const root = { dirs: {}, files: [] };
  files.forEach(path => {
    const parts = path.split("/");
    let cur = root;
    for (let i = 0; i < parts.length - 1; i++) {
      const seg = parts[i];
      if (!cur.dirs[seg]) cur.dirs[seg] = { dirs: {}, files: [], path: parts.slice(0, i + 1).join("/") };
      cur = cur.dirs[seg];
    }
    cur.files.push({ name: parts[parts.length - 1], path });
  });
  return root;
}
function cvRenderBranch(box, branch, depth) {
  Object.keys(branch.dirs).sort().forEach(name => {
    const dir = branch.dirs[name];
    const open = !cv.collapsed.has(dir.path);
    const row = node("div", "cv-file cv-dir", (open ? "▾ " : "▸ ") + name);
    row.style.paddingLeft = (depth * 11 + 5) + "px";
    row.addEventListener("click", () => {
      if (open) cv.collapsed.add(dir.path); else cv.collapsed.delete(dir.path);
      cvRenderFiles(cv.lastFiles, cv.lastHasIndex);
    });
    box.append(row);
    if (open) cvRenderBranch(box, dir, depth + 1);
  });
  branch.files.forEach(f => {
    const el = node("div", "cv-file" + (f.name === "index.html" ? " index" : "") + (f.path === cv.file ? " active" : ""), f.name);
    el.style.paddingLeft = (depth * 11 + 5) + "px";
    el.title = f.path;
    el.addEventListener("click", () => safely(() => cvSelectFile(f.path)));
    box.append(el);
  });
}
function cvRenderFiles(files, hasIndex) {
  cv.lastFiles = files || []; cv.lastHasIndex = hasIndex;
  const box = byId("cv-files"); clear(box);
  if (!cv.token) { box.append(node("div", "empty", "Open a saved canvas project above — its file tree appears here. Click a file to edit it; what you write saves to the folder and the preview renders the live app.")); return; }
  if (!files || !files.length) { box.append(node("div", "empty", "Folder is empty — start typing to create index.html.")); return; }
  if (!hasIndex) box.append(node("div", "empty", "⚠ no index.html — the live preview renders a web entry point; add one to see the app run."));
  cvRenderBranch(box, cvBuildTree(files), 0);
}
// Load a file from the open folder into the editor for editing.
async function cvSelectFile(path) {
  if (!cv.token) return;
  const d = await getJson("/api/canvas/file?token=" + cv.token + "&path=" + encodeURIComponent(path));
  byId("cv-src").value = typeof d.content === "string" ? d.content : "";
  cv.file = d.path || path;
  cvStatus("editing · " + cv.file);
  try { const f = await getJson("/api/canvas/files?token=" + cv.token); cvRenderFiles(f.files, f.has_index); } catch (e) {}
}

async function cvOpen(opts) {
  const folder = byId("cv-folder").value.trim();
  if (!folder) { notice("Enter or pick the folder to use as your canvas."); return; }
  const res = await postJson("/api/canvas/open", { folder });
  cv.token = res.token; cv.folder = folder; cv.mtime = res.mtime;
  if (!byId("cv-name").value.trim()) byId("cv-name").value = cvBasename(folder).toLowerCase().replace(/[^a-z0-9._-]+/g, "-");
  cvUpdateDomain();
  // Load index.html into the editor so the folder is immediately writeable — unless we
  // were told to keep what the user just typed (cvEnsureFolder seeded it already).
  if (!(opts && opts.keepEditor)) {
    cv.file = "index.html";
    if (res.has_index) { await cvSelectFile("index.html"); }
    else { byId("cv-src").value = ""; cvRenderFiles(res.files, res.has_index); }
  } else {
    cvRenderFiles(res.files, res.has_index);
  }
  cvShowLive();
  cvStatus(res.has_index ? (res.files.length + " file(s) · live") : "no index.html yet — start typing to create one");
  startCvPoll();
  logSystem("studio · opened " + folder, "ok");
}
async function cvPoll() {
  if (!cv.token || cv.viewing) return;
  let res; try { res = await getJson("/api/canvas/mtime?token=" + cv.token); } catch (e) { return; }
  if (res.mtime && res.mtime !== cv.mtime) {
    cv.mtime = res.mtime;
    cvShowLive();                                   // hot reload
    try { const f = await getJson("/api/canvas/files?token=" + cv.token); cvRenderFiles(f.files, f.has_index); } catch (e) {}
    // Reflect external edits (e.g. the AI rewriting a file) in the editor too — but
    // never clobber what you're actively typing.
    if (document.activeElement !== byId("cv-src")) quietly(() => cvSelectFile(cv.file || "index.html"));
    if (cvRtc.role === "host") cvBroadcast();        // push to educational viewers
  }
}
function startCvPoll() { if (cv.poll) clearInterval(cv.poll); cv.poll = setInterval(() => quietly(cvPoll), 1000); }
cvIdlePreview();
async function cvPublish(platform = "ipfs") {
  const name = byId("cv-name").value.trim();
  if (!name) { byId("cv-name").focus(); notice("Give your site a name."); return; }
  // Connect gate: Web2 hosts need an account. If this one isn't connected yet,
  // open the guided connect walk-through instead of failing the publish.
  if (platform !== "ipfs") {
    let status = {}; try { status = await getJson("/api/deploy/credentials"); } catch (e) {}
    if (!status[platform]) {
      notice("Connect your " + (DEPLOY_META[platform] ? DEPLOY_META[platform].name : platform) + " account to publish there.");
      depOpen(platform);
      return;
    }
  }
  if (!cv.folder) {
    const html = byId("cv-src").value;
    if (!html.trim()) { notice("Open a folder or write your site first."); return; }
    await cvEnsureFolder(html);   // create + open a draft folder seeded with the editor's HTML
  }
  const folder = cv.folder;
  await cvCaptureAnimationIfNeeded();   // animation projects get a video baked in before publish
  // Only websites publish: the folder needs an index.html web entry point. Check live so
  // non-web projects (a CLI, a service, raw source) get a clear message, not a publish.
  let hasIndex = cv.lastHasIndex;
  if (cv.token) { try { hasIndex = (await getJson("/api/canvas/files?token=" + cv.token)).has_index; } catch (e) {} }
  if (!hasIndex) { notice("Only websites can be published — this folder has no index.html to load as a page. Add a web entry point (index.html) and try again."); return; }
  const review = await postJson("/api/site/deploy-plan", { name, folder, kind: "site", platform });
  const plan = review.plan;
  const summary = [
    "Review exact deployment",
    "",
    "Platform: " + plan.platform,
    "Destination: " + plan.destination,
    "Files: " + plan.manifest.length,
    "Bytes: " + plan.total_bytes,
    "Manifest SHA-256: " + plan.manifest_digest,
    "",
    "Publish only if this exact destination and manifest are expected."
  ].join("\n");
  if (!window.confirm(summary)) return;
  const password = byId("cv-password").value;
  if (!password) { byId("cv-password").focus(); notice("Your store password is required after reviewing the exact egress plan."); return; }
  notice("Publishing to " + platform.toUpperCase() + "…");
  logSystem("studio · publishing " + name + " to " + platform, "ok");
  const res = await postJson("/api/site/publish", { review_token: review.review_token, password });
  byId("cv-password").value = "";
  if (res.url) {
    logSystem("studio · live at " + (res.ipns || res.url), "ok");
    if (res.ipns) cv.lastIpns = res.ipns;
    // IPFS is served from your own node, so a shared link only loads if that node is
    // reachable — tell the user honestly and pop the fix-it instructions if not. Web2
    // hosts (Firebase, Cloudflare, …) serve the site themselves, so this never applies.
    if (platform === "ipfs") {
      notice("Published. View it now with “Open published site” (served from your node).");
      try {
        const r = await getJson("/api/publish/reachability");
        if (r.reachable) {
          logSystem("publish · node is publicly reachable — a shared link will load for others", "ok");
        } else {
          logSystem("publish · node " + (r.relay_only ? "relay-only" : "NOT publicly reachable") + " — see the popup to fix", "warn");
          reachOpen(r);
        }
      } catch (e) {}
    } else {
      notice("Published — live at " + res.url);
    }
  } else {
    notice("Publishing initiated for " + platform.toUpperCase() + ". Check your dashboard.");
  }
  await cvLoadSites();
}

// ── Reachability instructions popup: shown after publish when a shared link won't
// reach others (node not publicly reachable). Explicit, do-one-of-these steps. ──
function reachOpen(r) {
  const port = (r && r.swarm_port) || 4011;
  const how = r && r.relay_only ? "only relay-reachable (public gateways often can't dial a relay)" : "not publicly reachable";
  const status = byId("reach-status"); clear(status);
  status.appendChild(document.createTextNode(
    "Your site is published and already works for you (Open published site). But your node is " + how +
    ", so a link you send to someone else won't reliably load. Do ONE of these, then restart the node and click Recheck:"));
  const steps = byId("reach-steps"); clear(steps);
  const ol = node("ol", "reach-list");
  [
    ["Enable UPnP on your router — easiest.",
     "Open your router's admin page (often http://192.168.1.1), find UPnP / NAT-PMP, and turn it on. Then restart the node here. Many routers have this on already."],
    ["Port-forward port " + port + " — both TCP and UDP — most reliable.",
     "In your router settings, forward TCP and UDP port " + port + " to this computer's local IP address."],
    ["Pin to an always-on peer — bulletproof, no router changes.",
     "Pin the site to a reachable Kubo node (a cheap VPS), a pinning service, or an IPFS Cluster peer — then the link loads regardless of your home node."],
  ].forEach(([h, d]) => {
    const li = node("li", "");
    li.appendChild(node("b", "", h));
    li.appendChild(node("div", "eyebrow", d));
    ol.appendChild(li);
  });
  steps.appendChild(ol);
  byId("reach-modal").style.display = "flex";
}
function reachClose() { byId("reach-modal").style.display = "none"; }
async function reachRecheck() {
  let r; try { r = await getJson("/api/publish/reachability"); } catch (e) { return; }
  if (r.reachable) { reachClose(); notice("✓ Your node is now publicly reachable — shared links will load for others."); }
  else { reachOpen(r); notice("Still " + (r.relay_only ? "relay-only" : "not reachable") + " — detection can lag a minute after a node restart."); }
}
byId("reach-close").addEventListener("click", reachClose);
byId("reach-ok").addEventListener("click", reachClose);
byId("reach-recheck").addEventListener("click", () => safely(reachRecheck));
byId("reach-view").addEventListener("click", () => cvOpenPublished());
byId("reach-modal").addEventListener("click", e => { if (e.target === byId("reach-modal")) reachClose(); });
document.addEventListener("keydown", e => { if (e.key === "Escape" && byId("reach-modal").style.display === "flex") reachClose(); });

// ── AI write-access toggle ──
async function cvLoadMcp() {
  let s; try { s = await getJson("/api/mcp/status"); } catch (e) { return; }
  const b = byId("cv-mcp"); b.classList.toggle("on", !!s.write_enabled);
  b.textContent = "AI writes: " + (s.write_enabled ? "ON" : "off");
}
byId("cv-mcp").addEventListener("click", () => safely(async () => {
  const on = byId("cv-mcp").classList.contains("on");
  await postJson("/api/mcp/write", { enabled: !on });
  await cvLoadMcp();
  notice(!on
    ? "Your AI can now write into the Concierge (MCP write tools enabled)."
    : "AI write access turned off — the AI has read tools only.");
  logSystem("mcp write tools " + (!on ? "enabled" : "disabled"), !on ? "warn" : "ok");
}));
async function cvLoadSites() {
  await cvLoadMcp();
  let data; try { data = await getJson("/api/sites"); } catch (e) { return; }
  cv.sites = data.sites || [];
  cvUpdateDomain();
}
// Show the actual IPFS address the site publishes to. A site lives at a stable
// IPNS name (k51…): /ipns/<k51> on any gateway, or <k51>.ipns.dweb.link. The human
// "site name" is just the local label/key; the public address is the k51. If a
// published site already matches the typed name, show its REAL address.
const CV_EXAMPLE_IPNS = "k51qzi5uqu5dlvj2baxnqndepeb86cbk3ng7n3i46uzyxzyqj2xjonzllnv0v8";
function cvIpnsShort(k) { return k.length > 22 ? k.slice(0, 14) + "…" + k.slice(-4) : k; }
function cvUpdateDomain() {
  const el = byId("cv-domain"); if (!el) return;
  const name = byId("cv-name").value.trim().toLowerCase();
  const match = (cv.sites || []).find(s => (s.name || "").toLowerCase() === name && s.ipns);
  clear(el);
  if (match) {
    const fullUrl = "https://ipfs.io/ipns/" + match.ipns + "/";
    const dwebUrl = "https://" + match.ipns + ".ipns.dweb.link/";
    const braveUrl = "ipns://" + match.ipns + "/";

    el.classList.add("live");
    el.append(document.createTextNode("✓ live at "));

    const link = node("a", "cv-link", "ipfs.io/ipns/" + cvIpnsShort(match.ipns));
    link.href = fullUrl; link.target = "_blank"; link.title = "View on public gateway";
    el.append(link);

    el.append(document.createTextNode("  ·  also "));
    const dlink = node("a", "cv-link", "dweb.link");
    dlink.href = dwebUrl; dlink.target = "_blank"; dlink.title = "View on dweb.link gateway";
    el.append(dlink);

    if (cvIsBrave) {
      el.append(document.createTextNode("  ·  "));
      const blink = node("a", "cv-link brave", "Open in Brave");
      blink.href = braveUrl; blink.target = "_blank"; blink.title = "Open sovereignly in Brave (ipns://)";
      el.append(blink);
    }

    const copy = node("button", "cv-copy", "Copy URL");
    copy.title = "Copy full IPFS gateway URL to clipboard";
    copy.onclick = () => {
      navigator.clipboard.writeText(fullUrl).then(() => {
        const old = copy.textContent; copy.textContent = "Copied!";
        setTimeout(() => copy.textContent = old, 1500);
      });
    };
    el.append(copy);
  } else {
    el.classList.remove("live");
    el.append(document.createTextNode((name ? "“" + name + "”" : "your site") + " publishes to a permanent IPFS address — "));
    el.append(node("b", "", "ipfs.io/ipns/" + cvIpnsShort(CV_EXAMPLE_IPNS)));
    el.append(document.createTextNode(" (your unique k51… address is assigned on Publish)"));
  }
}
byId("cv-name").addEventListener("input", cvUpdateDomain);
cvUpdateDomain();

// Studio publish checkpoints: every Publish snapshots the page (timestamped + its
// stable /ipns/ address). Reopen any past version to edit and re-publish to the
// SAME address.
async function cvOpenCheckpoints() {
  let data; try { data = await getJson("/api/site/checkpoints"); } catch (e) { notice("Couldn't load checkpoints."); return; }
  const list = data.checkpoints || [];
  const old = byId("cv-ckpt-overlay"); if (old) old.remove();
  const overlay = node("div", "modal-overlay"); overlay.id = "cv-ckpt-overlay";
  const card = node("div", "pform modal-card");
  card.append(node("div", "modal-title", "Studio checkpoints"));
  if (!list.length) {
    card.append(node("div", "review", "No checkpoints yet. Click 💾 Save checkpoint anytime, or Publish a site — each saves a timestamped snapshot here you can reopen."));
  } else {
    card.append(node("div", "review", "Every 💾 Save checkpoint and every Publish is snapshotted (with a real CID in Records). Reopen any version to edit; re-publishing keeps the same /ipns/ address. Published versions show a copyable live URL."));
    const box = node("div", "ckpt-list");
    list.forEach(c => {
      const row = node("div", "ckpt-row");
      const when = c.ts ? new Date(c.ts * 1000).toLocaleString() : "—";
      const meta = node("div", "ckpt-meta");
      meta.append(node("div", "ckpt-when", when));
      // The public, shareable URL: the sovereign ipfs.io/ipns address for IPFS sites,
      // else the host's deploy URL (GitHub Pages / Netlify / …). Empty = save-only draft.
      const url = c.ipns ? ("https://ipfs.io/ipns/" + c.ipns + "/") : (c.url || "");
      meta.append(node("div", "ckpt-sub", c.site + (url ? "  ·  published" : "  ·  local draft — not published")));
      if (url) {
        const urlRow = node("div", "ckpt-url");
        const link = node("a", "cv-link", url);
        link.href = url; link.target = "_blank"; link.rel = "noopener noreferrer"; link.title = url;
        const copy = node("button", "cv-copy", "Copy URL");
        copy.title = "Copy the published site URL to clipboard";
        copy.onclick = () => {
          navigator.clipboard.writeText(url).then(() => {
            const old = copy.textContent; copy.textContent = "Copied!";
            setTimeout(() => copy.textContent = old, 1500);
          }).catch(() => notice("Copy failed — select the URL and copy it manually."));
        };
        urlRow.append(link, copy);
        meta.append(urlRow);
      }
      const load = node("button", "pbtn ckpt-load", "Load to edit");
      load.addEventListener("click", () => safely(() => cvRestoreCheckpoint(c.site, c.ts, overlay)));
      row.append(meta, load); box.append(row);
    });
    card.append(box);
  }
  const actions = node("div", "modal-actions");
  const close = node("button", "pbtn", "Close"); close.type = "button";
  close.addEventListener("click", () => overlay.remove());
  actions.append(close); card.append(actions);
  overlay.append(card);
  overlay.addEventListener("click", e => { if (e.target === overlay) overlay.remove(); });
  document.body.append(overlay);
}
async function cvRestoreCheckpoint(site, ts, overlay) {
  const d = await getJson("/api/site/checkpoint?site=" + encodeURIComponent(site) + "&ts=" + encodeURIComponent(ts));
  if (typeof d.html !== "string") { notice("Checkpoint has no saved HTML."); return; }
  byId("cv-name").value = site;
  // Stage the restored HTML into a folder and open it as the live writeable canvas.
  const snap = await postJson("/api/canvas/snapshot", { session: site, html: d.html });
  byId("cv-folder").value = snap.folder;
  cv.token = null; cv.folder = null;   // force a fresh open of the restored folder
  await cvOpen();
  cvUpdateDomain();
  if (overlay) overlay.remove();
  notice("Loaded the " + new Date(ts * 1000).toLocaleString() + " version of “" + site + "” — edit and Publish to update the same address.");
}
// Save a checkpoint of the current draft at any time (no publish). Content-addresses
// the HTML so the snapshot has a real CID in Records, and lists it under ⏱ Checkpoints
// to reopen later.
// For an animation project (one with capture.js), silently render the <canvas> to a video
// and drop it in the folder before Save/Publish — so a video is produced automatically,
// no Record button, no screen picker. Other projects are untouched.
async function cvCaptureAnimationIfNeeded() {
  if (!cv.token) return;
  const isAnim = (cv.lastFiles || []).some(f => String(f).split("/").pop() === "capture.js");
  if (!isAnim) return;
  const iframe = byId("cv-preview");
  if (!iframe || !iframe.contentWindow) return;
  cvStatus("rendering animation to video…");
  // The renderer streams chunks ({phase:'chunk', position, buf}) as it encodes; we write
  // each to disk at its offset and ack so the next is sent (bounded memory). Progress
  // ({phase:'render'}) updates the status; {phase:'done'} finishes. No fixed length.
  const abToB64 = (ab) => { const b = new Uint8Array(ab), CH = 0x8000, s = []; for (let i = 0; i < b.length; i += CH) s.push(String.fromCharCode.apply(null, b.subarray(i, i + CH))); return btoa(s.join("")); };
  const ok = await new Promise(resolve => {
    let settled = false, watchdog, writing = Promise.resolve();
    const finish = (v) => { if (!settled) { settled = true; clearTimeout(watchdog); window.removeEventListener("message", onMsg); resolve(v); } };
    const arm = (ms) => { clearTimeout(watchdog); watchdog = setTimeout(() => finish(false), ms); };
    function onMsg(e) {
      const d = e.data || {};
      if (d.concierge !== "capture") return;
      if (d.phase === "render") { arm(60000); cvStatus("rendering video · frame " + d.frame + " / " + d.total); }
      else if (d.phase === "chunk") {
        arm(60000);
        const buf = d.buf, pos = d.position;
        writing = writing
          .then(() => postJson("/api/canvas/write", { token: cv.token, path: "animation.webm", content: abToB64(buf), base64: true, pos: pos }))
          .catch(() => {})
          .then(() => { try { iframe.contentWindow.postMessage({ concierge: "chunk-ack" }, "*"); } catch (e) {} });
      } else if (d.phase === "done") { writing.then(() => finish(!!d.ok)); }
    }
    window.addEventListener("message", onMsg);
    arm(30000); // 30s to start, then 60s between pings/chunks
    try { iframe.contentWindow.postMessage({ concierge: "record" }, "*"); } catch (e) { finish(false); }
  });
  if (ok) {
    cvStatus("animation saved · animation.webm");
    logSystem("studio · streamed full animation → animation.webm", "ok");
  } else {
    cvStatus("animation render skipped");
  }
}

async function cvSaveCheckpoint() {
  const name = byId("cv-name").value.trim();
  if (!name) { byId("cv-name").focus(); notice("Give your site a name first."); return; }
  if (!cv.folder) {
    const html = byId("cv-src").value;
    if (!html.trim()) { notice("Open a folder or write your site first."); return; }
    await cvEnsureFolder(html);   // create + open a draft folder seeded with the editor's HTML
  }
  await cvCaptureAnimationIfNeeded();   // animation projects get a video, automatically
  const res = await postJson("/api/site/checkpoint/save", { name, folder: cv.folder });
  notice("Checkpoint saved" + (res.cid ? " · " + String(res.cid).slice(0, 14) + "…" : "") + " — in ⏱ Checkpoints and Records.");
  logSystem("studio · saved checkpoint “" + name + "”", "ok");
  if (byId("cv-ckpt-overlay")) safely(cvOpenCheckpoints); // refresh the list if it's open
}
byId("cv-save-ckpt").addEventListener("click", () => safely(cvSaveCheckpoint));
byId("cv-ckpts-btn").addEventListener("click", () => safely(cvOpenCheckpoints));
// B-Portal: reveal a published site's native ipns:// link only when running in Brave
// (it resolves ipns:// via a local node). Read by cvUpdateDomain.
let cvIsBrave = false;
(async () => {
  try { cvIsBrave = !!(navigator.brave && await navigator.brave.isBrave()); } catch (e) {}
})();
// Open the most recently published site through YOUR OWN node's local gateway (it has
// the content). Used by the "make reachable" modal. ipns:// would route a public gateway
// that can't find a local-only, NAT'd node, so we go straight to the local gateway.
function cvOpenPublished() {
  const ipns = cv.lastIpns || "";
  if (!ipns) { notice("Publish a site first."); return; }
  window.open("http://127.0.0.1:8090/ipns/" + ipns + "/", "_blank");
  logSystem("studio · opened " + String(ipns).slice(0, 12) + "… via your node's local gateway", "ok");
}
// Open a folder directly from the path field (Enter), or browse the saved projects
// under your canvas folder via the Open button's picker.
async function cvOpenFolder(path) {
  byId("cv-folder").value = path;
  cv.token = null; cv.folder = null;   // force a fresh open of the chosen folder
  await cvOpen();
}
async function cvOpenPicker() {
  let data; try { data = await getJson("/api/canvas/projects"); } catch (e) { notice("Couldn't list your projects."); return; }
  const existing = byId("cv-pick-overlay"); if (existing) existing.remove();
  const overlay = node("div", "modal-overlay"); overlay.id = "cv-pick-overlay";
  const card = node("div", "pform modal-card");
  card.append(node("div", "modal-title", "Open a project"));
  card.append(node("div", "review", "Saved projects in " + (data.root || "your canvas folder") + " — click one to open it as the live canvas."));
  const projects = data.projects || [];
  if (!projects.length) {
    card.append(node("div", "review", "No projects here yet. Start writing or create a new project to stage one here."));
  } else {
    const list = node("div", "ckpt-list");
    projects.forEach(p => {
      const row = node("div", "ckpt-row"); row.style.cursor = "pointer";
      const meta = node("div", "ckpt-meta");
      meta.append(node("div", "ckpt-when", "📁 " + p.name + (p.has_index ? "" : "  ·  ⚠ no index.html")));
      meta.append(node("div", "ckpt-sub", p.files + " file" + (p.files === 1 ? "" : "s") + (p.mtime ? "  ·  " + new Date(p.mtime * 1000).toLocaleString() : "")));
      const open = node("button", "pbtn ckpt-load", "Open");
      const go = () => safely(async () => { overlay.remove(); await cvOpenFolder(p.path); });
      open.addEventListener("click", e => { e.stopPropagation(); go(); });
      const del = node("button", "pbtn ckpt-del", "🗑 Delete");
      del.title = "Permanently delete this project folder and its files";
      del.addEventListener("click", e => {
        e.stopPropagation();
        safely(async () => {
          if (!window.confirm("Delete project “" + p.name + "”? This permanently removes its folder and all its files — this cannot be undone.")) return;
          await postJson("/api/canvas/delete", { name: p.name });
          notice("Deleted “" + p.name + "”.");
          logSystem("studio · deleted project " + p.name, "wn");
          // If the deleted project was the one open in the canvas, clear it.
          if (cv.folder && (cv.folder === p.path || String(cv.folder).replace(/\/+$/, "").endsWith("/" + p.name))) {
            cv.token = null; cv.folder = null; byId("cv-folder").value = "";
          }
          overlay.remove();
          safely(cvOpenPicker);   // reopen with the refreshed list
        });
      });
      row.addEventListener("click", go);
      row.append(meta, open, del); list.append(row);
    });
    card.append(list);
  }
  const actions = node("div", "modal-actions");
  const manual = node("input", "input"); manual.placeholder = "…or paste a path under this canvas folder + Enter"; manual.style.flex = "1";
  manual.addEventListener("keydown", e => { if (e.key === "Enter" && manual.value.trim()) safely(async () => { overlay.remove(); await cvOpenFolder(manual.value.trim()); }); });
  const close = node("button", "pbtn", "Close"); close.type = "button";
  close.addEventListener("click", () => overlay.remove());
  actions.append(manual, close); card.append(actions);
  overlay.append(card);
  overlay.addEventListener("click", e => { if (e.target === overlay) overlay.remove(); });
  document.body.append(overlay);
}
byId("cv-open").addEventListener("click", () => safely(cvOpenPicker));
byId("cv-folder").addEventListener("keydown", e => { if (e.key === "Enter") safely(cvOpen); });
document.addEventListener("keydown", e => { if (e.key === "Escape" && byId("cv-pick-overlay")) byId("cv-pick-overlay").remove(); });

byId("cv-publish").addEventListener("click", () => safely(() => cvPublish("ipfs")));
byId("cv-pub-ipfs").addEventListener("click", () => safely(() => cvPublish("ipfs")));
byId("cv-pub-github").addEventListener("click", () => safely(() => cvPublish("github")));
byId("cv-pub-netlify").addEventListener("click", () => safely(() => cvPublish("netlify")));
byId("cv-pub-vercel").addEventListener("click", () => safely(() => cvPublish("vercel")));
byId("cv-pub-cloudflare").addEventListener("click", () => safely(() => cvPublish("cloudflare")));
byId("cv-pub-firebase").addEventListener("click", () => safely(() => cvPublish("firebase")));

// Pin the site to an always-on pinning service (Filebase / Pinata / 4everland / any
// PSA endpoint), so the /ipns/ link stays reachable even when this node is offline.
async function cvPin(service) {
  const name = byId("cv-name").value.trim();
  if (!name) { byId("cv-name").focus(); notice("Give your site a name."); return; }
  // Connect gate: if this service isn't set up yet, open the guided walk-through.
  let status = {}; try { status = await getJson("/api/pin/credentials"); } catch (e) {}
  if (!status[service]) {
    notice("Connect your " + (PIN_META[service] ? PIN_META[service].name : service) + " account to pin there.");
    pinOpen(service);
    return;
  }
  if (!cv.folder) {
    const html = byId("cv-src").value;
    if (!html.trim()) { notice("Open a folder or write your site first."); return; }
    await cvEnsureFolder(html);
  }
  const folder = cv.folder;
  // Only websites can be pinned (the IPNS link needs a web entry point).
  let hasIndex = cv.lastHasIndex;
  if (cv.token) { try { hasIndex = (await getJson("/api/canvas/files?token=" + cv.token)).has_index; } catch (e) {} }
  if (!hasIndex) { notice("Only websites can be pinned — this folder has no index.html. Add a web entry point and try again."); return; }
  const password = byId("cv-password").value;
  if (!password) { byId("cv-password").focus(); notice("Your store password is required — pinning is public egress."); return; }
  cvStatus("pinning to " + (PIN_META[service] ? PIN_META[service].name : service) + "…");
  notice("Pinning to " + (PIN_META[service] ? PIN_META[service].name : service) + "… (publishing to IPFS + uploading to the service)");
  const res = await postJson("/api/site/pin", { name, folder, service, password });
  byId("cv-password").value = "";
  if (res.ipns) cv.lastIpns = res.ipns;
  cvStatus("pinned · " + (res.status || "queued"));
  notice("Pinned to " + (PIN_META[service] ? PIN_META[service].name : service) + " (" + (res.status || "queued") + "). Live at " + res.url + " — stays up even when this node is off.");
  logSystem("studio · pinned “" + name + "” to " + service + " · " + res.url, "ok");
  cvUpdateDomain();
  await cvLoadSites();
}
byId("cv-pin").addEventListener("click", () => safely(() => cvPin("filebase")));
byId("cv-pin-filebase").addEventListener("click", () => safely(() => cvPin("filebase")));
byId("cv-pin-pinata").addEventListener("click", () => safely(() => cvPin("pinata")));
byId("cv-pin-foureverland").addEventListener("click", () => safely(() => cvPin("foureverland")));
byId("cv-pin-ipfs").addEventListener("click", () => safely(() => cvPin("ipfs")));
byId("cv-pin-settings").addEventListener("click", () => safely(() => pinOpen()));

// ── New project: pick Website or Mobile App/Game; the right starter files are staged ──
function newClose() { byId("new-modal").style.display = "none"; }
byId("cv-new").addEventListener("click", () => {
  byId("new-status").textContent = "";
  byId("new-modal").style.display = "flex";
  byId("new-name").focus();
});
byId("new-close").addEventListener("click", newClose);
byId("new-modal").addEventListener("click", e => { if (e.target === byId("new-modal")) newClose(); });
document.addEventListener("keydown", e => { if (e.key === "Escape" && byId("new-modal").style.display === "flex") newClose(); });
async function newProject(kind) {
  const name = byId("new-name").value.trim();
  byId("new-status").textContent = "Creating…";
  const res = await postJson("/api/canvas/new", { name, kind });
  newClose();
  byId("new-name").value = "";
  await cvOpenFolder(res.path);          // open the fresh project in the canvas
  if (kind === "movie") {
    // Self-contained animation skill: GSAP + Lottie bundled into the project — no installs.
    notice("🎬 New movie/animation “" + res.name + "” — built in the browser with GSAP + Lottie (bundled, no installs). Edit animation.js, watch it play, then ⏺ Record to save a video. (Advanced 3D via Blender is optional — see README.)");
    logSystem("studio · new movie/animation · " + res.name + " · GSAP + Lottie skill", "ok");
    return;
  }
  notice(kind === "app"
    ? "📱 New app/game “" + res.name + "” — staged as an installable PWA (manifest, service worker + icons). Build it, then Publish to install on a phone."
    : "🌐 New website “" + res.name + "” created. Tell the Concierge what to build — it writes the files right here.");
  logSystem("studio · new " + (kind === "app" ? "app/game" : "website") + " · " + res.name, "ok");
}
byId("new-website").addEventListener("click", () => safely(() => newProject("website")));
byId("new-app").addEventListener("click", () => safely(() => newProject("app")));
byId("new-movie").addEventListener("click", () => safely(() => newProject("movie")));

// ── Commit the whole project to GitHub (git add -A → commit → push) ───────────
function gitIsPrivate() {
  const active = byId("git-vis").querySelector(".pin-mode-btn.active");
  return !active || active.dataset.vis !== "public";
}
byId("git-vis").querySelectorAll(".pin-mode-btn").forEach(btn => btn.addEventListener("click", () => {
  byId("git-vis").querySelectorAll(".pin-mode-btn").forEach(b => b.classList.remove("active"));
  btn.classList.add("active");
}));
function gitFolder() { return cv.folder || byId("cv-folder").value.trim(); }
function gitClose() { byId("git-modal").style.display = "none"; byId("git-password").value = ""; }
byId("cv-commit").addEventListener("click", () => safely(async () => {
  const folder = gitFolder();
  if (!folder) { notice("Open a project folder first — Commit pushes the whole open project."); return; }
  // Connect gate: GitHub must be connected (token + owner) before we can commit. If it
  // isn't, open the same Connect-accounts walk-through instead of failing the push.
  let status = {}; try { status = await getJson("/api/deploy/credentials"); } catch (e) {}
  if (!status.github) {
    notice("Connect your GitHub account (with the 'repo' scope) to commit a project there.");
    depOpen("github");
    return;
  }
  byId("git-folder").textContent = "Project: " + folder;
  const base = folder.replace(/\/+$/, "").split("/").pop() || "project";
  if (!byId("git-repo").value.trim()) byId("git-repo").value = base;
  if (!byId("git-message").value.trim()) byId("git-message").value = "Update via Concierge";
  byId("git-status").textContent = "";
  byId("git-modal").style.display = "flex";
  byId("git-password").focus();
}));
byId("git-close").addEventListener("click", gitClose);
byId("git-modal").addEventListener("click", e => { if (e.target === byId("git-modal")) gitClose(); });
document.addEventListener("keydown", e => { if (e.key === "Escape" && byId("git-modal").style.display === "flex") gitClose(); });
byId("git-go").addEventListener("click", () => safely(async () => {
  const folder = gitFolder();
  if (!folder) { notice("Open a project folder first."); return; }
  const password = byId("git-password").value;
  if (!password) { byId("git-password").focus(); notice("Your store password is required — pushing to GitHub is egress."); return; }
  const priv = gitIsPrivate();
  byId("git-status").textContent = "Staging + committing + pushing…";
  cvStatus("committing to GitHub…");
  const res = await postJson("/api/git/commit", {
    folder,
    repo: byId("git-repo").value.trim(),
    message: byId("git-message").value.trim(),
    private: priv,
    password,
  });
  byId("git-password").value = "";
  const what = res.created_repo ? "created + pushed" : (res.committed ? "committed + pushed" : "pushed — nothing new to commit");
  byId("git-status").textContent = "✓ " + what + " · " + res.repo_url;
  notice("GitHub (" + (res.private ? "private" : "public") + "): " + what + " · " + res.repo_url);
  logSystem("studio · git " + what + " → " + res.repo_url, "ok");
}));

// ── Deploy settings: per-host credentials, stored on-device (0600), never echoed back ──
const DEPLOY_FIELDS = {
  github: [
    { k: "token", label: "Personal access token (repo scope)", type: "password" },
    { k: "owner", label: "GitHub username or org", type: "text" },
    { k: "repo", label: "Repository name", type: "text" },
    { k: "branch", label: "Branch", type: "text", placeholder: "gh-pages" },
  ],
  netlify: [
    { k: "token", label: "Personal access token", type: "password" },
    { k: "site_id", label: "Site ID (optional — created if blank)", type: "text" },
  ],
  vercel: [
    { k: "token", label: "Access token", type: "password" },
    { k: "project", label: "Project name (optional)", type: "text" },
    { k: "team_id", label: "Team ID (optional)", type: "text" },
  ],
  cloudflare: [
    { k: "token", label: "API token (Pages : Edit) — manual fallback", type: "password" },
    { k: "account_id", label: "Account ID (optional — auto-detected)", type: "text" },
    { k: "project", label: "Pages project name (optional — uses the site name)", type: "text" },
  ],
  firebase: [
    { k: "site_id", label: "Hosting site ID (optional — auto-detected with one-click)", type: "text" },
    { k: "service_account", label: "Service-account JSON key — manual fallback (paste the whole file)", type: "textarea", placeholder: "{ \"type\": \"service_account\", \"project_id\": \"…\", \"client_email\": \"…\", \"private_key\": \"-----BEGIN PRIVATE KEY-----…\" }" },
  ],
};
// Guided "connect" walk-through per platform: a deep link to the token page + steps.
const DEPLOY_META = {
  github: {
    name: "GitHub",
    url: "https://github.com/settings/tokens/new?scopes=repo&description=Universal%20Concierge%20Plugin",
    steps: [
      "Click “Open GitHub token page” — it pre-selects the repo scope.",
      "Generate the token and copy it.",
      "Paste it below, then enter the repository’s owner and name.",
      "Test connection, then Save.",
    ],
  },
  netlify: {
    name: "Netlify",
    url: "https://app.netlify.com/user/applications#personal-access-tokens",
    steps: [
      "Click “Open Netlify token page”.",
      "Create a new personal access token and copy it.",
      "Paste it below. Leave Site ID blank to auto-create a site.",
      "Test connection, then Save.",
    ],
  },
  vercel: {
    name: "Vercel",
    url: "https://vercel.com/account/tokens",
    steps: [
      "Click “Open Vercel token page”.",
      "Create a token (full account scope) and copy it.",
      "Paste it below. Team ID is only for team accounts.",
      "Test connection, then Save.",
    ],
  },
  cloudflare: {
    name: "Cloudflare",
    url: "https://dash.cloudflare.com/profile/api-tokens",
    steps: [
      "Easiest: click “⚡ Connect Cloudflare in one click” above — approve in your browser, done.",
      "Manual alternative: create an API Token (My Profile → API Tokens) with Account → Cloudflare Pages → Edit.",
      "Paste the token below — Account ID and project name are optional (they auto-fill).",
      "Test connection, then Save.",
    ],
  },
  firebase: {
    name: "Firebase",
    url: "https://console.firebase.google.com/project/_/settings/serviceaccounts/adminsdk",
    steps: [
      "Easiest: click “⚡ Connect Firebase in one click” above — sign in with Google, approve, done.",
      "Just have a Firebase project (console.firebase.google.com); the Hosting site auto-detects.",
      "Manual alternative: Service accounts → “Generate new private key”, paste the JSON below + your site ID.",
      "Test connection, then Save. Everything stays on this device.",
    ],
  },
};
let depStatus = {};
function depRender() {
  const platform = byId("dep-platform").value;
  const meta = DEPLOY_META[platform] || { name: platform, url: "", steps: [] };
  // Guide: numbered steps + a deep link to the platform's token page.
  const guide = byId("dep-guide"); clear(guide);
  // Cloudflare + Firebase: one-click OAuth — approve in the browser, nothing to paste.
  if (OAUTH_PROVIDERS[platform]) {
    const oauth = node("div", "dep-oauth");
    const btn = node("button", "tool-button cf-oauth-btn", "⚡ Connect " + OAUTH_PROVIDERS[platform] + " in one click");
    btn.type = "button";
    btn.addEventListener("click", () => safely(() => oauthConnect(platform)));
    oauth.append(btn);
    oauth.append(node("div", "eyebrow", "Opens " + OAUTH_PROVIDERS[platform] + " in your browser — approve once, nothing to copy. (Manual entry still works below.)"));
    guide.appendChild(oauth);
  }
  const ol = node("ol", "");
  meta.steps.forEach(s => ol.appendChild(node("li", "", s)));
  guide.appendChild(ol);
  if (meta.url) {
    const open = node("button", "tool-button dep-open", "Open " + meta.name + " token page ↗");
    open.addEventListener("click", () => window.open(meta.url, "_blank", "noopener"));
    guide.appendChild(open);
  }
  // Fields.
  const wrap = byId("dep-fields"); clear(wrap);
  const known = (depStatus && depStatus[platform]) || {};
  DEPLOY_FIELDS[platform].forEach(f => {
    const row = node("label", "profile-row");
    row.appendChild(node("span", "profile-label", f.label));
    let inp;
    if (f.type === "textarea") {
      inp = document.createElement("textarea");
      inp.className = "input"; inp.rows = 6; inp.spellcheck = false;
      inp.autocapitalize = "off"; inp.autocorrect = "off";
      // A pasted key is a secret — never prefill it from saved status.
    } else {
      inp = document.createElement("input");
      inp.type = f.type;
      inp.className = "input";
      inp.autocomplete = f.type === "password" ? "new-password" : "off";
      // Prefill only the public (non-secret) fields from saved status — never a secret.
      if (f.type !== "password" && known && known[f.k] != null) inp.value = known[f.k];
    }
    inp.id = "dep-" + f.k;
    if (f.placeholder) inp.placeholder = f.placeholder;
    row.appendChild(inp);
    wrap.appendChild(row);
  });
  const st = byId("dep-status"); clear(st);
  st.appendChild(document.createTextNode(depStatus[platform] ? "✓ connected on this device" : "not connected yet"));
}
async function depLoad() {
  try { depStatus = await getJson("/api/deploy/credentials"); } catch (e) { depStatus = {}; }
  depRender();
}
// One-click OAuth (Cloudflare / Firebase): open the authorize page, then poll until the
// loopback listener captures the token (or the user cancels / it times out).
const OAUTH_PROVIDERS = { cloudflare: "Cloudflare", firebase: "Firebase" };
let oauthPoll = null;
async function oauthConnect(provider) {
  if (oauthPoll) { clearInterval(oauthPoll); oauthPoll = null; }
  const label = OAUTH_PROVIDERS[provider] || provider;
  const st = byId("dep-status"); clear(st);
  let res;
  try { res = await postJson("/api/deploy/" + provider + "/oauth-start", {}); }
  catch (e) { st.appendChild(node("span", "dep-test-bad", "✗ " + (e.message || "couldn't start login"))); return; }
  if (!res.authorize_url) { st.appendChild(node("span", "dep-test-bad", "✗ couldn't start login")); return; }
  window.open(res.authorize_url, "_blank", "noopener");
  st.appendChild(node("span", "", "Approve the " + label + " login in your browser…"));
  let ticks = 0;
  oauthPoll = setInterval(() => quietly(async () => {
    ticks++;
    let s; try { s = await getJson("/api/deploy/" + provider + "/oauth-status"); } catch (e) { return; }
    if (s.status === "connected") {
      clearInterval(oauthPoll); oauthPoll = null;
      clear(st); st.appendChild(node("span", "dep-test-ok", "✓ Connected to " + label + (s.account ? " — " + String(s.account).slice(0, 22) : "")));
      notice(label + " connected — you can Publish to it now.");
      await depLoad();
    } else if (s.status === "error") {
      clearInterval(oauthPoll); oauthPoll = null;
      clear(st); st.appendChild(node("span", "dep-test-bad", "✗ " + (s.message || "login failed")));
    } else if (ticks > 200) { clearInterval(oauthPoll); oauthPoll = null; }
  }), 1500);
}
function depOpen(platform) {
  byId("deploy-modal").style.display = "flex";
  if (platform && DEPLOY_FIELDS[platform]) byId("dep-platform").value = platform;
  safely(depLoad);
}
function depClose() { byId("deploy-modal").style.display = "none"; }
// Collect the currently-entered fields for the selected platform.
function depFields() {
  const platform = byId("dep-platform").value;
  const fields = {};
  DEPLOY_FIELDS[platform].forEach(f => {
    const v = byId("dep-" + f.k).value.trim();
    if (v) fields[f.k] = v;
  });
  return { platform, fields };
}
// "Test connection": verify the entered token live against the platform's API.
async function depTest() {
  const { platform, fields } = depFields();
  const secret = fields.token || fields.service_account;
  if (!secret) { notice(platform === "firebase" ? "Paste your service-account JSON first." : "Paste your token first."); return; }
  const st = byId("dep-status"); clear(st);
  st.appendChild(document.createTextNode("Testing…"));
  const res = await postJson("/api/deploy/test", { platform, fields });
  clear(st);
  if (res.ok) {
    const ok = node("span", "dep-test-ok", "✓ Connected — " + res.account);
    st.appendChild(ok);
  } else {
    st.appendChild(node("span", "dep-test-bad", "✗ " + (res.error || "could not connect")));
  }
}
async function depSave(wipe) {
  const platform = byId("dep-platform").value;
  let fields = null;
  if (!wipe) {
    fields = {};
    DEPLOY_FIELDS[platform].forEach(f => {
      const v = byId("dep-" + f.k).value.trim();
      if (v) fields[f.k] = v;
    });
    if (!Object.keys(fields).length) { notice("Fill in the credentials first."); return; }
  }
  await postJson("/api/deploy/credentials", { platform, fields });
  notice(wipe ? "Cleared " + platform + " credentials." : "Saved " + platform + " credentials on this device.");
  await depLoad();
}
byId("cv-deploy-settings").addEventListener("click", () => safely(() => depOpen()));
byId("dep-close").addEventListener("click", depClose);
byId("dep-platform").addEventListener("change", depRender);
byId("dep-test").addEventListener("click", () => safely(depTest));
byId("dep-save").addEventListener("click", () => safely(() => depSave(false)));
byId("dep-clear").addEventListener("click", () => safely(() => depSave(true)));
byId("deploy-modal").addEventListener("click", e => { if (e.target === byId("deploy-modal")) depClose(); });

// ── Pin settings: per-service {endpoint, token}, stored on-device (0600). All four
// speak the standard IPFS Pinning Services API, so one form covers them; the endpoint
// is prefilled per service. ──
const PIN_FIELDS = {
  filebase: [
    { k: "key", label: "Access Key (dashboard → Access Keys)", type: "text" },
    { k: "secret", label: "Secret Key", type: "password" },
    { k: "bucket", label: "Bucket name (any name — created for you)", type: "text" },
  ],
  pinata: [
    { k: "endpoint", label: "Pinning API endpoint", type: "text", default: "https://api.pinata.cloud/psa" },
    { k: "token", label: "JWT (API Keys → New Key)", type: "password" },
  ],
  foureverland: [
    { k: "endpoint", label: "Pinning API endpoint", type: "text", default: "https://api.4everland.dev" },
    { k: "token", label: "Pinning-service token", type: "password" },
  ],
  ipfs: [
    { k: "endpoint", label: "PSA base URL (the part before /pins)", type: "text", placeholder: "https://your-service.example/api/v1" },
    { k: "token", label: "Bearer token", type: "password" },
  ],
};
const PIN_META = {
  filebase: { name: "Filebase", url: "https://console.filebase.com/", steps: [
    "Create a free Filebase account (no bucket needed — we make one for you).",
    "Access Keys → copy your Access Key and Secret Key.",
    "Enter the key, secret, and any bucket name (e.g. concierge-sites).",
    "Test connection, then Save. They stay on this device.",
  ]},
  pinata: { name: "Pinata", url: "https://app.pinata.cloud/developers/api-keys", steps: [
    "Create a free Pinata account.",
    "API Keys → New Key (admin or pinning scope) → copy the JWT.",
    "Paste the JWT below — the endpoint stays as-is.",
    "Free plan works: your site is uploaded directly (Pinata's pin-by-CID is paid-only).",
    "Test connection, then Save.",
  ]},
  foureverland: { name: "4EVERLAND", url: "https://dashboard.4everland.org/", steps: [
    "Create a free 4EVERLAND account and open the Bucket / 4EVER Pin dashboard.",
    "Generate a Pinning-service token and copy it.",
    "Paste it below — the endpoint is already prefilled.",
    "Test connection, then Save.",
  ]},
  ipfs: { name: "an IPFS pinning service", url: "https://ipfs.github.io/pinning-services-api-spec/", steps: [
    "Any service that implements the IPFS Pinning Services API works.",
    "Enter its PSA base URL (everything before /pins) and a bearer token.",
    "Test connection, then Save.",
  ]},
};
let pinStatusData = {};
function pinRender() {
  const service = byId("pin-service").value;
  const meta = PIN_META[service] || { name: service, url: "", steps: [] };
  const guide = byId("pin-guide"); clear(guide);
  const ol = node("ol", ""); meta.steps.forEach(s => ol.appendChild(node("li", "", s))); guide.appendChild(ol);
  if (meta.url) {
    const open = node("button", "tool-button dep-open", "Open " + meta.name + " ↗");
    open.addEventListener("click", () => window.open(meta.url, "_blank", "noopener"));
    guide.appendChild(open);
  }
  const wrap = byId("pin-fields"); clear(wrap);
  const known = (pinStatusData && pinStatusData[service]) || {};
  PIN_FIELDS[service].forEach(f => {
    const row = node("label", "profile-row");
    row.appendChild(node("span", "profile-label", f.label));
    const inp = document.createElement("input");
    inp.type = f.type; inp.className = "input";
    inp.autocomplete = f.type === "password" ? "new-password" : "off";
    inp.autocapitalize = "off"; inp.autocorrect = "off"; inp.spellcheck = false;
    // Prefill only non-secret fields — never a token. Endpoint: saved value, else default.
    if (f.type !== "password") inp.value = (known && known[f.k]) || f.default || "";
    inp.id = "pin-" + f.k;
    if (f.placeholder) inp.placeholder = f.placeholder;
    row.appendChild(inp); wrap.appendChild(row);
  });
  const st = byId("pin-status"); clear(st);
  st.appendChild(document.createTextNode(pinStatusData[service] ? "✓ connected on this device" : "not connected yet"));
}
async function pinLoad() { try { pinStatusData = await getJson("/api/pin/credentials"); } catch (e) { pinStatusData = {}; } pinRender(); }
function pinOpen(service) { byId("pin-modal").style.display = "flex"; if (service && PIN_FIELDS[service]) byId("pin-service").value = service; safely(pinLoad); }
function pinClose() { byId("pin-modal").style.display = "none"; }
function pinNeedMsg(service) {
  return service === "filebase" ? "Enter your Filebase access key, secret, and IPFS bucket name." : "Paste your token first.";
}
function pinFields() {
  const service = byId("pin-service").value;
  const raw = {};
  PIN_FIELDS[service].forEach(f => { const v = byId("pin-" + f.k).value.trim(); if (v) raw[f.k] = v; });
  if (service === "filebase") {
    // Filebase PSA auth is base64(accessKey:secretKey:bucket) — exactly what the
    // official @filebase/sdk PinManager builds. Construct it client-side.
    const fields = {};
    if (raw.key && raw.secret && raw.bucket) {
      fields.token = btoa(raw.key + ":" + raw.secret + ":" + raw.bucket);
      fields.endpoint = "https://api.filebase.io/v1/ipfs";
    }
    return { service, fields };
  }
  const fields = Object.assign({}, raw);
  if (!fields.endpoint) { const ep = PIN_FIELDS[service].find(f => f.k === "endpoint"); if (ep && ep.default) fields.endpoint = ep.default; }
  return { service, fields };
}
async function pinTest() {
  const { service, fields } = pinFields();
  if (!fields.token || !fields.endpoint) { notice(pinNeedMsg(service)); return; }
  const st = byId("pin-status"); clear(st); st.appendChild(document.createTextNode("Testing…"));
  const res = await postJson("/api/pin/test", { service, fields });
  clear(st);
  if (res.ok) st.appendChild(node("span", "dep-test-ok", "✓ Connected — " + res.account));
  else st.appendChild(node("span", "dep-test-bad", "✗ " + (res.error || "could not connect")));
}
async function pinSave(wipe) {
  const { service, fields } = pinFields();
  if (!wipe && (!fields.token || !fields.endpoint)) { notice(pinNeedMsg(service)); return; }
  await postJson("/api/pin/credentials", { service, fields: wipe ? null : fields });
  notice(wipe ? "Cleared " + service + " pinning credentials." : "Saved " + service + " pinning credentials on this device.");
  await pinLoad();
}
byId("pin-close").addEventListener("click", pinClose);
byId("pin-service").addEventListener("change", pinRender);
byId("pin-test").addEventListener("click", () => safely(pinTest));
byId("pin-save").addEventListener("click", () => safely(() => pinSave(false)));
byId("pin-clear").addEventListener("click", () => safely(() => pinSave(true)));
byId("pin-modal").addEventListener("click", e => { if (e.target === byId("pin-modal")) pinClose(); });
document.addEventListener("keydown", e => { if (e.key === "Escape" && byId("pin-modal").style.display === "flex") pinClose(); });

// Expand the preview to a near-fullscreen overlay for testing (e.g. play the game
// big). Esc or a backdrop click collapses it; the iframe content keeps running.
let cvBig = false;
function cvSetBig(on) {
  cvBig = on;
  byId("cv-preview").classList.toggle("big", on);
  byId("cv-backdrop").classList.toggle("on", on);
  byId("cv-expand").classList.toggle("on", on);
  byId("cv-expand").textContent = on ? "⤡ Collapse" : "⛶ Expand";
}
byId("cv-expand").addEventListener("click", () => cvSetBig(!cvBig));
byId("cv-backdrop").addEventListener("click", () => cvSetBig(false));
document.addEventListener("keydown", e => {
  if (e.key === "Escape" && cvBig) { cvSetBig(false); return; }
  // F toggles expand — but only in the Studio view and not while typing in a field
  // (so typing "f" in the HTML window, or Cmd/Ctrl-F find, still works normally).
  if (e.key === "f" || e.key === "F") {
    const t = e.target || {}, tag = t.tagName || "";
    const typing = tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t.isContentEditable;
    if (typing || e.metaKey || e.ctrlKey || e.altKey) return;
    if (!byId("canvas-view").classList.contains("active")) return;
    e.preventDefault(); cvSetBig(!cvBig);
  }
});

// Educational live-share: broadcast the current page to remote viewers over WebRTC
// (signaling via the Rust relay / DM channel). Single-page; the local builder handles
// full multi-file sites. Host = "Share live"; viewers type the session name + Enter.
const cvRtc = { session: null, role: null, me: null, peers: new Map(), poll: null };
const CV_ICE = { iceServers: [{ urls: "stun:stun.l.google.com:19302" }] };
function cvViewerRender(html) { byId("cv-preview").removeAttribute("src"); byId("cv-preview").srcdoc = html || ""; }
async function cvSignal(to, kind, data) { try { await postJson("/api/canvas/signal", { session: cvRtc.session, from: cvRtc.me, to, kind, data }); } catch (e) {} }
function cvPeer(id) {
  const pc = new RTCPeerConnection(CV_ICE);
  pc.onicecandidate = e => { if (e.candidate) cvSignal(id, "ice", e.candidate); };
  const entry = { pc, dc: null }; cvRtc.peers.set(id, entry); return entry;
}
async function cvCurrentHtml() {
  if (cv.token) { try { return await (await fetch("/canvas-preview/" + cv.token + "/index.html")).text(); } catch (e) {} }
  return byId("cv-src").value;
}
async function cvBroadcast() {
  const html = await cvCurrentHtml();
  for (const { dc } of cvRtc.peers.values()) if (dc && dc.readyState === "open") dc.send(html);
}
async function cvHandle(m) {
  const { from, kind, data } = m; if (!from || from === cvRtc.me) return;
  if (cvRtc.role === "host") {
    if (kind === "join") {
      const e = cvPeer(from); const dc = e.pc.createDataChannel("page"); e.dc = dc;
      dc.onopen = async () => dc.send(await cvCurrentHtml());
      const offer = await e.pc.createOffer(); await e.pc.setLocalDescription(offer); await cvSignal(from, "offer", offer);
    } else if (kind === "answer") { const e = cvRtc.peers.get(from); if (e) await e.pc.setRemoteDescription(data); }
    else if (kind === "ice") { const e = cvRtc.peers.get(from); if (e && data) try { await e.pc.addIceCandidate(data); } catch (x) {} }
  } else if (cvRtc.role === "viewer") {
    if (kind === "offer") {
      const e = cvPeer(from); e.pc.ondatachannel = ev => { ev.channel.onmessage = msg => cvViewerRender(msg.data); };
      await e.pc.setRemoteDescription(data); const ans = await e.pc.createAnswer(); await e.pc.setLocalDescription(ans); await cvSignal(from, "answer", ans);
    } else if (kind === "ice") { const e = cvRtc.peers.get(from); if (e && data) try { await e.pc.addIceCandidate(data); } catch (x) {} }
  }
}
function startCvSignalPoll() {
  if (cvRtc.poll) clearInterval(cvRtc.poll);
  cvRtc.poll = setInterval(() => quietly(async () => {
    if (!cvRtc.session) return;
    let res; try { res = await getJson("/api/canvas/signal?session=" + encodeURIComponent(cvRtc.session) + "&me=" + encodeURIComponent(cvRtc.me)); } catch (e) { return; }
    for (const m of (res.messages || [])) await cvHandle(m);
  }), 1200);
}
byId("cv-share").addEventListener("click", () => {
  if (cvRtc.role === "host") { notice("Already sharing live · session “" + cvRtc.session + "”."); return; }
  cvRtc.session = byId("cv-session").value.trim() || ("studio-" + randId());
  byId("cv-session").value = cvRtc.session;
  cvRtc.role = "host"; cvRtc.me = "host"; cvRtc.peers.clear();
  startCvSignalPoll();
  cvStatus("sharing live · session “" + cvRtc.session + "”");
  notice("Live share started — others open Studio, type “" + cvRtc.session + "”, and press Enter to watch.");
});
byId("cv-session").addEventListener("keydown", e => {
  if (e.key !== "Enter" || cvRtc.role === "host") return;
  cvRtc.session = byId("cv-session").value.trim(); if (!cvRtc.session) return;
  cvRtc.role = "viewer"; cvRtc.me = randId(); cvRtc.peers.clear();
  startCvSignalPoll(); cvSignal("host", "join", {});
  cvStatus("watching “" + cvRtc.session + "”…");
});
// Bridge: when the AI stages a site via the MCP write tools (concierge.write_site /
// concierge.write_asset, which default to <store>/canvas/draft/), auto-prefill the
// site-folder field with that path and open it as the live canvas — so "tell the AI
// to build the page" lands right here, ready to preview + publish.
let cvDraftMtime = 0;
async function cvDraftPoll() {
  let d; try { d = await getJson("/api/canvas/draft"); } catch (e) { return; }
  if (!d.folder || !d.mtime || d.mtime <= cvDraftMtime) return;
  cvDraftMtime = d.mtime;
  byId("cv-folder").value = d.folder;                 // prefill the site-folder field
  if (cv.folder === d.folder) return;                 // already the open canvas — cvPoll hot-reloads it
  if (!cv.token) {                                    // nothing open yet: surface the AI's site live
    await cvOpen();
    cvStatus("the AI built this site · review + Publish");
    logSystem("studio · opened the AI-staged site", "ok");
  } else {                                            // a different folder is open: prefill only, don't hijack
    cvStatus("the AI staged a site — click Open to load it");
  }
}
setInterval(() => quietly(cvDraftPoll), 2000);

// Your username chip: shows online state; click copies the full username to share.
async function loadMe() {
  let me; try { me = await getJson("/api/me"); } catch (e) { return; }
  state.myUsername = me.username || "";
  const chip = byId("chat-id"), label = byId("chat-id-label");
  if (!chip) return;
  chip.classList.toggle("online", !!me.online);
  chip.dataset.full = me.username || "";
  label.textContent = me.online ? ("you · " + (me.username || "").slice(0, 8)) : "offline";
}
byId("chat-id").addEventListener("click", () => {
  const full = byId("chat-id").dataset.full || "";
  if (!full) { notice("Your network node starts the moment you send your first message."); return; }
  if (navigator.clipboard) navigator.clipboard.writeText(full);
  notice("Username copied — share it so other Concierge users can message you.");
});

// Pending message requests (the consent gate's holding area).
async function loadRequests() {
  let data; try { data = await getJson("/api/requests"); } catch (e) { return; }
  const reqs = data.requests || [];
  const badge = byId("msg-badge");
  if (badge) { badge.style.display = reqs.length ? "inline-block" : "none"; badge.textContent = reqs.length; }
  const box = byId("requests"); if (!box) return; clear(box);
  reqs.forEach(r => {
    const card = node("div", "request-card");
    const main = node("div", "req-main");
    main.append(
      node("div", "req-user", "Request from " + (r.username || "").slice(0, 18) + "…"),
      node("div", "req-preview", r.preview || (r.count + " message(s)")));
    const accept = node("button", "req-accept", "Accept");
    const decline = node("button", "req-decline", "Decline");
    accept.addEventListener("click", () => safely(async () => {
      await postJson("/api/requests/accept", { username: r.username });
      notice("Accepted — their messages were delivered to the thread.");
      await loadRequests(); await loadContacts(); await loadRooms(); if (state.room) await loadThread();
    }));
    decline.addEventListener("click", () => safely(async () => {
      await postJson("/api/requests/decline", { username: r.username });
      await loadRequests();
    }));
    card.append(main, accept, decline);
    box.append(card);
  });
}

// Deterministic 1:1 thread id for two usernames (matches the backend dm_room_id).
function dmRoom(a, b) { return "dm:" + [a, b].sort().join("-"); }

// Approved peers — the list of who can message you. Click a peer to open the
// thread; Remove blocks them (they need a fresh request to reach you again).
async function loadContacts() {
  let data; try { data = await getJson("/api/contacts"); } catch (e) { return; }
  const list = data.contacts || [];
  const box = byId("contacts"); if (!box) return; clear(box);
  if (!list.length) {
    box.append(node("div", "contact-empty", "No approved peers yet — accept a request, or message a username to add them."));
    return;
  }
  list.forEach(c => {
    const card = node("div", "contact-card");
    const main = node("div", "c-main");
    const nameRow = node("div", "c-name");
    nameRow.append(node("span", "", c.name || ((c.username || "").slice(0, 16) + "…")));
    if (c.verified) nameRow.append(node("span", "c-badge ok", "✓ nickname"));
    else if (c.name_source === "introduced") nameRow.append(node("span", "c-badge hint", "introduced"));
    else if (c.name_source === "card") nameRow.append(node("span", "c-badge hint", "unverified"));
    main.append(nameRow);
    main.append(node("div", "c-sub", (c.username || "").slice(0, 18) + "…" + (c.site_ipns ? "  ·  /ipns/" + String(c.site_ipns).slice(0, 12) + "…" : "")));
    main.title = "Open thread · " + c.username;
    main.addEventListener("click", () => {
      byId("chat-to").value = c.username;
      const room = c.room || (state.myUsername ? dmRoom(state.myUsername, c.username) : "");
      if (room) { byId("room").value = room; state.room = room; safely(loadThread); }
      byId("chat-msg").focus();
    });
    const pet = node("button", "c-pet", "Nickname");
    pet.title = "Give this peer your own private name — it overrides any name they assert (anti-spoofing).";
    pet.addEventListener("click", () => safely(async () => {
      const name = window.prompt("Set a private nickname for this peer (blank to clear):", c.verified ? c.name : "");
      if (name === null) return;
      await postJson("/api/petname", { agent_id: c.username, name: name.trim() });
      await loadContacts();
      notice(name.trim() ? "Nickname set — it's yours, locally, and overrides any name they assert." : "Nickname cleared.");
    }));
    const pair = node("button", "c-pet", "Pair");
    pair.title = "Share this node with this peer (e.g. your other computer) — generate a pairing offer to send them.";
    pair.addEventListener("click", () => safely(pairShareWizard));
    const remove = node("button", "c-remove", "Remove");
    remove.addEventListener("click", () => safely(async () => {
      if (!window.confirm("Block " + (c.name || (c.username || "").slice(0, 16) + "…") + "? They'll need a new request to reach you.")) return;
      await postJson("/api/contacts/remove", { username: c.username });
      await loadContacts();
    }));
    card.append(node("div", "c-dot"), main, pet, pair, remove);
    box.append(card);
  });
}
// Your own contact card editor (Layer 2 self-asserted profile). Peers you connect
// to see this name; petnames they set locally always override it on their side.
async function loadProfile() {
  const box = byId("profile-editor"); if (!box) return;
  let p; try { p = await getJson("/api/profile"); } catch (e) { return; }
  clear(box);
  const nameI = node("input", "input"); nameI.type = "text"; nameI.placeholder = "display name"; nameI.value = p.display_name || ""; nameI.maxLength = 48;
  const siteI = node("input", "input"); siteI.type = "text"; siteI.placeholder = "your site IPNS (k51…) — optional"; siteI.value = p.site_ipns || "";
  const bioI = node("input", "input"); bioI.type = "text"; bioI.placeholder = "short bio — optional"; bioI.value = p.bio || "";
  const save = node("button", "tool-button", "Save card");
  save.addEventListener("click", () => safely(async () => {
    await postJson("/api/profile", { display_name: nameI.value.trim(), bio: bioI.value.trim(), site_ipns: siteI.value.trim() });
    notice("Your contact card updated — peers you connect to will see this name.");
    await loadProfile();
  }));
  box.append(nameI, siteI, bioI, save, node("div", "profile-did", "did:key  " + (p.did || "").slice(0, 40) + "…"));
}

function applyZoom() {
  const svg = byId("graph");
  if (!svg) return;
  const content = svg.querySelector("g.graph-content");
  if (content) {
    content.setAttribute("transform", `translate(${state.pan.x}, ${state.pan.y}) scale(${state.zoom})`);
  }
}

byId("graph-view").addEventListener("wheel", event => {
  event.preventDefault();
  const zoomSpeed = 0.001;
  state.zoom -= event.deltaY * zoomSpeed;
  state.zoom = Math.max(0.1, Math.min(5, state.zoom));
  applyZoom();
}, { passive: false });

let isDragging = false;
let lastMouse = { x: 0, y: 0 };

byId("graph-view").addEventListener("mousedown", event => {
  if (event.target.closest(".graph-node")) return;
  isDragging = true;
  lastMouse = { x: event.clientX, y: event.clientY };
});

window.addEventListener("mousemove", event => {
  if (!isDragging) return;
  const dx = event.clientX - lastMouse.x;
  const dy = event.clientY - lastMouse.y;
  state.pan.x += dx;
  state.pan.y += dy;
  lastMouse = { x: event.clientX, y: event.clientY };
  applyZoom();
});

window.addEventListener("mouseup", () => {
  isDragging = false;
});

// Phase 8: the Sidekick (on-node embedding model) + its private Kubo node, which
// enable together. Coupled — the Sidekick needs the node; the node only runs as
// part of the Sidekick.
async function loadSidekickStatus() {
  const el = byId("sidekick-ctl");
  let s;
  try { s = await getJson("/api/sidekick/status"); }
  catch (_) { if (el) clear(el); document.body.classList.remove("node-live"); return; }
  // Heartbeat: pulse the central brain whenever the private node is actually up.
  document.body.classList.toggle("node-live", !!s.node_running);
  if (state.lastNodeRunning !== s.node_running) {
    if (s.node_running) logSystem("kubo private node ready · on-node embedding model online", "ok");
    else if (state.lastNodeRunning === true) logSystem("kubo node stopped", "wn");
    state.lastNodeRunning = s.node_running;
  }
  if (!el) return;
  clear(el);
  const btn = node("button", "node-toggle"); btn.append(node("span", "dot", ""));
  const label = txt => btn.append(node("span", "", txt));
  if (s.operational) {
    btn.classList.add("on"); label("Kubo node sidekick: ON");
    btn.title = "On-node embedding model + private Kubo node running. Click to disable.";
    btn.addEventListener("click", () => safely(async () => {
      await postJson("/api/sidekick/disable", {}); notice("Sidekick disabled."); await loadSidekickStatus();
    }));
  } else if (s.enabled && !s.node_running) {
    btn.classList.add("warn"); label("Kubo node sidekick: starting…");
    btn.title = "The private Kubo node is coming up. Click to re-launch.";
    btn.addEventListener("click", () => safely(async () => {
      await postJson("/api/sidekick/enable", {}); notice("Re-launching the private Kubo node…"); await loadSidekickStatus();
    }));
  } else if (!s.kubo_installed) {
    btn.classList.add("warn"); label("Kubo node sidekick: needs Kubo");
    btn.title = "The sidekick (on-node embedding model) runs as part of a private Kubo node. Install Kubo to enable it.";
    btn.addEventListener("click", () => window.open("https://docs.ipfs.tech/install/", "_blank", "noopener,noreferrer"));
  } else {
    label("Kubo node sidekick: OFF");
    btn.title = s.disclaimer || "Enable the on-node embedding model + private Kubo node.";
    btn.addEventListener("click", () => safely(async () => {
      await postJson("/api/sidekick/enable", {}); notice("Enabling Sidekick — launching your private Kubo node."); await loadSidekickStatus();
    }));
  }
  el.append(btn);
}

// Claude Code auto-capture control (opt-in). Renders into the header control bar:
// "Detach" when capturing, "Attach" when sessions are found, nothing otherwise.
async function loadClaudeCodeStatus() {
  const el = byId("cc-ctl");
  let status;
  try { status = await getJson("/api/claude-code/status"); }
  catch (_) { if (el) clear(el); return; }
  if (!el) return;
  clear(el);
  if (!status.available || (!status.attached && !status.session_count)) return;
  const n = status.session_count;
  if (status.attached) {
    el.append(node("span", "cc-live", ""));
    el.append(node("span", "cc-cap", "Capturing Claude Code · " + n + " session" + (n === 1 ? "" : "s")));
    const detach = node("button", "cc-detach", "Detach");
    detach.title = "Pause Claude Code auto-capture (watched sessions stay ingested).";
    detach.addEventListener("click", () => safely(async () => {
      await postJson("/api/claude-code/detach", {});
      notice("Detached — Claude Code capture paused.");
      await loadClaudeCodeStatus();
    }));
    el.append(detach);
  } else {
    el.append(node("span", "cc-cap", n + " Claude Code session" + (n === 1 ? "" : "s") + " found"));
    const attach = node("button", "cc-attach", "Attach");
    attach.title = "Backfill and watch Claude Code sessions — local-only, nothing leaves your device.";
    attach.addEventListener("click", () => safely(async () => {
      await postJson("/api/claude-code/attach", {});
      notice("Attached — backfilling and watching Claude Code sessions.");
      await loadClaudeCodeStatus();
      quietly(async () => { await Promise.all([loadNames(), refreshGraphData(), loadStats()]); });
    }));
    el.append(attach);
  }
}

// Phase 8 §1 — the Librarian's semantic-search bar. Ranks by meaning × graph
// gravity, packed to a token budget; clicking a hit opens that node's record.
async function runSearch() {
  const q = byId("search-q").value.trim();
  const depth = byId("search-depth").value;
  const hops = byId("search-related").checked ? 1 : 0;
  const out = byId("search-results");
  clear(out);
  if (!q) return;
  out.append(node("div", "search-summary", "Searching…"));
  let data;
  try {
    data = await getJson("/api/search?q=" + encodeURIComponent(q) + "&depth=" + encodeURIComponent(depth) + "&budget=4000&hops=" + hops);
  } catch (error) {
    clear(out); out.append(node("div", "pblockers", error.message)); return;
  }
  clear(out);
  flashSynapses(); // a search is memory recall → fire the current on the graph
  logSystem("recall “" + q + "” → " + data.items.length + " hit(s) over " + data.indexed + " nodes", "ev");
  out.append(node("div", "search-summary",
    data.items.length + " hit(s) over " + data.indexed + " indexed node(s) · " + data.used_tokens + "/" + data.budget_tokens + " tokens"));
  if (!data.items.length) {
    out.append(node("div", "empty", "No matches. Capture or ingest more, or broaden the query."));
    return;
  }
  data.items.forEach(hit => {
    const card = node("div", "search-hit" + (hit.hop ? " related" : ""));
    const head = node("div", "hit-head");
    head.append(
      node("span", "hit-score", hit.hop ? "related ↳" : "score " + hit.score.toFixed(3)),
      node("span", "hit-kind", hit.kind),
      node("span", "", "gravity " + hit.gravity.toFixed(2)),
      node("span", "", shortCid(hit.cid)),
    );
    card.append(head, node("div", "hit-preview", hit.preview));
    card.addEventListener("click", () => { loadRecord(hit.cid, true); });
    out.append(card);
  });
}

// The app opens on the Studio canvas (first tab). Activate it + load its sites up
// front, since the graph (loaded below) is no longer the landing view.
showView("canvas");
safely(cvLoadSites);
safely(async () => {
  await Promise.all([loadMeta(), loadNames(), loadRooms(), loadMe(), loadRequests(), loadContacts(), loadProfile()]);
  await loadGraph();
  await refreshPrivacy();
});
// Stats (the store/DAG rail) are informational and the slowest call, so they
// load in the background — the UI is interactive as soon as the graph is ready.
logSystem("data platter mounted — watching the concierge", "ok");
quietly(loadStats);
quietly(pollActivity);
quietly(loadSidekickStatus);
quietly(loadClaudeCodeStatus);
window.setInterval(() => quietly(async () => {
  await Promise.all([loadNames(), refreshGraphData(), loadStats(), pollActivity(), loadSidekickStatus(), loadMe(), loadRequests(), loadContacts()]);
  if (!byId("privacy").querySelector("form")) await refreshPrivacy();
  if (state.room) await loadThread();
}), 5000);
