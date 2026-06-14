// ── Studio: live website builder + publish + educational live-share ──────────
// Open a site folder; it renders live (multi-file) at /canvas-preview/<token>/ and
// hot-reloads as files change (you or your AI write them). Publish the folder to a
// stable IPNS site. "Share live" broadcasts the page to viewers for example displays.
const cv = { mode: "write", token: null, folder: null, mtime: 0, poll: null, viewing: false };
function cvStatus(t) { byId("cv-status").textContent = t; }
function randId() { return Math.random().toString(36).slice(2, 10); }
function cvBasename(p) { return (p || "").replace(/\/+$/, "").split("/").pop() || ""; }
function cvWritePreview() { byId("cv-preview").removeAttribute("src"); byId("cv-preview").srcdoc = byId("cv-src").value; }
let cvSrcDebounce;
byId("cv-src").addEventListener("input", () => {
  if (cv.mode !== "write") return;
  clearTimeout(cvSrcDebounce);
  cvSrcDebounce = setTimeout(() => { cvWritePreview(); if (cvRtc.role === "host") cvBroadcast(); }, 250);
});
function cvSetMode(m) {
  cv.mode = m; cv.viewing = false;
  const write = m === "write";
  byId("cv-mode-write").classList.toggle("on", write);
  byId("cv-mode-folder").classList.toggle("on", !write);
  byId("cv-src").style.display = write ? "" : "none";
  byId("cv-files").style.display = write ? "none" : "";
  document.querySelectorAll(".cv-folder-only").forEach(el => el.style.display = write ? "none" : "");
  if (write) { if (cv.poll) clearInterval(cv.poll); cvWritePreview(); cvStatus("writing"); }
  else { if (cv.token) cvShowLive(); cvStatus(cv.token ? "live" : "no folder open"); }
}
byId("cv-mode-write").addEventListener("click", () => cvSetMode("write"));
byId("cv-mode-folder").addEventListener("click", () => cvSetMode("folder"));

