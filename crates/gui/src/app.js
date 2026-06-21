"use strict";
const state = { root: "", rootMode: "default", selected: "", humanOnly: false, muted: new Set(), roles: new Map(), room: "", fullGraph: null, visibleCids: new Set(), zoom: 1.0, pan: { x: 0, y: 0 }, viewInitialized: false, graphBounds: null, namesOpen: new Set(), namesOpenInit: false, publishingReady: false, lastStats: null, lastNodeRunning: null };
// Monotonic selection stamp. Bumped every time the selected node changes, so an
// in-flight privacy render started for an older selection can detect it was
// superseded and refuse to paint (otherwise its late `await` could overwrite the
// panel with a different node than the one now highlighted).
let privacySeq = 0;
function setSelected(cid) { state.selected = cid; privacySeq++; }
const byId = id => document.getElementById(id);
const svgNS = "http://www.w3.org/2000/svg";

function node(tag, className, text) {
  const item = document.createElement(tag);
  if (className) item.className = className;
  if (text !== undefined) item.textContent = String(text);
  return item;
}
function svgNode(tag, attrs, text) {
  const item = document.createElementNS(svgNS, tag);
  Object.entries(attrs || {}).forEach(([key, value]) => item.setAttribute(key, value));
  if (text !== undefined) item.textContent = String(text);
  return item;
}
function clear(item) { item.replaceChildren(); }
// A small padlock glyph (shackle arc + body + keyhole) centered at (cx, cy).
function lockGlyph(cx, cy, color) {
  const g = svgNode("g", {});
  g.append(svgNode("path", { d: "M " + (cx - 3) + " " + (cy - 1) + " v -2 a 3 3 0 0 1 6 0 v 2", fill: "none", stroke: color, "stroke-width": 1.3 }));
  g.append(svgNode("rect", { x: cx - 4.5, y: cy - 1, width: 9, height: 7, rx: 1.3, fill: color }));
  g.append(svgNode("circle", { cx: cx, cy: cy + 2.4, r: 1, fill: "var(--black)" }));
  return g;
}
function brainGlyph(cx, cy, size, color) {
  const g = svgNode("g", { class: "brain-icon" });
  const s = size;
  // A soft halo behind the brain — pulses (via CSS) whenever the private node is
  // live, so the running Sidekick is felt at the center of the graph.
  g.append(svgNode("circle", { cx, cy, r: s * 0.62, class: "brain-halo" }));
  const img = svgNode("image", {
    href: "/api/brain.png",
    x: cx - s / 2,
    y: cy - s / 2,
    width: s,
    height: s
  });
  g.append(img);
  return g;
}
function shortCid(cid) { return cid ? cid.slice(0, 12) + "..." + cid.slice(-5) : "none"; }
// Compact timestamp from unix seconds (graph nodes, checkpoint ticks). Empty if unknown.
function fmtWhen(ts) {
  const s = Number(ts || 0);
  if (!s) return "";
  return new Date(s * 1000).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}
function formatBytes(bytes) {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB"]; let value = bytes; let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return value.toFixed(unit ? 1 : 0) + " " + units[unit];
}
function notice(text) {
  const item = byId("notice"); item.textContent = text; item.classList.add("show");
  window.setTimeout(() => item.classList.remove("show"), 2800);
}
async function getJson(url) {
  const response = await fetch(url);
  const data = await response.json();
  if (!response.ok || data.error) throw new Error(data.error || response.statusText);
  return data;
}
let csrfToken = "";
async function postJson(url, payload) {
  const response = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-CSRF-Token": csrfToken },
    body: JSON.stringify(payload),
  });
  const data = await response.json().catch(() => ({}));
  if (!response.ok || data.error) throw new Error(data.error || response.statusText);
  return data;
}
// Window lifecycle is owned by the server now: it launches the app window in a browser it
// owns and watches that process, so closing the window shuts the server down. The page no
// longer needs to heartbeat — a backgrounded tab used to look "closed" and kill the server.
// User-initiated work runs through `safely`: while anything is in flight the
// blocking overlay is shown, so further clicks are swallowed instead of stacking
// up concurrent requests. A counter keeps the overlay up across nested/overlapping
// actions and only drops it once the last one settles.
let busyCount = 0;
let busyShowTimer = null;
function renderBusy() {
  const overlay = byId("busy-overlay");
  if (!overlay) return;
  const busy = busyCount > 0;
  overlay.classList.toggle("active", busy); // block clicks immediately
  if (busy) {
    if (!busyShowTimer && !overlay.classList.contains("show")) {
      busyShowTimer = window.setTimeout(() => {
        busyShowTimer = null;
        if (busyCount > 0) overlay.classList.add("show");
      }, 140);
    }
  } else {
    if (busyShowTimer) { window.clearTimeout(busyShowTimer); busyShowTimer = null; }
    overlay.classList.remove("show");
  }
}
async function safely(action) {
  busyCount++; renderBusy();
  try { await action(); }
  catch (error) { notice(error.message || String(error)); }
  finally { busyCount = Math.max(0, busyCount - 1); renderBusy(); }
}
// Background work (the periodic poll, grant-expiry sweep): same error handling
// but never shows the overlay or blocks the UI.
async function quietly(action) {
  try { await action(); } catch (error) { notice(error.message || String(error)); }
}

// ── UI event delegation ───────────────────────────────────────────────────────
// One set of document-level listeners drives every STATIC control, instead of a
// long run of top-level `byId("x").addEventListener(...)`. A missing element is
// now a harmless no-op, so a removed button can never throw during init and take
// the whole page down (which is exactly what used to happen). Static controls
// declare `data-action`; the handler is resolved from ACTIONS at event time, so
// load order never matters. Modals share `.modal-overlay`, so one backdrop-click
// and one Escape handler dismiss them all (a modal with cleanup adds `data-close`).
function showModal(id) { const m = byId(id); if (m) m.style.display = "flex"; }
function hideModal(id) { const m = byId(id); if (m) m.style.display = "none"; }
function closeOverlay(m) {
  if (!m) return;
  const custom = m.dataset && m.dataset.close && window[m.dataset.close];
  if (typeof custom === "function") custom(); else m.style.display = "none";
}
function topOverlay() {
  const open = [...document.querySelectorAll(".modal-overlay")].filter(m => m.style.display === "flex");
  return open[open.length - 1] || null;
}
// Handlers factored out of the old inline bindings so ACTIONS can reference them.
function openProfileBundle() { return Promise.all([loadProfile(), loadContacts(), loadRequests()]); }
function focusNetworkName() { const f = byId("network-name"); if (f) { f.value = ""; f.focus(); } }
async function bookmarksSync() {
  const st = byId("bookmarks-status"); if (st) { clear(st); st.appendChild(document.createTextNode("Syncing…")); }
  const res = await postJson("/api/bookmarks/sync", {});
  if (st) { clear(st); st.appendChild(document.createTextNode(res.added > 0
    ? "Added " + res.added + " bookmark" + (res.added === 1 ? "" : "s") + " to memory."
    : "Up to date — no new bookmarks.")); }
  logSystem("synced browser bookmarks · +" + (res.added || 0), "ok");
  // Append the new records into the tree — one leaf each, no full re-render. Only if
  // the tree is already on screen; otherwise it renders fresh on next open. Falls back
  // to a single reload if a target folder doesn't exist yet (first record of the day).
  const tree = byId("graph-tree");
  if (tree && tree.children.length && (res.records || []).length) {
    const now = Math.floor(Date.now() / 1000);
    const allInserted = res.records.every(r => insertTreeRecord({
      cid: r.cid, created_at: now, kind: r.kind, preview: r.preview, linked: r.linked, names: r.names,
    }));
    if (!allInserted) await loadGraphTree();
  }
  await loadStats();
}
async function networkCreate() {
  const f = byId("network-name"); const name = (f && f.value.trim()) || ""; if (!name) return;
  await postJson("/api/network/create", { name });
  if (f) f.value = "";
  hideModal("newnet-modal");
  await loadNetwork();
}
async function updateCheck() {
  const { release } = await postJson("/api/update/check", {});
  notice(release && release.version ? "Update available: " + release.version : "You're on the latest version.");
  await loadUpdateStatus();
}
async function brainModelApply() {
  const sel = byId("brain-model"); const model = sel ? sel.value : "";
  await postJson("/api/brain/model", { model });
  notice(model ? "Active model set to " + model + "." : "Model selection cleared.");
  await loadBrainMetrics();
}
const ACTIONS = {
  open:  el => { showModal(el.dataset.modal); const fn = el.dataset.onopen && window[el.dataset.onopen]; if (typeof fn === "function") safely(fn); },
  close: el => closeOverlay(el.closest(".modal-overlay")),
  hotManage: () => safely(loadHotManager),
  bookmarksSync: () => quietly(bookmarksSync),
  refreshTree: () => quietly(loadGraphTree),
  pairShare: () => safely(pairShareWizard),
  pairJoin: () => safely(pairJoinWizard),
  networkCreate: () => safely(networkCreate),
  updateCheck: () => safely(updateCheck),
  brainModelApply: () => safely(brainModelApply),
};
// Studio (studio.js) registers its own actions into this same map.
window.ACTIONS = ACTIONS;
document.addEventListener("click", e => {
  const actor = e.target.closest("[data-action]");
  if (actor) { const fn = ACTIONS[actor.dataset.action]; if (fn) fn(actor, e); return; }
  if (e.target.classList && e.target.classList.contains("modal-overlay")) closeOverlay(e.target); // backdrop
});
document.addEventListener("keydown", e => {
  if (e.key === "Escape") { const m = topOverlay(); if (m) closeOverlay(m); return; }
  if (e.key === "Enter" && e.target.dataset && e.target.dataset.enter) {
    const fn = ACTIONS[e.target.dataset.enter]; if (fn) { e.preventDefault(); fn(e.target, e); }
  }
});

function showView(name) {
  document.querySelectorAll(".view").forEach(item => item.classList.toggle("active", item.id === name + "-view"));
  document.querySelectorAll("[data-view]").forEach(item => item.classList.toggle("active", item.dataset.view === name));
  // Studio gets the full width: collapse the Store/DAG rail (console, backends,
  // pin status) so the canvas can maximize. CSS hides .panel under this class.
  document.body.classList.toggle("studio-max", name === "canvas");
}

async function loadMeta() {
  const meta = await getJson("/api/meta");
  csrfToken = meta.csrf_token || "";
  { const m = byId("model"); if (m) m.textContent = meta.mounted_model; } // moved into Brain tab
  const option = node("option", "", meta.store);
  byId("store").replaceChildren(option);
}
// Records is merged into the Graph file tree; loadNames now just refreshes it.
async function loadNames() { return loadGraphTree(); }
function appendChildren(parent, ...children) { children.forEach(child => parent.append(child)); return parent; }