function cvShowLive() {
  cv.viewing = false;
  if (cv.token) byId("cv-preview").removeAttribute("srcdoc"), byId("cv-preview").src = "/canvas-preview/" + cv.token + "/?t=" + cv.mtime;
}
function cvRenderFiles(files, hasIndex) {
  const box = byId("cv-files"); clear(box);
  if (!files || !files.length) { box.append(node("div", "empty", "Folder is empty — add an index.html (you or your AI).")); return; }
  if (!hasIndex) box.append(node("div", "empty", "⚠ no index.html — the preview needs one."));
  files.forEach(f => box.append(node("div", "cv-file" + (f === "index.html" ? " index" : ""), f)));
}
async function cvOpen() {
  const folder = byId("cv-folder").value.trim();
  if (!folder) { notice("Enter the folder where your site's files are."); return; }
  const res = await postJson("/api/canvas/open", { folder });
  cv.token = res.token; cv.folder = folder; cv.mtime = res.mtime;
  if (!byId("cv-name").value.trim()) byId("cv-name").value = cvBasename(folder).toLowerCase().replace(/[^a-z0-9._-]+/g, "-");
  cvUpdateDomain();
  cvRenderFiles(res.files, res.has_index);
  cvShowLive();
  cvStatus(res.has_index ? (res.files.length + " file(s) · live") : "no index.html yet — add one to render");
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
    if (cvRtc.role === "host") cvBroadcast();        // push to educational viewers
  }
}
function startCvPoll() { if (cv.poll) clearInterval(cv.poll); cv.poll = setInterval(() => quietly(cvPoll), 1000); }
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
  let folder;
  if (cv.mode === "write") {
    const html = byId("cv-src").value;
    if (!html.trim()) { notice("Write some HTML first."); return; }
    const snap = await postJson("/api/canvas/snapshot", { session: name, html });   // stage HTML to a folder
    folder = snap.folder;
  } else {
    if (!cv.folder) { notice("Open a folder first."); return; }
    folder = cv.folder;
  }
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
    notice("Published. View it now with “Open published site” (served from your node).");
    // Tell the user honestly whether outsiders can load a shared link — and if not,
    // pop up the explicit instructions to fix it.
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
byId("reach-view").addEventListener("click", () => byId("cv-open-native").click());
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
  const sel = byId("cv-view"); const cur = sel.value;
  clear(sel); sel.append(node("option", "", "— view a published site —"));
  cv.sites.forEach(s => {
    const o = node("option", "", s.name + "  ·  " + (s.ipns || "").slice(0, 12) + "…"); o.value = s.ipns; sel.append(o);
  });
  sel.value = cur;
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
    card.append(node("div", "review", "Every 💾 Save checkpoint and every Publish is snapshotted (with a real CID in Records). Reopen any version to edit; re-publishing keeps the same /ipns/ address."));
    const box = node("div", "ckpt-list");
    list.forEach(c => {
      const row = node("div", "ckpt-row");
      const when = c.ts ? new Date(c.ts * 1000).toLocaleString() : "—";
      const meta = node("div", "ckpt-meta");
      meta.append(node("div", "ckpt-when", when));
      meta.append(node("div", "ckpt-sub", c.site + (c.ipns ? "  ·  /ipns/" + String(c.ipns).slice(0, 14) + "…" : "")));
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
  byId("cv-src").value = d.html;
  byId("cv-name").value = site;
  cvSetMode("write");
  cvUpdateDomain();
  if (overlay) overlay.remove();
  notice("Loaded the " + new Date(ts * 1000).toLocaleString() + " version of “" + site + "” — edit and Publish to update the same address.");
}
// Save a checkpoint of the current draft at any time (no publish). Content-addresses
// the HTML so the snapshot has a real CID in Records, and lists it under ⏱ Checkpoints
// to reopen later.
async function cvSaveCheckpoint() {
  const name = byId("cv-name").value.trim();
  if (!name) { byId("cv-name").focus(); notice("Give your site a name first."); return; }
  let req;
  if (cv.mode === "write") {
    const html = byId("cv-src").value;
    if (!html.trim()) { notice("Write some HTML first."); return; }
    req = { name, html };
  } else if (cv.folder) {
    req = { name, folder: cv.folder };
  } else {
    notice("Open a folder or switch to Write mode first."); return;
  }
  const res = await postJson("/api/site/checkpoint/save", req);
  notice("Checkpoint saved" + (res.cid ? " · " + String(res.cid).slice(0, 14) + "…" : "") + " — in ⏱ Checkpoints and Records.");
  logSystem("studio · saved checkpoint “" + name + "”", "ok");
  if (byId("cv-ckpt-overlay")) safely(cvOpenCheckpoints); // refresh the list if it's open
}
byId("cv-save-ckpt").addEventListener("click", () => safely(cvSaveCheckpoint));
byId("cv-ckpts-btn").addEventListener("click", () => safely(cvOpenCheckpoints));
byId("cv-view").addEventListener("change", () => {
  const ipns = byId("cv-view").value; if (!ipns) return;
  cv.viewing = true;
  byId("cv-preview").removeAttribute("srcdoc");
  byId("cv-preview").src = "http://127.0.0.1:8090/ipns/" + ipns + "/";    // your public node's gateway
  cvStatus("viewing published site (your node's gateway)");
});

// B-Portal: open a published site's ipns:// address NATIVELY in Brave (no gateway).
// Brave resolves ipns:// via a local node; we reveal the button only when in Brave.
let cvIsBrave = false;
(async () => {
  try { cvIsBrave = !!(navigator.brave && await navigator.brave.isBrave()); } catch (e) {}
})();
// Open the published site through YOUR OWN node's local gateway (it has the content).
// ipns:// routes Brave to a public gateway that can't find a local-only, NAT'd node.
byId("cv-open-native").addEventListener("click", () => {
  const ipns = byId("cv-view").value || cv.lastIpns || "";
  if (!ipns) { notice("Publish or select a site first."); return; }
  window.open("http://127.0.0.1:8090/ipns/" + ipns + "/", "_blank");
  logSystem("studio · opened " + String(ipns).slice(0, 12) + "… via your node's local gateway", "ok");
});
byId("cv-edit").addEventListener("click", () => {
  if (cv.mode === "write") { cvWritePreview(); cvStatus("writing"); }
  else if (cv.token) { cvShowLive(); cvStatus("live"); }
});
byId("cv-open").addEventListener("click", () => safely(cvOpen));
byId("cv-folder").addEventListener("keydown", e => { if (e.key === "Enter") safely(cvOpen); });

byId("cv-publish").addEventListener("click", () => safely(() => cvPublish("ipfs")));
byId("cv-pub-ipfs").addEventListener("click", () => safely(() => cvPublish("ipfs")));
byId("cv-pub-github").addEventListener("click", () => safely(() => cvPublish("github")));
byId("cv-pub-netlify").addEventListener("click", () => safely(() => cvPublish("netlify")));
byId("cv-pub-vercel").addEventListener("click", () => safely(() => cvPublish("vercel")));
byId("cv-pub-cloudflare").addEventListener("click", () => safely(() => cvPublish("cloudflare")));

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
    { k: "token", label: "API token (Pages : Edit)", type: "password" },
    { k: "account_id", label: "Account ID", type: "text" },
    { k: "project", label: "Pages project name", type: "text" },
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
      "Click “Open Cloudflare token page”.",
      "Create a token with the Cloudflare Pages: Edit permission; copy it.",
      "Paste it below, plus your Account ID and a Pages project name.",
      "Test connection, then Save.",
    ],
  },
};
let depStatus = {};
function depRender() {
  const platform = byId("dep-platform").value;
  const meta = DEPLOY_META[platform] || { name: platform, url: "", steps: [] };
  // Guide: numbered steps + a deep link to the platform's token page.
  const guide = byId("dep-guide"); clear(guide);
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
    const inp = document.createElement("input");
    inp.type = f.type; inp.className = "input"; inp.id = "dep-" + f.k;
    inp.autocomplete = f.type === "password" ? "new-password" : "off";
    if (f.placeholder) inp.placeholder = f.placeholder;
    // Prefill only the public (non-secret) fields from saved status — never a token.
    if (f.type !== "password" && known && known[f.k] != null) inp.value = known[f.k];
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
  if (!fields.token) { notice("Paste your token first."); return; }
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
  if (cv.mode === "write") return byId("cv-src").value;
  if (!cv.token) return "";
  try { return await (await fetch("/canvas-preview/" + cv.token + "/index.html")).text(); } catch (e) { return ""; }
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
// Bridge: pick up HTML the AI staged via the MCP `concierge.write_site` tool
// (<store>/canvas/draft/index.html) and surface it live in the Write tab — so
// "tell the AI to write the page" really lands in this window.
let cvDraftMtime = 0;
async function cvDraftPoll() {
  let d; try { d = await getJson("/api/canvas/draft"); } catch (e) { return; }
  if (!d.mtime || d.mtime <= cvDraftMtime) return;
  const baseline = cvDraftMtime === 0;
  cvDraftMtime = d.mtime;
  if (baseline && byId("cv-src").value.trim()) return;   // don't clobber existing typing on first sight
  if (cv.mode === "write" && d.html != null) {
    byId("cv-src").value = d.html;
    cvWritePreview();
    if (cvRtc.role === "host") cvBroadcast();
    cvStatus("the AI wrote the page · review + Publish");
    logSystem("studio · AI wrote into the Write tab", "ok");
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
    if (c.verified) nameRow.append(node("span", "c-badge ok", "✓ petname"));
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
    const pet = node("button", "c-pet", "Petname");
    pet.title = "Give this peer your own private name — it overrides any name they assert (anti-spoofing).";
    pet.addEventListener("click", () => safely(async () => {
      const name = window.prompt("Set a private petname for this peer (blank to clear):", c.verified ? c.name : "");
      if (name === null) return;
      await postJson("/api/petname", { agent_id: c.username, name: name.trim() });
      await loadContacts();
      notice(name.trim() ? "Petname set — it's yours, locally, and overrides any name they assert." : "Petname cleared.");
    }));
    const remove = node("button", "c-remove", "Remove");
    remove.addEventListener("click", () => safely(async () => {
      if (!window.confirm("Block " + (c.name || (c.username || "").slice(0, 16) + "…") + "? They'll need a new request to reach you.")) return;
      await postJson("/api/contacts/remove", { username: c.username });
      await loadContacts();
    }));
    card.append(node("div", "c-dot"), main, pet, remove);
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