// ── Graph tab: a file tree of your memory (Month ▸ Day ▸ Session ▸ Record), built
// from the SAME /api/names path the Records tab uses, on the canonical UTC axis. ──
function sessionOf(names) {
  for (const nm of (names || [])) {
    const after = (nm.split(":session:")[1] || "");
    const id = after.split(":")[0];
    if (id) return id;
  }
  return "loose";
}
function sessionLabel(s) { return s === "loose" ? "loose records" : "session " + s.slice(0, 8); }
async function loadGraphTree() {
  const names = await getJson("/api/names");
  renderGraphTree(names, byId("graph-tree"));
}
function ftFolder(open, icon, label, count, key, body, badge) {
  const head = node("button", "ft-row ft-folder-row" + (open ? " open" : ""));
  head.dataset.key = key; // so an incremental insert can find this folder
  head.append(node("span", "disclosure", open ? "▾" : "▸"), node("span", "ft-icon", icon),
    node("span", "ft-label", label));
  // A session whose records link out (Merkle edges) gets a link badge.
  if (badge) { const b = node("span", "ft-link-badge", badge); b.title = "Contains records that link out (Merkle edges)"; head.append(b); }
  head.append(node("span", "count", count + " rec"));
  head.addEventListener("click", () => {
    const isOpen = body.style.display !== "none";
    body.style.display = isOpen ? "none" : "block";
    head.classList.toggle("open", !isOpen);
    const caret = head.querySelector(".disclosure");
    if (caret) caret.textContent = isOpen ? "▸" : "▾";
    if (isOpen) state.namesOpen.delete(key); else state.namesOpen.add(key);
  });
  return head;
}
// A record leaf. Click the row to view its content; click the caret to follow the
// record's REAL Merkle edges — past the calendar, the tree becomes the DAG itself.
function fileLeaf(entry) {
  const group = node("div", "ft-group");
  const row = node("div", "ft-row ft-file" + (entry.locked ? " locked" : ""));
  const caret = node("span", "disclosure ft-caret", "▸");
  caret.title = "Follow the records this one links to (its Merkle edges)";
  const seconds = Number(entry.created_at || 0);
  const time = seconds > 0
    ? new Date(seconds * 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", timeZone: "UTC" })
    : "--:--";
  const desc = entry.locked ? "🔒 Locked record" : (entry.preview || "(no description)");
  row.append(caret, node("span", "ft-icon", "📄"), node("span", "event-kind", entry.kind || "node"),
    node("span", "event-desc", desc), node("span", "event-time", time), node("span", "event-cid", shortCid(entry.cid)));
  row.title = (entry.names && entry.names.length ? entry.names : [entry.name]).join("\n") + "\n" + entry.cid;
  const body = node("div", "ft-body ft-indent"); body.style.display = "none";
  bindMerkleCaret(caret, body, entry.cid);
  row.addEventListener("click", () => loadRecord(entry.cid, true));
  group.append(row, body);
  return group;
}
// Lazily fetch a record's outbound CID links (the Merkle edges) and render each as
// a child node — which itself expands the same way, walking the DAG by content hash.
async function expandMerkleLinks(cid, body) {
  const rec = await getJson("/api/record?cid=" + encodeURIComponent(cid));
  clear(body);
  const links = (rec && rec.links) || [];
  if (!links.length) { body.append(node("div", "ft-empty", "— a root · nothing links out from here")); return; }
  links.forEach(l => body.append(merkleNode(l.cid, l.relation)));
}
function bindMerkleCaret(caret, body, cid) {
  let loaded = false;
  caret.addEventListener("click", e => {
    e.stopPropagation();
    const open = body.style.display !== "none";
    body.style.display = open ? "none" : "block";
    caret.textContent = open ? "▸" : "▾";
    if (!open && !loaded) { loaded = true; safely(() => expandMerkleLinks(cid, body)); }
  });
}
// One hop in the Merkle walk: the edge's relation + the target CID. Row opens the
// linked record; caret follows ITS links, recursively.
function merkleNode(cid, relation) {
  const group = node("div", "ft-group ft-indent");
  const row = node("div", "ft-row ft-link");
  const caret = node("span", "disclosure ft-caret", "▸");
  row.append(caret, node("span", "ft-rel", relation || "→"), node("span", "ft-icon", "🔗"),
    node("span", "event-cid", shortCid(cid)));
  row.title = cid + (relation ? "\nedge: " + relation : "");
  const body = node("div", "ft-body ft-indent"); body.style.display = "none";
  bindMerkleCaret(caret, body, cid);
  row.addEventListener("click", () => loadRecord(cid, true));
  group.append(row, body);
  return group;
}
function renderGraphTree(names, container) {
  if (!container) return;
  clear(container);
  if (!names || !names.length) { container.append(node("div", "empty", "No records yet — capture or ingest some, and they'll appear here.")); return; }
  const months = new Map();
  names.forEach(binding => {
    const seconds = Number(binding.created_at || 0);
    let mKey, dKey, mLabel, dLabel, sort;
    if (seconds > 0) {
      const date = new Date(seconds * 1000);
      mKey = date.getUTCFullYear() + "-" + String(date.getUTCMonth() + 1).padStart(2, "0");
      dKey = mKey + "-" + String(date.getUTCDate()).padStart(2, "0");
      mLabel = date.toLocaleDateString(undefined, { year: "numeric", month: "long", timeZone: "UTC" });
      dLabel = date.toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric", timeZone: "UTC" });
      sort = date.getTime();
    } else { mKey = dKey = "undated"; mLabel = "Undated"; dLabel = "No timestamp"; sort = -1; }
    const sess = sessionOf(binding.names);
    if (!months.has(mKey)) months.set(mKey, { label: mLabel, sort, count: 0, days: new Map() });
    const m = months.get(mKey); m.count++; m.sort = Math.max(m.sort, sort);
    if (!m.days.has(dKey)) m.days.set(dKey, { label: dLabel, sort, count: 0, sessions: new Map() });
    const d = m.days.get(dKey); d.count++; d.sort = Math.max(d.sort, sort);
    if (!d.sessions.has(sess)) d.sessions.set(sess, { label: sessionLabel(sess), sort, entries: [], hasLinks: false });
    const s = d.sessions.get(sess); s.sort = Math.max(s.sort, sort); s.entries.push({ ...binding, sort });
    if (binding.linked) s.hasLinks = true;
  });
  const sortedMonths = [...months.entries()].sort((a, b) => b[1].sort - a[1].sort);
  // On first view, open the newest month so the tree isn't a wall of closed folders.
  if (!state.graphTreeInit && sortedMonths.length) { state.namesOpen.add("g:" + sortedMonths[0][0]); state.graphTreeInit = true; }
  sortedMonths.forEach(([mKey, m]) => {
    const mOpen = state.namesOpen.has("g:" + mKey);
    const mBody = node("div", "ft-body"); mBody.style.display = mOpen ? "block" : "none";
    const mGroup = appendChildren(node("div", "ft-group"), ftFolder(mOpen, "📁", m.label, m.count, "g:" + mKey, mBody), mBody);
    [...m.days.entries()].sort((a, b) => b[1].sort - a[1].sort).forEach(([dKey, d]) => {
      const dOpen = state.namesOpen.has("g:" + dKey);
      const dBody = node("div", "ft-body"); dBody.style.display = dOpen ? "block" : "none";
      const dGroup = appendChildren(node("div", "ft-group ft-indent"), ftFolder(dOpen, "📁", d.label, d.count, "g:" + dKey, dBody), dBody);
      [...d.sessions.entries()].sort((a, b) => b[1].sort - a[1].sort).forEach(([sKey, s]) => {
        const sk = "g:" + dKey + ":" + sKey;
        const sOpen = state.namesOpen.has(sk);
        const sBody = node("div", "ft-body"); sBody.style.display = sOpen ? "block" : "none";
        s.entries.sort((a, b) => a.sort - b.sort).forEach(entry => sBody.append(fileLeaf(entry)));
        dBody.append(appendChildren(node("div", "ft-group ft-indent"), ftFolder(sOpen, "🗂", s.label, s.entries.length, sk, sBody, s.hasLinks ? "🔗" : null), sBody));
      });
      mBody.append(dGroup);
    });
    container.append(mGroup);
  });
}
// Append ONE record into the already-rendered tree at its month/day/session — no
// re-render. IPLD is append-only, so a write is just a new leaf + bumped counts.
// Returns false if the target folder isn't present yet (caller reloads once).
function insertTreeRecord(entry) {
  const tree = byId("graph-tree"); if (!tree) return false;
  const seconds = Number(entry.created_at || 0); if (seconds <= 0) return false;
  const d = new Date(seconds * 1000);
  const m = d.getUTCFullYear() + "-" + String(d.getUTCMonth() + 1).padStart(2, "0");
  const day = m + "-" + String(d.getUTCDate()).padStart(2, "0");
  const sKey = "g:" + day + ":" + sessionOf(entry.names);
  const sHead = tree.querySelector('.ft-folder-row[data-key="' + sKey + '"]');
  if (!sHead) return false; // session folder not rendered → caller falls back to reload
  const sBody = sHead.parentElement && sHead.parentElement.querySelector(":scope > .ft-body");
  if (!sBody) return false;
  sBody.append(fileLeaf(entry));
  bumpFolderCount(tree, sKey);
  bumpFolderCount(tree, "g:" + day);
  bumpFolderCount(tree, "g:" + m);
  if (entry.linked) ensureSessionBadge(sHead);
  return true;
}
function bumpFolderCount(tree, key) {
  const head = tree.querySelector('.ft-folder-row[data-key="' + key + '"]'); if (!head) return;
  const c = head.querySelector(".count"); if (!c) return;
  c.textContent = ((parseInt(c.textContent, 10) || 0) + 1) + " rec";
}
function ensureSessionBadge(sHead) {
  if (sHead.querySelector(".ft-link-badge")) return;
  const b = node("span", "ft-link-badge", "🔗");
  b.title = "Contains records that link out (Merkle edges)";
  sHead.insertBefore(b, sHead.querySelector(".count") || null);
}
function isSynthetic(cid) {
  return cid.startsWith("store:") || cid.startsWith("session:") ||
         cid.startsWith("year:") || cid.startsWith("month:") || cid.startsWith("day:");
}

// The record is a popup window, not a tab: it opens only when a record is
// explicitly selected (Records tab, graph node, search hit, or a link within it).
// Passive refreshes (after a lock/clear) update the content but never pop it open.
function openRecordModal() { byId("record-modal").style.display = "flex"; }
function closeRecordModal() { byId("record-modal").style.display = "none"; }
async function loadRecord(cid, open = false) {
  setSelected(cid); // privacy panel targets the opened record (advances privacySeq)
  logSystem("access " + shortCid(cid), "dim");
  safely(refreshPrivacy);
  const record = await getJson("/api/record?cid=" + encodeURIComponent(cid));
  const container = byId("record"); clear(container); container.className = "content";
  const top = node("div", "record-top");
  top.append(node("span", "cid", record.cid));
  if (record.live !== false) top.append(node("span", "kind", record.kind));
  if (record.live !== false) {
    const privBtn = node("button", "tool-button rec-privacy", "🔒 Privacy");
    privBtn.title = "Privacy & publication state for this record";
    privBtn.addEventListener("click", () => openPrivacyModal());
    top.append(privBtn);
  }
  container.append(top);
  // Pin this record to an always-on service so a copy survives off-device. Same
  // four providers as the Studio, plus "Keep hot on my node" (sovereign — served to
  // your paired devices over your private swarm) and a Public/Private toggle.
  if (record.live !== false) {
    const pinBar = buildRecordPinBar(record.cid);
    container.append(pinBar);
    // Surface whether this record is already kept hot on this node.
    safely(async () => {
      const hot = await getJson("/api/hot-pins");
      if ((hot.pins || []).some(p => p.source_cid === record.cid)) {
        pinBar._slot.append(node("div", "pin-hint", "🔥 Kept hot on your private node — served to your paired devices while this node is up."));
      }
    });
  }
  // Decision 0026: fenced from egress by default — never hidden locally. Show an
  // egress badge (fenced is the norm), then the full content below.
  if (record.locked) {
    container.append(node("div", "review", "Fenced from egress (local-only). Clear for export to allow publish / share."));
  }
  if (record.live === false) {
    container.append(node("h2", "", "Tombstone receipt"), node("pre", "", record.tombstone || "Tombstoned"));
  } else {
    const preview = mediaPreview(record);
    if (preview) container.append(preview);
    const rawWrap = node("details", "raw-record");
    rawWrap.append(node("summary", "", "Raw record"), node("pre", "", JSON.stringify(record.record, null, 2)));
    if (!preview) rawWrap.open = true;
    container.append(rawWrap, node("h2", "", "Outbound links"));
    if (!record.links.length) container.append(node("div", "empty", "No outbound links."));
    record.links.forEach(link => {
      const button = node("button", "link");
      button.append(node("strong", "", link.relation), node("span", "cid", link.cid));
      button.addEventListener("click", () => loadRecord(link.cid, true));
      container.append(button);
    });
  }
  if (open) openRecordModal();
}
// The four pinning providers offered in the record window — identical to the Studio
// pin menu (free tiers only; no pay gate).
const RECORD_PIN_SERVICES = [
  { key: "filebase", name: "Filebase", limit: "5 GB free" },
  { key: "pinata", name: "Pinata", limit: "1 GB free" },
  { key: "foureverland", name: "4EVERLAND", limit: "free tier" },
  { key: "ipfs", name: "IPFS pinning service", limit: "any PSA endpoint" },
];

// A pin bar for the open record: a Public/Private toggle + the same provider menu as
// the Studio. Private encrypts the subgraph on-device first, then uploads only the
// opaque ciphertext (the service blind-pins what it cannot read).
function buildRecordPinBar(cid) {
  const bar = node("div", "record-pinbar");
  bar.dataset.mode = "private"; // records default to private (encrypted blind-pin)

  const mode = node("div", "pin-mode");
  const priv = node("button", "pin-mode-btn active", "🔒 Private");
  const pub = node("button", "pin-mode-btn", "🌐 Public");
  priv.type = "button"; pub.type = "button";
  priv.title = "Encrypt before upload — the service stores ciphertext only you can read";
  pub.title = "Plaintext — anyone with the CID can read it";
  priv.addEventListener("click", () => { bar.dataset.mode = "private"; priv.classList.add("active"); pub.classList.remove("active"); });
  pub.addEventListener("click", () => { bar.dataset.mode = "public"; pub.classList.add("active"); priv.classList.remove("active"); });
  mode.append(priv, pub);

  const group = node("div", "publish-group");
  const btn = node("button", "tool-button", "📌 Pin ▾");
  btn.type = "button";
  btn.title = "Keep a copy of this record online even when this node is off";
  const menu = node("div", "publish-menu");
  // Sovereign option first: keep it hot on YOUR own always-on node, served to your
  // paired devices over the private swarm — no third party.
  const nodeItem = node("button", "menu-item");
  nodeItem.type = "button";
  nodeItem.style.color = "var(--gold)";
  nodeItem.style.borderBottom = "1px solid var(--line)";
  nodeItem.append(node("span", "", "🔥 Keep hot on my node"), node("span", "limit", "private swarm · no 3rd party"));
  nodeItem.addEventListener("click", () => safely(() => recPin(cid, "node", bar)));
  menu.append(nodeItem);
  RECORD_PIN_SERVICES.forEach(s => {
    const item = node("button", "menu-item");
    item.type = "button";
    item.append(node("span", "", s.name), node("span", "limit", s.limit));
    item.addEventListener("click", () => safely(() => recPin(cid, s.key, bar)));
    menu.append(item);
  });
  const settings = node("button", "menu-item", "📌 Connect pinning accounts");
  settings.type = "button";
  settings.style.borderTop = "1px solid var(--line)";
  settings.style.color = "var(--gold)";
  settings.addEventListener("click", () => pinOpen());
  menu.append(settings);
  group.append(btn, menu);

  const slot = node("div", "pin-slot");
  bar.append(mode, group, slot);
  bar._slot = slot;
  return bar;
}

async function recPin(cid, service, bar) {
  const onNode = service === "node";
  const dest = onNode
    ? "your private node"
    : ((typeof PIN_META !== "undefined" && PIN_META[service]) ? PIN_META[service].name : service);
  // Connect gate: open the same guided walk-through if an external service isn't set up
  // yet. The private node is local — no account to connect.
  if (!onNode) {
    let status = {}; try { status = await getJson("/api/pin/credentials"); } catch (e) {}
    if (!status[service]) {
      notice("Connect your " + dest + " account to pin there.");
      pinOpen(service);
      return;
    }
  }
  // "Keep hot on my node" is always private (your devices hold the key); external pins
  // honour the Public/Private toggle.
  const isPrivate = onNode ? true : (bar.dataset.mode !== "public");
  const slot = bar._slot; clear(slot);
  const label = onNode
    ? "🔥 Keep hot on my node"
    : (isPrivate ? "🔒 Pin privately to " : "🌐 Pin publicly to ") + dest;
  const form = passwordAction(label, async (password) => {
    if (!password) { notice("Your store password is required — pinning is egress."); return; }
    notice(onNode
      ? "Encrypting + seeding onto your private node…"
      : "Pinning to " + dest + (isPrivate ? " (encrypting first)…" : "…"));
    const res = await postJson("/api/record/pin", { cid, service, private: isPrivate, password });
    clear(slot);
    const done = onNode
      ? "Kept hot on your private node (encrypted) · ciphertext " + shortCid(res.cid) + " — served to your paired devices over the private swarm while this node is up."
      : (isPrivate
        ? "Pinned privately to " + dest + " (" + (res.status || "queued") + ") · ciphertext " + shortCid(res.cid) + " — only you can read it."
        : "Pinned publicly to " + dest + " (" + (res.status || "queued") + ") · " + (res.url || res.cid));
    slot.append(node("div", "pin-hint", done));
    notice(done);
    logSystem("record · " + (onNode ? "kept hot " : "pinned ") + shortCid(cid) + (isPrivate ? " (encrypted)" : " (public)") + " → " + service, "ok");
  });
  slot.append(
    node("div", "pin-hint", onNode
      ? "Sovereign: encrypted on-device, then seeded onto your own always-on node. No third party — your paired devices pull it over your private swarm and index it locally."
      : (isPrivate
        ? "Private: this record is encrypted on-device, then only the ciphertext is uploaded."
        : "Public: the plaintext is uploaded — anyone with the CID can read it.")),
    form
  );
  const input = form.querySelector("input"); if (input) input.focus();
}
// "Kept hot" manager: the records this node is serving to your paired devices over the
// private swarm. List them with an unpin (stop serving) control.
async function loadHotManager() {
  byId("hot-modal").style.display = "flex";
  const list = byId("hot-list"); clear(list);
  list.append(node("div", "pin-hint", "Loading…"));
  let data; try { data = await getJson("/api/hot-pins"); } catch (e) { clear(list); list.append(node("div", "empty", "Could not load.")); return; }
  const pins = data.pins || [];
  clear(list);
  if (!pins.length) { list.append(node("div", "empty", "Nothing kept hot yet. Open a record → 📌 Pin ▾ → 🔥 Keep hot on my node.")); return; }
  pins.forEach(p => {
    const row = node("div", "hot-row");
    const info = node("div", "hot-info");
    info.append(node("div", "hot-cid", shortCid(p.source_cid)));
    info.append(node("div", "pin-hint", (p.private ? "🔒 encrypted" : "🌐 public") + " · ciphertext " + shortCid(p.pinned_cid)));
    const open = node("button", "tool-button", "Open");
    open.addEventListener("click", () => { loadRecord(p.source_cid, true); });
    const stop = node("button", "c-remove", "Unpin");
    stop.title = "Stop serving this record from your node (the original stays in your store).";
    stop.addEventListener("click", () => safely(async () => {
      if (!window.confirm("Stop keeping this record hot? Your paired devices will no longer pull it from this node.")) return;
      await postJson("/api/record/unpin", { cid: p.source_cid });
      await loadHotManager();
      notice("Stopped keeping it hot.");
    }));
    row.append(info, open, stop);
    list.append(row);
  });
}
// Modal open/close/backdrop/Escape are all handled by the delegated listeners above
// (data-action="open|close", data-modal, data-onopen, data-close). Nothing to bind here.

// Build an inline preview for file_ref / blob records: images, video, audio,
// and PDFs render in-page; text/code is fetched and shown; everything else
// (Office docs, archives, unknown binaries) gets an open/download card.
function mediaPreview(record) {
  const body = (record.record && record.record.body) || record.record || {};
  let mediaType = body.media_type || "";
  let filename = body.path ? String(body.path).split("/").pop() : "";
  let blobCid = "";
  if (record.kind === "file_ref") {
    const content = (record.links || []).find(link => link.relation === "content");
    blobCid = content ? content.cid : "";
  } else if (record.kind === "blob") {
    blobCid = record.cid;
  } else {
    return null;
  }
  if (!blobCid) return null;
  const url = "/api/blob?cid=" + encodeURIComponent(blobCid);
  const wrap = node("div", "media-preview");
  if (filename) wrap.append(node("div", "media-name", filename));

  const isImage = mediaType.startsWith("image/");
  const isVideo = mediaType.startsWith("video/");
  const isAudio = mediaType.startsWith("audio/");
  const isPdf = mediaType === "application/pdf";
  const isText = mediaType.startsWith("text/") || mediaType === "application/json" || mediaType === "image/svg+xml";

  if (isImage) {
    const img = node("img", "media-img"); img.src = url; img.alt = filename || "image";
    wrap.append(img);
  } else if (isVideo) {
    const video = node("video", "media-video"); video.src = url; video.controls = true; video.preload = "metadata";
    wrap.append(video);
  } else if (isAudio) {
    const audio = node("audio", ""); audio.src = url; audio.controls = true; audio.style.width = "100%";
    wrap.append(audio);
  } else if (isPdf) {
    const frame = node("iframe", "media-frame"); frame.src = url; frame.title = filename || "PDF";
    wrap.append(frame);
    wrap.append(fileActions(url, filename));
  } else if (isText) {
    const pre = node("pre", "media-text", "Loading…");
    wrap.append(pre);
    safely(async () => {
      const response = await fetch(url);
      const text = await response.text();
      pre.textContent = text.length > 200000 ? text.slice(0, 200000) + "\n… (truncated)" : text;
    });
  } else {
    // Office documents, archives, unknown binaries: no native in-page render.
    wrap.append(node("div", "media-icon", "▣"));
    wrap.append(node("div", "review", (mediaType || "binary") + " — open or download to view"));
    wrap.append(fileActions(url, filename));
  }
  return wrap;
}

function fileActions(url, filename) {
  const row = node("div", "media-actions");
  const open = node("a", "pbtn", "Open in new tab"); open.href = url; open.target = "_blank"; open.rel = "noopener";
  const dl = node("a", "pbtn", "Download"); dl.href = url + "&download=1"; if (filename) dl.setAttribute("download", filename);
  row.append(open, dl);
  return row;
}
// The System Console: a live terminal of everything the concierge does. Append a
// timestamped line; keep the last ~200; auto-scroll only if already at the bottom.
function logSystem(text, cls) {
  // Keep the minimized header console showing the latest line, even when collapsed.
  const mini = byId("console-mini-line"); if (mini) { mini.textContent = text; mini.className = "console-mini-line " + (cls || "dim"); }
  const c = byId("system-console"); if (!c) return;
  const atBottom = c.scrollTop + c.clientHeight >= c.scrollHeight - 6;
  const ln = node("div", "ln");
  ln.append(node("span", "t", new Date().toLocaleTimeString(undefined, { hour12: false }) + "  "));
  ln.append(node("span", cls || "dim", text));
  c.append(ln);
  while (c.childElementCount > 200) c.firstChild.remove();
  if (atBottom) c.scrollTop = c.scrollHeight;
}
// Minimized System Console: click the header strip to expand the full log; the inner
// pop swallows clicks so it stays open while you read/scroll.
(function wireMiniConsole() {
  const mini = byId("console-mini"), pop = byId("console-pop");
  if (!mini || !pop) return;
  const toggle = open => {
    pop.style.display = open ? "block" : "none";
    mini.classList.toggle("open", open);
    if (open) { const c = byId("system-console"); if (c) c.scrollTop = c.scrollHeight; }
  };
  mini.addEventListener("click", () => toggle(pop.style.display === "none"));
  mini.addEventListener("keydown", e => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(pop.style.display === "none"); } });
  pop.addEventListener("click", e => e.stopPropagation());
  document.addEventListener("click", e => { if (!mini.contains(e.target)) toggle(false); });
})();
// Privacy & Publication popup — opened per-record from the record popup (see renderRecord).
// Refresh on open so it reflects the current selection.
function openPrivacyModal() { byId("privacy-modal").style.display = "flex"; safely(refreshPrivacy); }
// privacy-modal close/backdrop/Escape handled by the delegated listeners (data-action="close").
// The server-truth half of the System Console: pull everything the concierge has
// done since our last poll (embedder load, indexing, retrieval, every action,
// inbound messages) and print it, plus the active embedding model — so the user can
// see the plugin does what it says. (loadStats() adds the store-delta lines.)
async function pollActivity() {
  const data = await getJson("/api/activity?since=" + (state.activitySeq || 0));
  if (data.embedder) {
    const e = data.embedder;
    const dims = (e.built && e.dims) ? " · " + e.dims + "d" : "";
    const el = byId("sys-embedder");
    if (el) el.textContent = "embedder: " + e.id + dims + (e.built ? "" : " (loads on first search)");
  }
  (data.entries || []).forEach(entry => logSystem(entry.text, entry.level));
  if (typeof data.next_seq === "number") state.activitySeq = data.next_seq;
}
async function loadStats() {
  const query = state.rootMode === "fixed" && state.root ? "?cid=" + encodeURIComponent(state.root) : "";
  const stats = await getJson("/api/stats" + query);
  // Emit what changed in the store since the last poll — the node ingesting/binding.
  // (Compaction runs silently in the background, so its tombstones aren't surfaced.)
  if (state.lastStats) {
    const db = stats.blocks - state.lastStats.blocks, dn = stats.names - state.lastStats.names;
    if (db > 0) logSystem("ingested " + db + " block" + (db > 1 ? "s" : "") + "  ·  " + stats.blocks + " total", "ev");
    if (dn > 0) logSystem("bound " + dn + " name" + (dn > 1 ? "s" : ""), "ev");
  }
  state.lastStats = stats;
  // Phase B: publishing is opt-in. Remember readiness so the publish review can
  // present "not set up yet" guidance instead of a raw error.
  state.publishingReady = !!stats.publishing_ready;
}

function passwordAction(label, action) {
  const form = node("form", "pform");
  const input = node("input"); input.type = "password"; input.placeholder = "password"; input.autocomplete = "current-password";
  const submit = node("button", "pbtn", label); submit.type = "submit";
  form.append(input, submit);
  form.addEventListener("submit", event => {
    event.preventDefault();
    safely(async () => {
      try { await action(input.value); }
      finally { input.value = ""; }
    });
  });
  return form;
}

function passwordSetupAction(label, action) {
  const form = node("form", "pform");
  const password = node("input"); password.type = "password"; password.placeholder = "create password"; password.autocomplete = "new-password";
  const confirm = node("input"); confirm.type = "password"; confirm.placeholder = "confirm password"; confirm.autocomplete = "new-password";
  const submit = node("button", "pbtn", label); submit.type = "submit";
  form.append(password, confirm, submit);
  form.addEventListener("submit", event => {
    event.preventDefault();
    safely(async () => {
      try {
        if (!password.value || password.value !== confirm.value) throw new Error("Password confirmation does not match.");
        await action(password.value, confirm.value);
      } finally {
        password.value = "";
        confirm.value = "";
      }
    });
  });
  return form;
}

// Decision 0026: clearing a root for export is the explicit, password-gated act
// that lifts the default fence. It confirms the reach (what would become
// exportable) and takes the existing store password.
function openClearReview(target, info) {
  const panel = byId("privacy");
  panel.querySelectorAll(".clear-review").forEach(item => item.remove());
  const form = node("form", "pform clear-review");
  form.append(node("div", "review", "Clearing reaches " + info.reachable_node_count + " node(s) and " + info.file_count + " file path(s) — these become exportable."));
  const password = node("input"); password.type = "password"; password.placeholder = "password"; password.autocomplete = "current-password";
  form.append(password);
  const submit = node("button", "pbtn danger", "Confirm clear for export"); submit.type = "submit";
  form.append(submit);
  form.addEventListener("submit", event => {
    event.preventDefault();
    safely(async () => {
      try {
        if (!password.value) throw new Error("Enter the store password to clear for export.");
        const current = await getJson("/api/privacy?cid=" + encodeURIComponent(target));
        if (current.reachable_node_count !== info.reachable_node_count || current.file_count !== info.file_count) {
          notice("The subgraph changed. Review the updated clear counts.");
          openClearReview(target, current);
          return;
        }
        const warning = "Clear this subgraph for export?\n\n" + current.reachable_node_count + " reachable node(s)\n" + current.file_count + " file path(s)\n\nIt will no longer be fenced from publish / share / export.";
        if (!window.confirm(warning)) return;
        await postJson("/api/clear-for-egress", { target, password: password.value });
        notice("Cleared " + shortCid(target) + " for export.");
        await Promise.all([refreshPrivacy(), loadStats()]);
        if (state.selected) await loadRecord(state.selected);
      } finally {
        password.value = "";
      }
    });
  });
  panel.append(form);
}

// Privacy panel for a whole session. Decision 0026: every record is fenced from
// egress by default, so this panel surfaces how many have been *cleared for
// export* and lets the user clear the whole session at once (or re-fence it).
async function renderSessionPrivacy(panel, sessionCid, seq) {
  const sessionId = sessionCid.slice("session:".length);
  const graph = state.fullGraph;
  const sessionNode = graph && graph.nodes.find(n => n.cid === sessionCid);
  const childCids = graph ? graph.edges.filter(e => e.from === sessionCid).map(e => e.to) : [];
  const childNodes = childCids.map(c => graph.nodes.find(n => n.cid === c)).filter(Boolean);
  const total = (sessionNode && sessionNode.count) || childCids.length;
  const clearedCount = childNodes.filter(n => n.cleared || n.known_public).length;
  // Bail before painting if the selection already moved on (see `setSelected`).
  if (seq !== privacySeq) return;
  clear(panel);

  const badges = node("div", "");
  if (total > 0 && clearedCount >= total) badges.append(node("span", "pstate public", "Session cleared for export"));
  else if (clearedCount > 0) badges.append(node("span", "pstate inherited", clearedCount + " of " + total + " cleared"));
  else badges.append(node("span", "pstate locked", "Fenced / local-only"));
  panel.append(badges);
  panel.append(node("div", "cid", (sessionNode && sessionNode.description) || ("Session " + sessionId.slice(0, 8))));
  panel.append(node("div", "review", total + " record(s) in this session — fenced from egress by default. Content stays fully visible here."));

  // Apply an egress action across every record in the session client-side, reusing
  // the single-root endpoints so no bulk core path is needed. Returns a count.
  const eachRecord = async (path, extra) => {
    let count = 0;
    for (const cid of childCids) {
      try { await postJson(path, Object.assign({ target: cid }, extra)); count += 1; }
      catch (_) { /* skip records with nothing to change */ }
    }
    return count;
  };

  const meta = await getJson("/api/meta");
  if (seq !== privacySeq) return;

  // Re-fence the whole session (the safe direction — no password).
  if (clearedCount > 0) {
    const refence = node("button", "pbtn", "Re-fence session (" + clearedCount + " cleared)");
    refence.addEventListener("click", () => safely(async () => {
      if (!window.confirm("Re-fence all cleared records in this session? They will be local-only again.")) return;
      const n = await eachRecord("/api/refence", {});
      childNodes.forEach(node => { node.cleared = false; });
      notice("Re-fenced " + n + " record(s).");
      await Promise.all([loadStats(), refreshPrivacy()]);
      if (state.selected) await loadRecord(state.selected);
    }));
    panel.append(refence);
  }

  // Clear the whole session for export (password-gated).
  if (clearedCount < total) {
    const remaining = total - clearedCount;
    if (!meta.password_set) {
      panel.append(node("div", "review", "Create and confirm the store password before clearing for export."));
      panel.append(passwordSetupAction("Set password & clear session", async (password, confirmPassword) => {
        await postJson("/api/set-password", { password, confirm_password: confirmPassword });
        const n = await eachRecord("/api/clear-for-egress", { password });
        childNodes.forEach(node => { node.cleared = true; });
        notice("Cleared " + n + " record(s) for export.");
        await Promise.all([loadStats(), refreshPrivacy()]);
      }));
    } else {
      panel.append(passwordAction("Clear session for export (" + remaining + " record" + (remaining === 1 ? "" : "s") + ")", async password => {
        if (!window.confirm("Clear all " + remaining + " fenced record(s) in this session for export?")) return;
        const n = await eachRecord("/api/clear-for-egress", { password });
        childNodes.forEach(node => { node.cleared = true; });
        notice("Cleared " + n + " record(s) for export.");
        await Promise.all([loadStats(), refreshPrivacy()]);
        if (state.selected) await loadRecord(state.selected);
      }));
    }
  }
}

// The privacy panel runs the full egress/sensitivity scan, which is cheap for a
// small record but can take many seconds on a huge subgraph. It is secondary
// info, so it must never hold the blocking overlay: callers can `await
// refreshPrivacy()` but it returns immediately and does the heavy work in the
// background (`quietly`). Overlapping requests coalesce into a single re-run so
// the 5s poll and rapid selections can't stack expensive scans.
let privacyRunning = false;
let privacyRerun = false;
function refreshPrivacy() {
  if (privacyRunning) { privacyRerun = true; return Promise.resolve(); }
  privacyRunning = true;
  quietly(async () => {
    try {
      do { privacyRerun = false; await refreshPrivacyBody(); } while (privacyRerun);
    } finally { privacyRunning = false; }
  });
  return Promise.resolve();
}
async function refreshPrivacyBody() {
  const target = state.selected || state.root;
  const seq = privacySeq;
  const panel = byId("privacy");
  if (!target) { clear(panel); panel.append(node("div", "empty", "Select a root to see its privacy state.")); return; }
  if (target.startsWith("session:")) { await renderSessionPrivacy(panel, target, seq); return; }
  if (isSynthetic(target)) { clear(panel); panel.append(node("div", "empty", "Select a session or record node to see its privacy state.")); return; }
  let info;
  try { info = await getJson("/api/privacy?cid=" + encodeURIComponent(target)); }
  catch (error) { if (seq !== privacySeq) return; clear(panel); panel.append(node("div", "pblockers", error.message)); return; }
  // A newer selection superseded this one while the scan was in flight — drop this
  // render so the panel never shows a different node than the one now highlighted.
  if (seq !== privacySeq) return;
  clear(panel);

  // Decision 0026: fenced from egress by DEFAULT. The exception worth surfacing is
  // a root that has been explicitly cleared for export (it can leave the device).
  const badges = node("div", "");
  if (info.known_public) badges.append(node("span", "pstate public", "Known public"));
  else if (info.cleared) badges.append(node("span", "pstate public", "Cleared for export"));
  else badges.append(node("span", "pstate locked", "Fenced / local-only"));
  if (info.encrypted_private_copy) badges.append(node("span", "pstate encrypted", "Encrypted-private copy"));
  panel.append(badges, node("div", "cid", shortCid(info.root)));
  if (info.encrypted_private_copy) panel.append(node("div", "review", "private: " + shortCid(info.encrypted_private_copy)));
  panel.append(node("div", "review", info.reachable_node_count + " reachable node(s) / " + info.file_count + " file path(s)"));

  if (info.sensitivity_warnings && info.sensitivity_warnings.length) {
    panel.append(node("div", "pblockers", "⚠ " + info.sensitivity_warnings.length + " sensitive finding(s)"));
  }

  // Clear for export: the explicit, password-gated act that lifts the default
  // fence so this subgraph can be published / shared / exported.
  if (!info.cleared && !info.known_public) {
    if (info.password_set) {
      const clearBtn = node("button", "pbtn danger", "Clear for export");
      clearBtn.addEventListener("click", () => openClearReview(target, info));
      panel.append(clearBtn);
    } else {
      panel.append(node("div", "review", "Create and confirm the store password before clearing anything for export."));
      panel.append(passwordSetupAction("Set password & clear for export", async (password, confirmPassword) => {
        await postJson("/api/set-password", { password, confirm_password: confirmPassword });
        await postJson("/api/clear-for-egress", { target, password });
        notice("Cleared " + shortCid(target) + " for export.");
        await Promise.all([refreshPrivacy(), loadStats()]);
        if (state.selected) await loadRecord(state.selected);
      }));
    }
  }

  if (info.blocked && info.blocking_locks.length) {
    const blockers = node("div", "pblockers");
    blockers.append(node("div", "", info.blocked_node_count + " fenced node(s), " + info.blocked_file_count + " fenced file path(s) block this export:"));
    info.blocking_locks.forEach(hit => {
      const label = "focus " + shortCid(hit.lock_root) + " / " + hit.intersecting_count + " node(s) / " + hit.intersecting_file_paths.length + " file path(s)" + (hit.lock_label ? " (" + hit.lock_label + ")" : "");
      const focus = node("button", "", label);
      focus.addEventListener("click", () => safely(() => loadRecord(hit.lock_root, true)));
      blockers.append(focus);
    });
    panel.append(blockers);
  }

  // Re-fence: restore the default fence on a previously-cleared root. This makes
  // the data *more* private, so it needs no password.
  if (info.cleared) {
    panel.append(node("div", "review", "Re-fencing restores local-only protection and blocks export again."));
    const refence = node("button", "pbtn", "Re-fence (make private)");
    refence.addEventListener("click", () => safely(async () => {
      if (!window.confirm("Re-fence this subgraph? It will be local-only again and can no longer be exported.")) return;
      await postJson("/api/refence", { target });
      notice("Re-fenced — subgraph is local-only again.");
      await Promise.all([refreshPrivacy(), loadStats()]);
      if (state.selected) await loadRecord(state.selected);
    }));
    panel.append(refence);
  }

  if (!info.password_set) {
    panel.append(passwordSetupAction("Set store password", async (password, confirmPassword) => {
      await postJson("/api/set-password", { password, confirm_password: confirmPassword });
      notice("Store password set.");
      await refreshPrivacy();
    }));
  } else {
    const review = node("button", "pbtn", "Review & publish publicly");
    review.addEventListener("click", () => safely(() => openPublishReview(target)));
    panel.append(review);
    const convert = node("button", "pbtn", info.encrypted_private_copy ? "Re-encrypt and share private" : "Convert to encrypted private and share");
    convert.addEventListener("click", () => safely(() => openPrivateReview(target)));
    panel.append(convert);
  }
}

function openPrivateReview(target) {
  const panel = byId("privacy");
  panel.querySelectorAll(".private-review").forEach(item => item.remove());
  const form = node("form", "pform private-review");
  const namespace = node("input"); namespace.type = "text"; namespace.placeholder = "destination namespace, e.g. team:wetlands";
  const recipients = node("input"); recipients.type = "text"; recipients.placeholder = "recipient AgentIDs, comma-separated";
  const review = node("button", "pbtn", "Review private conversion and share"); review.type = "submit";
  form.append(
    node("div", "review", "The source plaintext stays local-only. Only new ciphertext CIDs and a read-only bearer capability may be shared."),
    namespace,
    recipients,
    review
  );
  form.addEventListener("submit", event => {
    event.preventDefault();
    safely(async () => {
      const recipientList = recipients.value.split(",").map(value => value.trim()).filter(Boolean);
      const plan = await getJson("/api/egress-plan?op=private&cid=" + encodeURIComponent(target) +
        "&namespace=" + encodeURIComponent(namespace.value.trim()) +
        "&recipients=" + encodeURIComponent(recipientList.join(",")));
      clear(form);
      form.append(node("div", "review", "Source review: " + plan.source.block_count + " plaintext block(s) / " + formatBytes(plan.source.byte_size)));
      form.append(node("div", "review", "Exact source manifest digest: " + plan.source.manifest_digest));
      form.append(node("div", "review", "Destination namespace: " + plan.destination_namespace));
      appendReviewList(form, "Exact source CID manifest", plan.source.manifest);
      appendReviewList(form, "Reviewed recipients", plan.recipients);
      if (plan.source.known_public_receipts) {
        form.append(node("div", "pblockers", "KNOWN PUBLIC: encryption cannot revoke " + plan.source.known_public_receipts + " prior public publication receipt(s)."));
      }
      const password = node("input"); password.type = "password"; password.placeholder = "password"; password.autocomplete = "current-password";
      const ack = node("input"); ack.type = "checkbox";
      const ackLabel = node("label", ""); ackLabel.append(ack, node("span", "", "I reviewed the exact source manifest, destination namespace, and recipients."));
      const submit = node("button", "pbtn", "Authorize one conversion and private share"); submit.type = "button";
      submit.addEventListener("click", () => safely(async () => {
        try {
          if (!ack.checked) throw new Error("Acknowledge the private-share review first.");
          const result = await postJson("/api/convert-private", {
            review_token: plan.review_token,
            password: password.value,
            acknowledge_private: true,
          });
          notice("Private ciphertext " + shortCid(result.ciphertext_root) + " created; plaintext remains locked.");
          const capability = node("details", "");
          capability.append(
            node("summary", "", "Read-only bearer capability for reviewed recipients"),
            node("div", "pblockers", "Treat this capability as a secret. Anyone holding it can decrypt the authorized subgraph."),
            node("pre", "", JSON.stringify(result.capability, null, 2))
          );
          form.replaceChildren(capability);
          await Promise.all([refreshPrivacy(), loadStats()]);
        } finally {
          password.value = "";
        }
      }));
      form.append(password, ackLabel, submit);
    });
  });
  panel.append(form);
}

async function openPublishReview(target) {
  const plan = await getJson("/api/egress-plan?op=publish&cid=" + encodeURIComponent(target));
  const panel = byId("privacy");
  panel.querySelectorAll(".publish-review").forEach(item => item.remove());
  const form = node("form", "pform publish-review");
  // Phase B: publishing is opt-in. If the selected backend isn't running, say so
  // up front as setup guidance — not an error. The review/gate still works; the
  // publish itself will report the unreachable node if they proceed.
  if (!state.publishingReady) {
    form.append(node("div", "review", "Publishing isn't set up on this machine — it's optional. Start a local IPFS node (Kubo) at " + plan.backend_target + " to enable. Everything else works offline."));
  }
  form.append(node("div", "review", "Review: " + plan.block_count + " blocks / " + formatBytes(plan.byte_size)));
  form.append(node("div", "review", "Backend: " + plan.backend + " / " + plan.network_posture));
  form.append(node("div", "review", "Exact manifest digest: " + plan.manifest_digest));
  appendReviewList(form, "Exact CID manifest", plan.manifest);
  appendReviewList(form, "File paths", plan.file_paths);
  appendReviewList(form, "Media types", plan.media_types);
  if (plan.blocking_locks.length) {
    const blockedNodes = new Set(plan.blocking_locks.flatMap(hit => hit.intersecting_cids)).size;
    const blockedFiles = new Set(plan.blocking_locks.flatMap(hit => hit.intersecting_file_paths)).size;
    form.append(node("div", "pblockers", plan.blocking_locks.length + " lock root(s) intersect " + blockedNodes + " node(s) and " + blockedFiles + " file path(s). Password authorization will be one-shot and consumed immediately."));
  }
  if (plan.sensitivity_warnings.length) form.append(node("div", "pblockers", "⚠ sensitive: " + plan.sensitivity_warnings.join("; ")));
  if (plan.known_public_receipts) form.append(node("div", "pblockers", "KNOWN PUBLIC: " + plan.known_public_receipts + " prior publication receipt(s). Locking cannot make this private again."));
  const password = node("input"); password.type = "password"; password.placeholder = "password"; password.autocomplete = "current-password";
  const ack = node("input"); ack.type = "checkbox";
  const ackLabel = node("label", ""); ackLabel.append(ack, node("span", "", "I understand this is an irreversible public publication."));
  const submit = node("button", "pbtn danger", "Authorize one public publish"); submit.type = "submit";
  form.append(password, ackLabel, submit);
  form.addEventListener("submit", event => {
    event.preventDefault();
    safely(async () => {
      try {
        if (!ack.checked) throw new Error("Acknowledge irreversibility first.");
        const receipt = await postJson("/api/authorize-publish", { review_token: plan.review_token, password: password.value, acknowledge_irreversible: true });
        notice("Published " + shortCid(receipt.root) + " via " + receipt.backend + "; one-shot authorization consumed.");
        await Promise.all([refreshPrivacy(), loadStats()]);
      } finally {
        password.value = "";
      }
    });
  });
  panel.append(form);
}
function appendReviewList(form, label, values) {
  const details = node("details", "");
  const summary = node("summary", "", label + " (" + values.length + ")");
  details.append(summary);
  values.forEach(value => details.append(node("div", "", value)));
  form.append(details);
}
async function loadRooms() {
  const list = byId("room-list"); if (!list) return;
  const rooms = await getJson("/api/rooms"); clear(list);
  rooms.forEach(room => { const option = node("option"); option.value = room; list.append(option); });
}
// "View thread" opens a popup listing your threads (rooms). Clicking one loads it
// inline and closes the popup.
async function showThreadPicker() {
  const picker = byId("thread-picker"); if (!picker) return;
  let rooms = [];
  try { rooms = await getJson("/api/rooms"); } catch (e) { rooms = []; }
  clear(picker);
  if (!rooms.length) {
    picker.append(node("div", "empty", "No threads yet — a thread appears once you exchange messages in a room or with an approved peer."));
  } else {
    rooms.forEach(room => {
      const row = node("div", "thread-row");
      const item = node("button", "thread-pick" + (room === state.room ? " active" : ""), room);
      item.addEventListener("click", () => { byId("thread-modal").style.display = "none"; safely(() => loadThread(room)); });
      // Delete the thread (forget the room pointer). The signed messages stay in
      // the store; this just clears a stale/legacy thread out of the messenger.
      const del = node("button", "thread-del", "✕");
      del.title = "Delete this thread";
      del.addEventListener("click", e => {
        e.stopPropagation();
        safely(async () => {
          if (!window.confirm("Delete this thread? Its messages stay in your store, but the thread is removed from the messenger.")) return;
          await postJson("/api/thread/delete", { room });
          if (state.room === room) { state.room = ""; const c = byId("thread"); if (c) clear(c); }
          await showThreadPicker();
        });
      });
      row.append(item, del);
      picker.append(row);
    });
  }
  byId("thread-modal").style.display = "flex";
}
// Render the latest *incoming* message (not your own) inline in the bottom DM bar, so a
// reply shows on the same strip without opening Messenger. Hidden when there's none.
function updateChatIncoming(messages) {
  const strip = byId("chat-incoming"); if (!strip) return;
  const incoming = (messages || []).filter(m => !(state.myUsername && m.author === state.myUsername));
  const last = incoming[incoming.length - 1];
  if (!last) { strip.style.display = "none"; clear(strip); return; }
  clear(strip);
  const from = last.nickname || shortCid(last.author);
  strip.append(node("span", "ci-from", "▸ " + from), node("span", "ci-text", last.payload));
  strip.style.display = "flex";
  strip.onclick = () => { const tab = document.querySelector('[data-view="network"]'); if (tab) tab.click(); };
}
async function loadThread(room) {
  room = (room || state.room || "").trim(); if (!room) return;
  state.room = room;
  const thread = await getJson("/api/thread?room=" + encodeURIComponent(room));
  const container = byId("thread"); clear(container);
  // Moderator badge (Phase 8 §3/§4): the Guardian watches every room — show its
  // policy + flag synthesis candidates so the host can summarize long threads.
  const mod = thread.moderation;
  if (mod) {
    const badge = node("div", "moderator-badge");
    const sendLabel = mod.ai_send === "off" ? "Human-only" : (mod.ai_send === "on_mention" ? "AI on mention" : "AI allowed");
    badge.append(node("span", "mod-guardian", "⊙ Guardian active"));
    badge.append(node("span", "mod-policy", sendLabel));
    if (mod.muted_count > 0) badge.append(node("span", "mod-muted", mod.muted_count + " muted"));
    badge.append(node("span", "mod-count", mod.message_count + " msgs"));
    if (mod.synthesis_candidate) badge.append(node("span", "mod-synth", "synthesis candidate (≥" + mod.synthesis_threshold + ")"));
    container.append(badge);
  }
  const visible = thread.messages.filter(message => !state.muted.has(message.author) && !(state.humanOnly && state.roles.get(message.author) === "ai"));
  // Feed the bottom DM bar's inline incoming field with the latest received message,
  // so you see replies without opening the Messenger tab.
  updateChatIncoming(visible);
  if (!visible.length) { container.append(node("div", "empty", "No visible messages in this room.")); return; }
  visible.forEach(message => {
    // Stagger the thread: your messages sit on the right, messages received from the
    // peer on the left, each in its own colour.
    const mine = !!state.myUsername && message.author === state.myUsername;
    const item = node("article", "message " + (mine ? "mine" : "theirs")); const head = node("div", "message-head");
    const label = message.nickname || shortCid(message.author);
    head.append(node("span", "", "[" + message.clock + "] " + label));
    const role = node("button", "", state.roles.get(message.author) === "ai" ? "mark human" : "mark AI");
    role.addEventListener("click", () => { state.roles.set(message.author, state.roles.get(message.author) === "ai" ? "human" : "ai"); loadThread(); });
    const mute = node("button", "", "mute participant");
    mute.addEventListener("click", () => { state.muted.add(message.author); loadThread(); });
    const reveal = node("button", "", "reveal CID");
    reveal.addEventListener("click", () => { reveal.textContent = message.cid; });
    head.append(role, mute, reveal);
    // Phase I — structural importance + follow lens (all local). The trust-tier badge
    // was removed from the bubble header for a friendlier, less cluttered chat.
    if (message.followed) head.append(Object.assign(node("span", "trust-follow", "following"), { title: "Authored by someone you follow" }));
    if (message.importance > 0) head.append(Object.assign(node("span", "trust-importance", "ties " + message.importance), { title: "How many decisions/files this message ties together (structural importance, not popularity)" }));
    item.append(head, node("p", "", message.payload)); container.append(item);
  });
}
function commandHelp(command) {
  const commands = {
    Ingest: "Write operation: use concierge-plugin ingest <file.jsonl> in a terminal.",
    Put: "Write operation: use the mem put surface in a terminal.",
    Bind: "Write operation: use mem bind <name> <cid> in a terminal.",
    Resolve: "Read operation: use concierge-plugin recall <name>, or select Names here.",
    Share: "Publish operation: use concierge-plugin share <root> in a terminal.",
    GC: "Destructive operation: use mem gc after reviewing roots.",
    Backends: "Backend status is shown in the right stats rail.",
    Init: "Write operation: use concierge-plugin init in the target store directory."
  };
  notice(commands[command] || command);
}

// Ingest from a path on disk: a file, a folder, or a whole repo. The loopback
// server reads the path directly, so large media and repos need no upload.
function openIngestModal() {
  const existing = byId("ingest-overlay");
  if (existing) existing.remove();
  const overlay = node("div", "modal-overlay"); overlay.id = "ingest-overlay";
  const form = node("form", "pform modal-card");
  form.append(node("div", "modal-title", "Ingest from disk"));
  const input = node("input", "input");
  input.type = "text"; input.placeholder = "/absolute/path/to/file-or-folder";
  input.spellcheck = false;
  form.append(input);
  form.append(node("div", "review", "Files, folders and repos ingest recursively, with no size or type limit. .git / node_modules / target are skipped. A single .jsonl is read as a harness session."));
  const actions = node("div", "modal-actions");
  const go = node("button", "pbtn", "Ingest"); go.type = "submit";
  const cancel = node("button", "pbtn", "Cancel"); cancel.type = "button";
  cancel.addEventListener("click", () => overlay.remove());
  actions.append(go, cancel); form.append(actions);
  overlay.append(form);
  overlay.addEventListener("click", event => { if (event.target === overlay) overlay.remove(); });
  form.addEventListener("submit", event => {
    event.preventDefault();
    const path = input.value.trim();
    if (!path) return;
    go.disabled = true; go.textContent = "Ingesting…";
    safely(async () => {
      try {
        const report = await postJson("/api/ingest-path", { path });
        let message;
        if (report.kind === "session") {
          message = "Ingested session: " + report.events + " events, " + report.nodes_written + " nodes";
        } else if (report.kind === "folder") {
          message = "Ingested folder: " + report.files + " files, " + formatBytes(report.bytes) + (report.ignored ? ", " + report.ignored + " skipped" : "");
        } else {
          message = "Ingested file (" + formatBytes(report.bytes || 0) + ")";
        }
        notice(message);
        overlay.remove();
        // Append the new import as a single leaf (folder/file ingest returns the
        // record). A session stream returns no records yet, so it reloads the tree.
        const now = Math.floor(Date.now() / 1000);
        const recs = report.records || [];
        if (recs.length && recs.every(r => insertTreeRecord({ cid: r.cid, created_at: now, kind: r.kind, preview: r.preview, linked: r.linked, names: r.names }))) {
          await loadStats();
        } else {
          await Promise.all([loadGraphTree(), loadStats()]);
        }
      } finally {
        go.disabled = false; go.textContent = "Ingest";
      }
    });
  });
  document.body.append(overlay);
  input.focus();
}


// Pillar A: pull Brave/Opera bookmarks into memory (they become searchable records).
// bookmarks-sync handled by delegation (data-action="bookmarksSync").


// Phase N · Phase H — the Private Network Map: identity hierarchy, networks, this
// device's scopes + their validity, epoch health, and who is revoked.
async function loadNetwork() {
  let data;
  try { data = await getJson("/api/network"); } catch (e) { return; }
  const idBar = byId("network-identity"); clear(idBar);
  idBar.append(node("span", "", "UserID (root): "), Object.assign(node("b", "", data.user_id ? shortCid(data.user_id) : "—"), { title: data.user_id || "" }));
  idBar.append(node("span", "", "This device: "), Object.assign(node("b", "", data.device_id ? shortCid(data.device_id) : "—"), { title: data.device_id || "" }));

  const map = byId("network-map"); clear(map);
  if (!data.networks.length) { map.append(node("div", "empty", "No private network yet. Click 📲 Pair a device to share this node with your other computer — it creates the network and walks you through pairing.")); return; }
  data.networks.forEach(net => {
    const card = node("div", "net-card");
    const head = node("h3", "", net.name);
    if (net.is_root) head.append(node("span", "net-pill root", "root"));
    head.append(node("span", "net-pill " + (net.descriptor_valid ? "ok" : "stale"), net.descriptor_valid ? "verified" : "INVALID"));
    head.append(node("span", "net-pill", "epoch " + net.membership_epoch));
    card.append(head);
    card.append(Object.assign(node("div", "net-meta", net.network_id), { title: net.network_id }));

    if (net.device_membership) {
      const m = node("div", "net-cap");
      m.append(node("span", "net-pill " + (net.device_membership.valid ? "ok" : "stale"), net.device_membership.valid ? "member" : "membership stale"));
      m.append(node("span", "", "this device can: " + (net.device_membership.capabilities || []).join(", ")));
      card.append(m);
    }
    (net.capabilities || []).forEach(cap => {
      const row = node("div", "net-cap");
      row.append(node("span", "net-pill " + (cap.valid ? "ok" : "stale"), cap.valid ? "valid" : "stale"));
      const code = node("code", "", cap.namespace); row.append(code);
      row.append(node("span", "", "→ " + cap.operations.join(", ")));
      card.append(row);
    });
    if ((net.revoked || []).length) {
      card.append(node("div", "net-revoked", "Revoked: " + net.revoked.map(shortCid).join(", ")));
    }
    // Revoke control (root only).
    if (net.is_root) {
      const row = node("div", "net-revoke-row");
      const input = node("input", "input"); input.placeholder = "subject id to revoke"; input.style.fontSize = "9px";
      const btn = node("button", "tool-button", "Revoke");
      btn.addEventListener("click", () => safely(async () => {
        const subject = input.value.trim(); if (!subject) return;
        await postJson("/api/network/revoke", { subject });
        await loadNetwork();
        notice("Revoked. Epoch advanced — re-grant remaining devices.");
      }));
      row.append(input, btn); card.append(row);
    }
    map.append(card);
  });
}
// network-create handled by delegation (data-action="networkCreate"; Enter via data-enter).

// ── Pairing wizard: share this node with your other computer (or join one). A guided
// offer → response → grant exchange (copy/paste between the two machines), with a safety
// phrase to compare — the secret never travels with these blobs. ──
function pairOverlay(title) {
  const old = byId("pair-overlay"); if (old) old.remove();
  const overlay = node("div", "modal-overlay"); overlay.id = "pair-overlay";
  overlay.style.zIndex = "26"; // above the Private network popup (record-modal z=22) it opens from
  const card = node("div", "pform modal-card"); card.style.maxWidth = "580px";
  const bar = node("div", "pair-head");
  const close = node("button", "record-close", "✕"); close.title = "Close (Esc)";
  close.addEventListener("click", () => overlay.remove());
  bar.append(node("div", "modal-title", title), close);
  const body = node("div", "pair-body"); card.append(bar, body);
  overlay.append(card);
  overlay.addEventListener("click", e => { if (e.target === overlay) overlay.remove(); });
  document.addEventListener("keydown", function esc(e) { if (e.key === "Escape" && byId("pair-overlay")) { overlay.remove(); document.removeEventListener("keydown", esc); } });
  document.body.append(overlay);
  return { overlay, body };
}
function pairStep(body) { clear(body); for (let i = 1; i < arguments.length; i++) body.append(arguments[i]); }
function pairCopyBox(value) {
  const wrap = node("div", "pair-copy");
  const ta = document.createElement("textarea"); ta.className = "input"; ta.rows = 5; ta.readOnly = true; ta.spellcheck = false; ta.value = value;
  const copy = node("button", "tool-button", "Copy");
  copy.addEventListener("click", () => navigator.clipboard.writeText(value).then(() => { const o = copy.textContent; copy.textContent = "Copied!"; setTimeout(() => copy.textContent = o, 1500); }).catch(() => notice("Copy failed — select the text and copy it.")));
  wrap.append(ta, copy); return wrap;
}
function pairPasteBox(placeholder) {
  const ta = document.createElement("textarea"); ta.className = "input"; ta.rows = 5; ta.spellcheck = false; ta.placeholder = placeholder;
  return ta;
}
function pairParse(text) { try { return JSON.parse(text.trim()); } catch (e) { notice("That doesn't look right — copy the WHOLE block of text and paste it again."); return null; } }

async function pairShareWizard() {
  const { overlay, body } = pairOverlay("📲 Pair another device");
  pairStep(body, node("div", "review", "Generating a one-use pairing offer…"));
  let res;
  try { res = await postJson("/api/network/pair/offer", {}); }
  catch (e) { pairStep(body, node("div", "review", "Couldn't start pairing: " + (e.message || e))); return; }
  const offer = res.offer;
  function stepOffer() {
    const next = node("button", "pbtn ckpt-load", "Next: paste their response ▸");
    next.addEventListener("click", stepResponse);
    pairStep(body,
      node("div", "review", "Step 1 — on your OTHER computer: open Network → 🔗 Join a network, and paste this offer:"),
      pairCopyBox(JSON.stringify(offer)), next);
  }
  function stepResponse() {
    const paste = pairPasteBox("Paste the response your other computer produced…");
    const check = node("button", "pbtn ckpt-load", "Check safety phrase ▸");
    const out = node("div", "");
    check.addEventListener("click", () => safely(async () => {
      const response = pairParse(paste.value); if (!response) return;
      const p = await postJson("/api/network/pair/phrase", { offer, response });
      const sel = document.createElement("select"); sel.className = "input";
      sel.append(new Option("Full access (read + write) — share the whole node", "full"));
      sel.append(new Option("Read-only — the device can read, not change", "read"));
      const approve = node("button", "pbtn danger", "✓ Phrase matches — Approve & grant");
      approve.addEventListener("click", () => safely(async () => {
        const g = await postJson("/api/network/pair/approve", { response, scope: sel.value });
        stepGrant(g.grant);
      }));
      clear(out);
      out.append(
        node("div", "review", "Step 2 — confirm this phrase matches the one shown on your other computer:"),
        node("div", "pair-phrase", p.phrase),
        node("div", "review", "Then choose what this device may do, and approve:"), sel, approve);
    }));
    pairStep(body, node("div", "review", "Step 2 — paste the response from your other computer:"), paste, check, out);
  }
  function stepGrant(grant) {
    const done = node("button", "pbtn", "Done");
    done.addEventListener("click", () => { overlay.remove(); safely(loadNetwork); });
    pairStep(body,
      node("div", "review", "Step 3 — paste this grant onto your other computer (its “Join a network” final step) to finish:"),
      pairCopyBox(JSON.stringify(grant)), done);
  }
  stepOffer();
}

async function pairJoinWizard() {
  const { overlay, body } = pairOverlay("🔗 Join a network");
  const paste = pairPasteBox("Paste the pairing offer from your main computer…");
  const go = node("button", "pbtn ckpt-load", "Continue ▸");
  go.addEventListener("click", () => safely(async () => {
    const offer = pairParse(paste.value); if (!offer) return;
    const r = await postJson("/api/network/pair/respond", { offer });
    stepResponse(r);
  }));
  function stepResponse(r) {
    const next = node("button", "pbtn ckpt-load", "Next: paste the grant ▸");
    next.addEventListener("click", stepGrant);
    pairStep(body,
      node("div", "review", "Step 2 — confirm this phrase matches your main computer:"),
      node("div", "pair-phrase", r.phrase),
      node("div", "review", "…and send this response back to your main computer (paste it into its “Pair a device” window):"),
      pairCopyBox(JSON.stringify(r.response)), next);
  }
  function stepGrant() {
    const gp = pairPasteBox("Paste the grant your main computer produced…");
    const finish = node("button", "pbtn danger", "Finish — join");
    finish.addEventListener("click", () => safely(async () => {
      const grant = pairParse(gp.value); if (!grant) return;
      const res = await postJson("/api/network/pair/accept", { grant });
      const done = node("button", "pbtn", "Done");
      done.addEventListener("click", () => { overlay.remove(); safely(loadNetwork); });
      pairStep(body, node("div", "review", "✓ Joined network “" + res.network + "” with " + res.capabilities + " capability — this device can now sync."), done);
      notice("Joined! Your devices are now paired.");
    }));
    pairStep(body, node("div", "review", "Step 3 — paste the grant from your main computer to finish:"), gp, finish);
  }
  pairStep(body, node("div", "review", "Step 1 — paste the pairing offer your main computer gave you:"), paste, go);
}

// pair-share / pair-join handled by delegation (data-action="pairShare" / "pairJoin").

// ── Network discovery map: this node (center) + the peers libp2p discovers around it ──
let discPoll = null;
// A small pulsating brain — the same glyph as the graph centre, scaled down.
function discBrain(cx, cy, size, cls) {
  const g = svgNode("g", { class: "disc-node " + (cls || "") });
  const halo = svgNode("circle", { cx: cx, cy: cy, r: size * 0.7, class: "disc-halo" });
  halo.style.animationDelay = (Math.random() * 1.8).toFixed(2) + "s"; // stagger so they don't blink in unison
  g.append(halo);
  g.append(svgNode("image", { href: "/api/brain.png", x: cx - size / 2, y: cy - size / 2, width: size, height: size, class: "disc-brain" }));
  return g;
}
// ── World map: equirectangular projection over the 600×360 viewBox ──
const MAP = { x0: 0, y0: 30, w: 600, h: 300 };
function mapX(lon) { return MAP.x0 + (lon + 180) / 360 * MAP.w; }
function mapY(lat) { return MAP.y0 + (90 - lat) / 180 * MAP.h; }
// Real coastlines/borders (Natural Earth 110m, projected to this map's space), loaded
// once. Until it arrives the map just shows the grid.
let DISC_LAND_PATHS = null;
async function loadWorldMap() {
  if (DISC_LAND_PATHS) return;
  try { DISC_LAND_PATHS = await getJson("/worldmap.json"); } catch (e) { DISC_LAND_PATHS = []; }
}
// Deterministic, region-weighted location for a peer (no geo-IP yet — see note in the
// caption). Stable per peer id, biased toward where nodes actually cluster, so the map
// reads as a believable global distribution rather than a random scatter.
function discHash(s) { let h = 2166136261 >>> 0; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619); } return h >>> 0; }
const DISC_REGIONS = [
  [-125,-70, 30, 50, 5], [-10, 30, 36, 60, 6], [ 70,140, 22, 46, 6],
  [-60,-38,-35, 2, 2], [ 12, 45,-32, 8, 2], [110,152,-39,-14, 1], [ 30, 60, 24, 42, 2],
];
function discGeo(id) {
  const h = discHash(id), h2 = discHash(id + "~");
  const totalW = DISC_REGIONS.reduce((s, r) => s + r[4], 0);
  let pick = h % totalW, region = DISC_REGIONS[0];
  for (const r of DISC_REGIONS) { if (pick < r[4]) { region = r; break; } pick -= r[4]; }
  const fx = ((h >>> 8) & 1023) / 1023, fy = ((h2 >>> 6) & 1023) / 1023;
  return { lon: region[0] + fx * (region[1] - region[0]), lat: region[2] + fy * (region[3] - region[2]) };
}
// Clicking a Concierge brain stages a DM to it: drop its username in the recipient
// field, focus the message box, and copy the username to the clipboard.
function discDmPeer(username) {
  if (!username) return;
  const to = byId("chat-to"); if (to) to.value = username;
  if (navigator.clipboard) navigator.clipboard.writeText(username).catch(() => {});
  const msg = byId("chat-msg"); if (msg) msg.focus();
  notice("Ready to DM " + username.slice(0, 10) + "… — username copied; type a message and Send.");
}
// A plain white network-node dot. No halo/glow and no per-element twinkle
// animation — at hundreds of peers those (a second circle + an infinite CSS
// animation each) were the map's main render cost. Just one cheap circle now.
function discStar(cx, cy, connected) {
  return svgNode("circle", {
    cx: cx, cy: cy, r: connected ? 0.6 : 0.45,
    class: "disc-star " + (connected ? "on" : "off"),
  });
}
// ── Discovery map zoom + pan ──────────────────────────────────────────────
// All drawn content lives in a single <g id="disc-zoom-root"> so one transform
// zooms/pans the whole map. The view state is module-level so it survives the
// 4s re-render poll (we just re-apply it after each render).
const DISC_VIEW = { k: 1, x: 0, y: 0 };
const DISC_MIN_K = 1, DISC_MAX_K = 14;
// viewBox is cropped to the populated latitude band (no empty poles): origin (0,40),
// size 600x240. Zoom/pan math is relative to this viewBox rect.
const DISC_VB_X0 = 0, DISC_VB_Y0 = 40, DISC_VB_W = 600, DISC_VB_H = 240;
let discBound = false;
function applyDiscTransform() {
  const root = byId("disc-zoom-root");
  if (root) root.setAttribute("transform",
    "translate(" + DISC_VIEW.x.toFixed(2) + " " + DISC_VIEW.y.toFixed(2) + ") scale(" + DISC_VIEW.k.toFixed(4) + ")");
}
// Keep the map covering the viewport — can't pan the world entirely off-screen.
function clampDiscPan() {
  const k = DISC_VIEW.k;
  DISC_VIEW.x = Math.min(DISC_VB_X0 * (1 - k), Math.max((DISC_VB_X0 + DISC_VB_W) * (1 - k), DISC_VIEW.x));
  DISC_VIEW.y = Math.min(DISC_VB_Y0 * (1 - k), Math.max((DISC_VB_Y0 + DISC_VB_H) * (1 - k), DISC_VIEW.y));
}
// Client (mouse) point → viewBox coordinates.
function discSvgPoint(svg, clientX, clientY) {
  const ctm = svg.getScreenCTM(); if (!ctm) return { x: 0, y: 0 };
  const pt = svg.createSVGPoint(); pt.x = clientX; pt.y = clientY;
  const p = pt.matrixTransform(ctm.inverse());
  return { x: p.x, y: p.y };
}
// Zoom by `factor`, keeping the viewBox point (cx,cy) fixed under the cursor.
function discZoomAt(cx, cy, factor) {
  const k0 = DISC_VIEW.k;
  const k = Math.max(DISC_MIN_K, Math.min(DISC_MAX_K, k0 * factor));
  if (k === k0) return;
  const wx = (cx - DISC_VIEW.x) / k0, wy = (cy - DISC_VIEW.y) / k0;
  DISC_VIEW.k = k;
  DISC_VIEW.x = cx - wx * k;
  DISC_VIEW.y = cy - wy * k;
  clampDiscPan();
  applyDiscTransform();
}
function discResetView() { DISC_VIEW.k = 1; DISC_VIEW.x = 0; DISC_VIEW.y = 0; applyDiscTransform(); }
function bindDiscInteractions() {
  if (discBound) return;
  const svg = byId("discovery-svg"); if (!svg) return;
  discBound = true;
  svg.addEventListener("wheel", (e) => {
    e.preventDefault();
    const p = discSvgPoint(svg, e.clientX, e.clientY);
    discZoomAt(p.x, p.y, e.deltaY < 0 ? 1.18 : 1 / 1.18);
  }, { passive: false });
  let panning = false, lastX = 0, lastY = 0;
  svg.addEventListener("mousedown", (e) => {
    panning = true; lastX = e.clientX; lastY = e.clientY; svg.classList.add("disc-panning");
  });
  window.addEventListener("mousemove", (e) => {
    if (!panning) return;
    const ctm = svg.getScreenCTM(); if (!ctm) return;
    DISC_VIEW.x += (e.clientX - lastX) / ctm.a;
    DISC_VIEW.y += (e.clientY - lastY) / ctm.d;
    lastX = e.clientX; lastY = e.clientY;
    clampDiscPan(); applyDiscTransform();
  });
  window.addEventListener("mouseup", () => { panning = false; svg.classList.remove("disc-panning"); });
  const ctr = { x: DISC_VB_X0 + DISC_VB_W / 2, y: DISC_VB_Y0 + DISC_VB_H / 2 };
  const bind = (id, fn) => { const b = byId(id); if (b) b.addEventListener("click", fn); };
  bind("disc-zoom-in", () => discZoomAt(ctr.x, ctr.y, 1.4));
  bind("disc-zoom-out", () => discZoomAt(ctr.x, ctr.y, 1 / 1.4));
  bind("disc-zoom-reset", discResetView);
  bind("disc-maximize", () => { const w = byId("discovery-wrap"); if (w) w.classList.toggle("maximized"); });
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") { const w = byId("discovery-wrap"); if (w) w.classList.remove("maximized"); }
  });
}
let discLastSig = "";
function renderDiscovery(data) {
  const svg = byId("discovery-svg"); if (!svg) return;
  const peers = (data && data.peers) || [];
  // Skip the rebuild when nothing meaningful changed — avoids re-creating thousands of
  // SVG nodes every poll. Signature covers online state + each peer's id/status/kind.
  const sig = (data && data.self && data.self.online ? "1" : "0") + "|" + ((data && data.total) || 0)
    + "|" + peers.map(p => p.peer_id + p.status + (p.is_concierge ? "b" : "s")).join(",");
  if (sig === discLastSig && byId("disc-zoom-root")) return;
  discLastSig = sig;
  clear(svg);
  const root = svgNode("g", { id: "disc-zoom-root", class: "disc-zoom-root" });
  svg.append(root);
  // World map: fine graticule + real coastlines/borders.
  const grid = svgNode("g", { class: "disc-grid" });
  for (let lon = -180; lon <= 180; lon += 20) grid.append(svgNode("line", { x1: mapX(lon), y1: MAP.y0, x2: mapX(lon), y2: MAP.y0 + MAP.h }));
  for (let lat = -80; lat <= 80; lat += 20) grid.append(svgNode("line", { x1: MAP.x0, y1: mapY(lat), x2: MAP.x0 + MAP.w, y2: mapY(lat) }));
  root.append(grid);
  if (DISC_LAND_PATHS && DISC_LAND_PATHS.length) {
    const land = svgNode("g", { class: "disc-land-g" });
    DISC_LAND_PATHS.forEach(d => land.append(svgNode("path", { d: d, class: "disc-land" })));
    root.append(land);
  }
  // Peers: fellow Concierges as brains (labelled), everyone else as a white LED star.
  // How many sit at a TRUE geo-IP location (vs the stylised region fallback for
  // relay/LAN-only peers with no public IP) — surfaced honestly in the caption.
  const located = peers.filter(p => typeof p.lat === "number" && typeof p.lon === "number").length;
  peers.forEach(p => {
    // Real geo-IP from the bundled DB when we have it; stylised region only as fallback.
    const g = (typeof p.lat === "number" && typeof p.lon === "number")
      ? { lat: p.lat, lon: p.lon } : discGeo(p.peer_id || "");
    const x = mapX(g.lon), y = mapY(g.lat);
    const connected = p.status === "connected";
    let el;
    if (p.is_concierge) {
      // Brains are sized to ~2× a LED star (a LED is ~1.9 across via its halo).
      el = discBrain(x, y, connected ? 3.8 : 3.2, connected ? "connected" : "discovered");
      const label = svgNode("text", { x: x, y: y + (connected ? 4 : 3.4), class: "disc-label", "text-anchor": "middle" });
      label.textContent = (p.peer_id || "").slice(-6);
      el.append(label);
      if (p.username) {
        el.classList.add("disc-dm");
        el.addEventListener("click", () => discDmPeer(p.username));
      }
    } else {
      el = discStar(x, y, connected);
    }
    const title = svgNode("title", {});
    title.textContent = (p.is_concierge ? "Concierge · " : "Network node · ") + (p.peer_id || "") + " · " + p.status + " · via " + p.source + (p.relayed ? " · relayed" : "") + (p.is_concierge && p.username ? " · click to DM" : "");
    el.append(title);
    root.append(el);
  });
  // Your node — at its real geo-IP location when the backend resolved one, else the
  // stylised fallback (e.g. behind NAT with no public address yet).
  const online = !!(data && data.self && data.self.online);
  const sg = (data && data.self && typeof data.self.lat === "number")
    ? { lat: data.self.lat, lon: data.self.lon }
    : discGeo((data && data.self && data.self.peer_id) || "self");
  const meX = mapX(sg.lon), meY = mapY(sg.lat);
  const me = discBrain(meX, meY, 3.8, "disc-self");
  const slabel = svgNode("text", { x: meX, y: meY + 4, class: "disc-label self", "text-anchor": "middle" });
  slabel.textContent = online ? "your node" : "your node (offline)";
  me.append(slabel);
  const stitle = svgNode("title", {});
  stitle.textContent = "This node · " + ((data && data.self && data.self.peer_id) || "");
  me.append(stitle);
  root.append(me);
  // Re-apply the current zoom/pan so it persists across the 4s re-render.
  applyDiscTransform();
  // Caption.
  const stat = byId("discovery-stat"); clear(stat);
  const total = (data && data.total) || peers.length;
  if (!online) stat.textContent = "Node offline — open this tab to bring it online and start discovering peers.";
  else if (!peers.length) stat.textContent = "Searching the network… no peers yet. mDNS finds LAN peers instantly; the DHT and rendezvous take a moment.";
  else stat.textContent = total + " node" + (total === 1 ? "" : "s") + " discovered on the libp2p network (mDNS · DHT · rendezvous) — Concierges as brains, others as stars · " + located + " at real geo-IP location · Geo © DB-IP (CC BY 4.0)";
  renderConciergeList(peers);
}
// The left-hand roster: every discovered Concierge node, clickable to start a DM
// (same as clicking its brain on the map). Non-Concierge "star" nodes are omitted.
function renderConciergeList(peers) {
  const list = byId("disc-concierge-list"); if (!list) return;
  clear(list);
  list.append(node("div", "dcl-title", "Concierge nodes"));
  const brains = (peers || []).filter(p => p.is_concierge);
  // Connected first, then a stable order by id.
  brains.sort((a, b) =>
    (a.status === "connected" ? 0 : 1) - (b.status === "connected" ? 0 : 1)
    || (a.peer_id || "").localeCompare(b.peer_id || ""));
  if (!brains.length) {
    list.append(node("div", "dcl-empty", "No Concierge peers discovered yet — they appear here as they join the network."));
    return;
  }
  brains.forEach(p => {
    const connected = p.status === "connected";
    const row = node("button", "disc-cn" + (connected ? " on" : ""));
    const dot = node("span", "dcl-dot");
    dot.style.background = connected ? "var(--patina)" : "var(--faint)";
    if (connected) dot.style.boxShadow = "0 0 6px var(--patina)";
    row.append(dot, node("span", "dcl-name", (p.peer_id || "").slice(-6) || "node"),
      node("span", "dcl-meta", connected ? "connected" : (p.status || "discovered")));
    if (p.username) {
      row.title = "Click to DM " + p.username.slice(0, 12) + "…";
      row.addEventListener("click", () => discDmPeer(p.username));
    } else {
      row.classList.add("no-dm");
      row.disabled = true;
      row.title = "This Concierge hasn't published a username yet — can't DM.";
    }
    list.append(row);
  });
}
async function loadPeers() {
  await loadWorldMap();
  let data; try { data = await getJson("/api/peers"); } catch (e) { return; }
  renderDiscovery(data);
}
function startDiscPoll() { stopDiscPoll(); bindDiscInteractions(); quietly(loadPeers); discPoll = setInterval(() => quietly(loadPeers), 4000); }
function stopDiscPoll() { if (discPoll) { clearInterval(discPoll); discPoll = null; } }

// ── Updates tab: app binary + signed safety rules ────────────────────────────────
function updateRow(key, value) {
  const row = node("div", "wallet-row");
  row.append(node("span", "wallet-k", key), node("span", "wallet-v", value));
  return row;
}
async function loadUpdateStatus() {
  const data = await getJson("/api/update/status");
  const card = byId("update-status"); clear(card);
  card.append(updateRow("App version", data.app_version || "—"));
  const appUpdate = data.app_update || {};
  byId("update-app").textContent = appUpdate.release && appUpdate.release.version
    ? "Running version " + (data.app_version || "—") + ". Update " + appUpdate.release.version + " is available — it installs automatically on next launch."
    : "Running version " + (data.app_version || "—") + ". You're on the latest version.";
}
// update-check handled by delegation (data-action="updateCheck").

// ── Brain tab: the sovereign LLM engine + the on-node embedder ───────────────────
// A compact collapsible <pre> of raw JSON — used wherever a metric provider's exact
// schema isn't known, so we never invent a bar/number for a field that isn't there.
function brainRaw(label, value) {
  const details = node("details", "brain-raw");
  details.append(node("summary", "eyebrow", label));
  const pre = node("pre", "cv-src");
  pre.style.whiteSpace = "pre-wrap";
  pre.textContent = JSON.stringify(value, null, 2);
  details.append(pre);
  return details;
}
function brainRow(key, value) {
  const row = node("div", "wallet-row");
  row.append(node("span", "wallet-k", key), node("span", "wallet-v", value));
  return row;
}
// Last good harness/meta status per key, so a transient fetch failure on one of the
// parallel requests falls back to the previous value instead of hiding a harness.
const brainStatusCache = {};
// The Concierge's PRIMARY brain is the host harness it's mounted to (e.g. Claude Code) — the
// large model that drives it via MCP. The Sovereign LLM below is the optional private alternative.
async function loadBrainHost() {
  // Fetch all harness statuses + meta in PARALLEL. The server handles each request
  // on its own thread, so this collapses six sequential round-trips — each doing a
  // filesystem scan — into one. Doing them one-at-a-time is what made the tab hang.
  // On a transient fetch failure, fall back to the LAST good value so a momentary
  // blip never hides a detected harness (which can look like "only 3 of 4 show").
  const fetchStatus = async (url, key) => {
    try { const v = await getJson(url); brainStatusCache[key] = v; return v; }
    catch (e) { return brainStatusCache[key] || {}; }
  };
  const [cc, aider, codex, gemini, cont, openclaw, cline, cursor, opendevin, copilot, meta] = await Promise.all([
    fetchStatus("/api/claude-code/status", "cc"),
    fetchStatus("/api/aider/status", "aider"),
    fetchStatus("/api/codex/status", "codex"),
    fetchStatus("/api/gemini/status", "gemini"),
    fetchStatus("/api/continue/status", "continue"),
    fetchStatus("/api/openclaw/status", "openclaw"),
    fetchStatus("/api/cline/status", "cline"),
    fetchStatus("/api/cursor/status", "cursor"),
    fetchStatus("/api/opendevin/status", "opendevin"),
    fetchStatus("/api/copilot/status", "copilot"),
    fetchStatus("/api/meta", "meta"),
  ]);
  const host = byId("brain-host");
  if (!host) return;
  clear(host);
  // At most HARNESS_CAP harnesses may capture at once — enforced here by greying
  // out the Attach buttons once the cap is reached (detaching one re-enables them).
  const HARNESS_CAP = 2;
  const capReached = [cc, aider, codex, gemini, cont, openclaw, cline, cursor, opendevin, copilot].filter(s => s && s.attached).length >= HARNESS_CAP;
  // One row per detected harness — the Concierge auto-mounts whichever it finds.
  function row(connected, name, detail, toggle) {
    const head = node("div", "model");
    const d = node("span", "dot", "");
    d.style.background = connected ? "var(--led)" : "var(--faint)";
    d.style.boxShadow = connected ? "0 0 8px var(--led)" : "none";
    head.append(d, node("span", "", name));
    if (toggle) head.append(toggle);
    const det = node("div", "eyebrow", detail);
    det.style.color = "var(--faint)";
    det.style.margin = "2px 0 8px";
    host.append(head, det);
  }
  const declared = meta.mounted_model && meta.mounted_model !== "manual mount" && meta.mounted_model !== "not declared";
  // Every harness the Concierge supports gets a row — detected ones show their
  // session counts and a capture toggle; undetected ones read "not detected". This
  // list grows as new adapters are added. `hostDriver` marks the MCP host (Claude
  // Code), which drives the Concierge rather than being read off disk.
  function harnessRow(st, key, label, hostDriver) {
    const available = !!(st && st.available);
    const attached = !!(st && st.attached);
    const n = (st && st.session_count) || 0, t = (st && st.transcript_count) || 0;
    let name, detail;
    if (!available) {
      name = label + " · not detected";
      detail = "not detected — run " + label + " and its sessions will appear here to capture";
    } else if (hostDriver) {
      name = label + " · connected";
      detail = (attached ? "capturing" : "detected — not attached") + " · " + n + " session" + (n === 1 ? "" : "s") + " · drives the Concierge via MCP";
    } else {
      name = label + " · " + (attached ? "capturing" : "detected");
      detail = n + " session" + (n === 1 ? "" : "s") + " across " + t + " transcript" + (t === 1 ? "" : "s") + (attached ? " · ingesting into memory" : " · attach to capture into memory");
    }
    let controls = null;
    if (available) {
      controls = node("div", "");
      controls.style.cssText = "margin-left:auto;display:flex;gap:6px;align-items:center;";
      // Manual one-time backfill of this harness's prior history (loading no longer
      // auto-ingests it). Runs in the background on the server.
      const ingest = node("button", "tool-button", "Ingest");
      ingest.style.cssText = "padding:2px 12px;font-size:12px;";
      ingest.title = "Backfill all of " + label + "'s past sessions into memory now (one-time).";
      ingest.addEventListener("click", () => safely(async () => {
        await postJson("/api/" + key + "/ingest", {});
        notice("Ingesting " + label + " history in the background — watch the System Console.");
      }));
      const btn = node("button", "tool-button", attached ? "Detach" : "Attach");
      btn.style.cssText = "padding:2px 12px;font-size:12px;";
      if (!attached && capReached) {
        btn.disabled = true;
        btn.title = "Capture limit reached (" + HARNESS_CAP + " max) — detach another harness first.";
      } else {
        btn.addEventListener("click", () => safely(async () => {
          await postJson("/api/" + key + (attached ? "/detach" : "/attach"), {});
          await loadBrainHost();
        }));
      }
      controls.append(ingest, btn);
    }
    row(available, name, detail, controls);
  }
  // The MCP host first. If a model other than Claude Code is declared as the mount,
  // show it as the connected host; otherwise show the Claude Code harness row.
  if (!cc.available && declared) {
    row(true, meta.mounted_model + " · mounted", "drives the Concierge via MCP");
  } else {
    harnessRow(cc, "claude-code", "Claude Code", true);
  }
  // Every supported file-based harness, detected or not.
  harnessRow(aider, "aider", "Aider");
  harnessRow(codex, "codex", "Codex");
  harnessRow(gemini, "gemini", "Gemini");
  harnessRow(cont, "continue", "Continue");
  harnessRow(openclaw, "openclaw", "OpenClaw");
  harnessRow(cline, "cline", "Cline");
  harnessRow(cursor, "cursor", "Cursor");
  harnessRow(opendevin, "opendevin", "OpenDevin");
  harnessRow(copilot, "copilot", "Copilot");
}
async function loadBrainMetrics() {
  loadBrainHost();
  let data; try { data = await getJson("/api/brain/metrics"); } catch (e) { return; }
  const baseline = data.baseline || {};
  // Panel A — engine status card: connection dot + engine name + base_url.
  const dot = document.querySelector("#brain-engine .dot");
  if (dot) {
    dot.style.background = baseline.up ? "var(--led)" : "rgb(var(--vermilion-rgb))";
    dot.style.boxShadow = baseline.up ? "0 0 8px var(--led)" : "none";
  }
  byId("brain-engine-name").textContent = baseline.up
    ? (baseline.engine || "engine") + " · connected"
    : "No local engine detected";
  byId("brain-engine-url").textContent = baseline.up
    ? (baseline.base_url || "")
    : "Optional — no local engine at " + (baseline.base_url || "—") + ". Start one (oMLX, Ollama, …) for private on-device inference; your host above is already the brain.";
  // Model picker — populated from baseline.models, current selection marked.
  const select = byId("brain-model"); clear(select);
  const models = Array.isArray(baseline.models) ? baseline.models : [];
  if (!models.length) {
    const opt = node("option", "", "— no models reported —"); opt.value = ""; select.append(opt);
    select.disabled = true; byId("brain-model-apply").disabled = true;
  } else {
    select.disabled = false; byId("brain-model-apply").disabled = false;
    models.forEach(id => {
      const opt = node("option", "", id); opt.value = id;
      if (id === baseline.active_model) opt.selected = true;
      select.append(opt);
    });
  }
  // Rich metrics — rendered defensively: only fields that exist, else raw JSON.
  renderBrainRich(data.rich);
  // Panel B — embedder.
  renderBrainEmbedder(data.embedder || {});
}
function renderBrainRich(rich) {
  const wrap = byId("brain-rich"); clear(wrap);
  const macmon = rich && rich.macmon;
  const omlx = rich && rich.omlx_stats;
  if (!rich || (macmon == null && omlx == null)) {
    wrap.style.display = "block";
    wrap.append(node("div", "eyebrow", "Rich metrics not available for this engine."));
    return;
  }
  wrap.style.display = "block";
  if (macmon != null) {
    wrap.append(node("div", "eyebrow", "System (macmon)"));
    const mapped = [];
    // Attempt a few optional fields; macmon's exact schema varies by version, so
    // every read is optional and unmapped data falls through to the raw <pre>.
    const cpu = macmon?.cpu_usage ?? macmon?.cpu_percent ?? macmon?.ecpu_usage;
    if (cpu != null) mapped.push(brainRow("CPU", typeof cpu === "number" ? cpu.toFixed(0) + "%" : String(cpu)));
    const gpu = macmon?.gpu_usage ?? macmon?.gpu_percent;
    if (gpu != null) mapped.push(brainRow("GPU", typeof gpu === "number" ? gpu.toFixed(0) + "%" : String(gpu)));
    const memUsed = macmon?.memory?.ram_usage ?? macmon?.mem_used ?? macmon?.memory_used;
    const memTotal = macmon?.memory?.ram_total ?? macmon?.mem_total ?? macmon?.memory_total;
    if (memUsed != null && memTotal != null) {
      mapped.push(brainRow("Memory", formatBytes(memUsed) + " / " + formatBytes(memTotal)));
    } else if (memUsed != null) {
      mapped.push(brainRow("Memory used", formatBytes(memUsed)));
    }
    const swap = macmon?.memory?.swap_usage ?? macmon?.swap_used;
    if (swap != null) mapped.push(brainRow("Swap", formatBytes(swap)));
    mapped.forEach(row => wrap.append(row));
    wrap.append(brainRaw(mapped.length ? "Raw macmon JSON" : "macmon JSON (unrecognized schema)", macmon));
  }
  if (omlx != null) {
    wrap.append(node("div", "eyebrow", "Engine (oMLX)"));
    const mapped = [];
    const weights = omlx?.model_weights ?? omlx?.weights;
    if (weights != null) mapped.push(brainRow("Model weights", typeof weights === "number" ? formatBytes(weights) : String(weights)));
    const hotKv = omlx?.hot_kv_cache ?? omlx?.kv_cache_hot ?? omlx?.kv_cache;
    if (hotKv != null) mapped.push(brainRow("Hot KV cache", typeof hotKv === "number" ? formatBytes(hotKv) : String(hotKv)));
    const pp = omlx?.prompt_tps ?? omlx?.pp;
    if (pp != null) mapped.push(brainRow("Prompt (PP)", typeof pp === "number" ? pp.toFixed(1) + " tok/s" : String(pp)));
    const tg = omlx?.generation_tps ?? omlx?.tg;
    if (tg != null) mapped.push(brainRow("Generation (TG)", typeof tg === "number" ? tg.toFixed(1) + " tok/s" : String(tg)));
    mapped.forEach(row => wrap.append(row));
    wrap.append(brainRaw(mapped.length ? "Raw oMLX stats JSON" : "oMLX stats JSON (unrecognized schema)", omlx));
  }
}
function renderBrainEmbedder(embedder) {
  const wrap = byId("brain-embedder"); clear(wrap);
  wrap.append(brainRow("Backend", embedder.backend || "—"));
  wrap.append(brainRow("Model", embedder.model || "—"));
  if (embedder.shares_engine) wrap.append(node("div", "eyebrow", "Shares the connected engine."));
  wrap.append(brainRow("Indexed nodes", embedder.indexed_nodes == null ? "—" : String(embedder.indexed_nodes)));
  wrap.append(brainRow("Queue depth", embedder.queue_depth == null ? "—" : String(embedder.queue_depth)));
  wrap.append(brainRow("Last latency", embedder.last_latency_ms == null ? "—" : embedder.last_latency_ms + " ms"));
}
// brain-model-apply handled by delegation (data-action="brainModelApply").
// Poll the Brain metrics only while its tab is open (same gating as the discovery map).
let brainPoll = null;
function startBrainPoll() { stopBrainPoll(); quietly(loadBrainMetrics); brainPoll = setInterval(() => quietly(loadBrainMetrics), 4000); }
function stopBrainPoll() { if (brainPoll) { clearInterval(brainPoll); brainPoll = null; } }

document.querySelectorAll("[data-view]").forEach(button => button.addEventListener("click", () => {
  showView(button.dataset.view);
  stopDiscPoll(); walletStopPoll(); stopBrainPoll(); // only poll these while their tab is open
  // The Graph tab is now a file tree of your memory (Month ▸ Day ▸ Session ▸ Record),
  // built from the same /api/names path as Records.
  if (button.dataset.view === "graph") quietly(loadGraphTree);
  if (button.dataset.view === "graph") { const q = byId("search-q"); if (q) q.focus(); }
  // Network now hosts messaging too (the Messenger tab was merged in). Profile/
  // peers/requests and the private-network mesh load when their popups are opened.
  // Tab-switch loads run in the background (quietly) so the view appears instantly
  // and fills in — never a blocking spinner while the server responds.
  if (button.dataset.view === "network") { startDiscPoll(); }
  if (button.dataset.view === "canvas") quietly(cvLoadSites);
  if (button.dataset.view === "wallet") { quietly(walletInit); walletStartPoll(); }
  if (button.dataset.view === "updates") quietly(loadUpdateStatus);
  if (button.dataset.view === "brain") startBrainPoll();
}));
// graph-filter input handled by the delegated input listener.
