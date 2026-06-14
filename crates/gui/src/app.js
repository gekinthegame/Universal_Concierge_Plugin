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
function showView(name) {
  document.querySelectorAll(".view").forEach(item => item.classList.toggle("active", item.id === name + "-view"));
  document.querySelectorAll("[data-view]").forEach(item => item.classList.toggle("active", item.dataset.view === name));
}

async function loadMeta() {
  const meta = await getJson("/api/meta");
  csrfToken = meta.csrf_token || "";
  byId("model").textContent = meta.mounted_model;
  const option = node("option", "", meta.store);
  byId("store").replaceChildren(option);
}
async function loadNames() {
  const names = await getJson("/api/names");
  const container = byId("names"); clear(container);
  if (!names.length) { container.append(node("div", "empty", "No names are bound yet.")); return; }

  const months = new Map();
  names.forEach(binding => {
    const seconds = Number(binding.created_at || 0);
    let monthKey, dayKey, monthLabel, dayLabel, sort;
    if (seconds > 0) {
      const date = new Date(seconds * 1000);
      monthKey = date.getFullYear() + "-" + String(date.getMonth() + 1).padStart(2, "0");
      dayKey = monthKey + "-" + String(date.getDate()).padStart(2, "0");
      monthLabel = date.toLocaleDateString(undefined, { year: "numeric", month: "long" });
      dayLabel = date.toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" });
      sort = date.getTime();
    } else {
      monthKey = dayKey = "undated"; monthLabel = "Undated"; dayLabel = "No timestamp"; sort = -1;
    }
    if (!months.has(monthKey)) months.set(monthKey, { label: monthLabel, sort, count: 0, days: new Map() });
    const month = months.get(monthKey); month.count += 1; month.sort = Math.max(month.sort, sort);
    if (!month.days.has(dayKey)) month.days.set(dayKey, { label: dayLabel, sort, entries: [] });
    const day = month.days.get(dayKey); day.sort = Math.max(day.sort, sort);
    day.entries.push({ ...binding, sort });
  });

  const sortedMonths = [...months.entries()].sort((a, b) => b[1].sort - a[1].sort);
  // On the very first render, open the newest month and its newest day. After
  // that, expansion is driven by state.namesOpen (the user's choices), so the 5s
  // background refresh re-renders without popping collapsed sections back open.
  if (!state.namesOpenInit && sortedMonths.length) {
    const [topMonthKey, topMonth] = sortedMonths[0];
    state.namesOpen.add(topMonthKey);
    const topDay = [...topMonth.days.entries()].sort((a, b) => b[1].sort - a[1].sort)[0];
    if (topDay) state.namesOpen.add(topDay[0]);
    state.namesOpenInit = true;
  }

  sortedMonths.forEach(([monthKey, month]) => {
    const monthOpen = state.namesOpen.has(monthKey);
    const head = node("button", "month-head" + (monthOpen ? " open" : ""));
    head.append(
      node("span", "disclosure", monthOpen ? "▾" : "▸"),
      node("span", "month-label", month.label),
      node("span", "count", month.count + (month.count === 1 ? " record" : " records")),
    );
    const body = node("div", "month-body"); body.style.display = monthOpen ? "flex" : "none";
    head.addEventListener("click", () => toggleSection(head, body, monthKey));

    [...month.days.entries()].sort((a, b) => b[1].sort - a[1].sort).forEach(([dayKey, day]) => {
      const dayOpen = state.namesOpen.has(dayKey);
      const dayHead = node("button", "day-head" + (dayOpen ? " open" : ""));
      dayHead.append(
        node("span", "disclosure", dayOpen ? "▾" : "▸"),
        node("span", "day-label", day.label),
        node("span", "count", day.entries.length + (day.entries.length === 1 ? " record" : " records")),
      );
      const dayBody = node("div", "day-body"); dayBody.style.display = dayOpen ? "flex" : "none";
      dayHead.addEventListener("click", () => toggleSection(dayHead, dayBody, dayKey));
      day.entries.sort((a, b) => a.sort - b.sort).forEach(entry => dayBody.append(eventRow(entry)));
      body.append(appendChildren(node("div", "day-group"), dayHead, dayBody));
    });
    container.append(appendChildren(node("div", "month-group"), head, body));
  });
}
function appendChildren(parent, ...children) { children.forEach(child => parent.append(child)); return parent; }
function toggleSection(head, body, key) {
  const open = body.style.display !== "none";
  body.style.display = open ? "none" : "flex";
  head.classList.toggle("open", !open);
  const caret = head.querySelector(".disclosure");
  if (caret) caret.textContent = open ? "▸" : "▾";
  // Remember the choice so the periodic re-render preserves it.
  if (key) { if (open) state.namesOpen.delete(key); else state.namesOpen.add(key); }
}
function eventRow(entry) {
  const row = node("button", "event-row" + (entry.locked ? " locked" : ""));
  const seconds = Number(entry.created_at || 0);
  const time = seconds > 0
    ? new Date(seconds * 1000).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" })
    : "--:--:--";
  const desc = entry.locked ? "🔒 Locked record" : (entry.preview || "(no description)");
  const aliases = entry.names && entry.names.length ? entry.names : [entry.name];
  const descSpan = node("span", "event-desc", desc);
  if (aliases.length > 1) descSpan.append(node("span", "event-aliases", " ×" + aliases.length));
  row.append(
    node("span", "event-time", time),
    node("span", "event-kind", entry.kind || "node"),
    descSpan,
    node("span", "event-cid", shortCid(entry.cid)),
  );
  row.title = aliases.join("\n") + "\n" + entry.cid;
  row.addEventListener("click", () => { selectRoot(entry.cid); loadRecord(entry.cid, true); });
  return row;
}

async function selectRoot(cid) {
  state.root = cid;
  state.rootMode = "fixed";
  setSelected(cid);
  state.fullGraph = null;
  state.visibleCids.clear();
  await Promise.all([loadGraph(), loadStats(), refreshPrivacy()]);
}

function isSynthetic(cid) {
  return cid.startsWith("store:") || cid.startsWith("session:") ||
         cid.startsWith("year:") || cid.startsWith("month:") || cid.startsWith("day:");
}

// Every cid reachable downward from `cid` in the loaded graph.
function descendantsOf(cid) {
  const out = new Set();
  const stack = [cid];
  while (stack.length) {
    const current = stack.pop();
    for (const e of state.fullGraph.edges) {
      if (e.from === current && !out.has(e.to)) { out.add(e.to); stack.push(e.to); }
    }
  }
  return out;
}

// Toggle a node: collapse it if its children are already shown, otherwise
// expand (lazy-fetching a record's subgraph the first time it is opened).
async function toggleNode(cid) {
  setSelected(cid);
  if (!state.fullGraph) return;

  const children = state.fullGraph.edges.filter(e => e.from === cid).map(e => e.to);
  const expanded = children.length > 0 && children.every(child => state.visibleCids.has(child));
  if (expanded) {
    for (const cidToHide of descendantsOf(cid)) state.visibleCids.delete(cidToHide);
    drawGraph(state.fullGraph);
    safely(refreshPrivacy);
    return;
  }

  // Synthetic forest nodes (store/session) and any node whose children are
  // already loaded just reveal their children locally — no record to fetch.
  const childrenLoaded = children.length > 0;
  if (!isSynthetic(cid) && !childrenLoaded) {
    const g = await getJson("/api/graph?cid=" + encodeURIComponent(cid));
    // The store-wide graph can be reset (tab switch, ingest, re-root) while this
    // fetch is in flight; bail rather than dereference a now-null fullGraph.
    if (!state.fullGraph) return;
    const seen = new Set(state.fullGraph.nodes.map(n => n.cid));
    for (const n of g.nodes) {
      if (!seen.has(n.cid)) {
        state.fullGraph.nodes.push(n);
        seen.add(n.cid);
      }
    }
    const seenEdges = new Set(state.fullGraph.edges.map(e => e.from + "|" + e.to));
    for (const e of g.edges) {
      if (!seenEdges.has(e.from + "|" + e.to)) {
        state.fullGraph.edges.push(e);
        seenEdges.add(e.from + "|" + e.to);
      }
    }
  }

  // Reveal the clicked node and its immediate children.
  state.visibleCids.add(cid);
  for (const e of state.fullGraph.edges) {
    if (e.from === cid) state.visibleCids.add(e.to);
  }

  drawGraph(state.fullGraph);
  safely(refreshPrivacy);
}

async function loadGraph() {
  const query = state.rootMode === "fixed" && state.root ? "?cid=" + encodeURIComponent(state.root) : "";
  const graph = await getJson("/api/graph" + query);
  state.root = graph.root;
  byId("root-note").textContent = graph.forest
    ? " / whole store" + (graph.truncated ? " / first " + graph.total + " records" : "")
    : " / " + shortCid(graph.root) + (graph.truncated ? " / first 96 nodes" : "");

  if (!state.fullGraph) {
    state.fullGraph = graph;
    state.viewInitialized = false; // a freshly loaded graph re-centers on first draw
    state.visibleCids.clear();
    state.visibleCids.add(state.root);
    // Initially show the root and its immediate children
    for (const e of graph.edges) {
      if (e.from === state.root) {
        state.visibleCids.add(e.to);
      }
    }
  }
  drawGraph(state.fullGraph);
}

// Re-fetch the graph and copy lock/preview state onto the already-loaded nodes,
// so a lock/unlock is reflected immediately without collapsing expansions.
// A whole-store graph fetch can take many seconds; guard against overlapping
// runs (the periodic poll) and against fullGraph being reset mid-fetch.
let graphRefreshInFlight = false;
async function refreshGraphData() {
  if (!state.fullGraph) { await loadGraph(); return; }
  if (graphRefreshInFlight) return;
  graphRefreshInFlight = true;
  try {
    const query = state.rootMode === "fixed" && state.root ? "?cid=" + encodeURIComponent(state.root) : "";
    const fresh = await getJson("/api/graph" + query);
    if (!state.fullGraph) return; // reset (tab switch / ingest / re-root) while awaiting
    const freshById = new Map(fresh.nodes.map(n => [n.cid, n]));
    for (const n of state.fullGraph.nodes) {
      const f = freshById.get(n.cid);
      if (!f) continue;
      n.fenced = f.fenced; n.cleared = f.cleared;
      n.known_public = f.known_public; n.quarantined = f.quarantined;
      n.encrypted_private = f.encrypted_private; n.kind = f.kind; n.preview = f.preview;
    }
    drawGraph(state.fullGraph);
  } finally {
    graphRefreshInFlight = false;
  }
}

function drawGraph(graph) {
  const svg = byId("graph"); clear(svg);
  if (!graph.nodes.length) { svg.append(svgNode("text", { x: 40, y: 50, class: "graph-preview" }, "No reachable nodes.")); return; }

  const container = svgNode("g", { class: "graph-content" });
  svg.append(container);

  // Filter to visible nodes and edges
  const visibleNodes = graph.nodes.filter(n => state.visibleCids.has(n.cid));
  const visibleEdges = graph.edges.filter(e => state.visibleCids.has(e.from) && state.visibleCids.has(e.to));

  const incoming = new Map();
  visibleNodes.forEach(n => incoming.set(n.cid, 0));
  visibleEdges.forEach(e => {
    if (incoming.has(e.to)) incoming.set(e.to, incoming.get(e.to) + 1);
  });

  const depths = new Map();
  visibleNodes.forEach(n => depths.set(n.cid, 0));

  let changed = true;
  let iters = 0;
  while(changed && iters < 1000) {
    changed = false;
    iters++;
    for (const e of visibleEdges) {
      if (depths.has(e.from) && depths.has(e.to)) {
        if (depths.get(e.to) <= depths.get(e.from)) {
          depths.set(e.to, depths.get(e.from) + 1);
          changed = true;
        }
      }
    }
  }

  const byDepth = new Map();
  visibleNodes.forEach(n => {
    const d = depths.get(n.cid);
    if (!byDepth.has(d)) byDepth.set(d, []);
    byDepth.get(d).push(n);
  });

  const positions = new Map();
  // Which way a node's subtree fans out: -1 = left of the platter, +1 = right.
  // Labels follow the same direction so left-side nodes read leftward.
  const nodeDir = new Map();
  let minX = 0, minY = 0, maxX = 1000, maxY = 800;
  const centerX = 800, centerY = 600; // Move center a bit to give room for horizontal expansion

  const rootNode = visibleNodes.find(n => n.kind === "store") || visibleNodes.find(n => incoming.get(n.cid) === 0);
  if (rootNode) { positions.set(rootNode.cid, { x: centerX, y: centerY }); nodeDir.set(rootNode.cid, 1); }

  const sessions = byDepth.get(1) || [];
  const sessionSubtrees = new Map(); // sessionCid -> Array of nodes in its horizontal breakdown

  // Assign each node at depth > 1 to a session
  visibleNodes.forEach(n => {
    const d = depths.get(n.cid);
    if (d <= 1) return;
    // Walk back to find session ancestor
    let current = n.cid;
    while (true) {
        const parentEdge = visibleEdges.find(e => e.to === current);
        if (!parentEdge) break;
        if (depths.get(parentEdge.from) === 1) {
            const sCid = parentEdge.from;
            if (!sessionSubtrees.has(sCid)) sessionSubtrees.set(sCid, []);
            sessionSubtrees.get(sCid).push(n);
            break;
        }
        current = parentEdge.from;
    }
  });

  // Position sessions in a circle around the root
  sessions.forEach((s, idx) => {
    const angle = (idx / sessions.length) * 2 * Math.PI;
    const radius = 300;
    const sx = centerX + radius * Math.cos(angle);
    const sy = centerY + radius * Math.sin(angle);
    positions.set(s.cid, { x: sx, y: sy });
    // Label side: left half of the platter reads leftward, right half rightward.
    const dir = Math.cos(angle) >= 0 ? 1 : -1;
    nodeDir.set(s.cid, dir);

    // The subtree breaks out along the SAME radial ray as the session, so it
    // keeps fanning outward from the platter (instead of only left/right).
    // `outward` is the unit ray center→session; `perp` spreads siblings apart
    // across that ray.
    const outX = Math.cos(angle), outY = Math.sin(angle);
    const perpX = -outY, perpY = outX;
    const DEPTH_STEP = 350;  // distance between successive depth rings
    const SIBLING_GAP = 76;  // spread between siblings at the same depth

    const subtree = sessionSubtrees.get(s.cid) || [];
    const subtreeByDepth = new Map();
    subtree.forEach(n => {
        const d = depths.get(n.cid);
        if (!subtreeByDepth.has(d)) subtreeByDepth.set(d, []);
        subtreeByDepth.get(d).push(n);
    });

    for (const [d, arr] of subtreeByDepth.entries()) {
        const ring = (d - 1) * DEPTH_STEP;  // how far out along the ray
        arr.forEach((n, i) => {
            const spread = (i - (arr.length - 1) / 2) * SIBLING_GAP;
            const px = sx + outX * ring + perpX * spread;
            const py = sy + outY * ring + perpY * spread;
            positions.set(n.cid, { x: px, y: py });
            nodeDir.set(n.cid, dir);
            if (px > maxX) maxX = px; if (py > maxY) maxY = py;
            if (px < minX) minX = px; if (py < minY) minY = py;
        });
    }
  });

  // Capture the real extent of every placed node (root, sessions, subtrees) so
  // Reset View can fit-and-center the whole graph. Pad right for the node labels,
  // which extend ~250px past each dot.
  let bx0 = Infinity, by0 = Infinity, bx1 = -Infinity, by1 = -Infinity;
  positions.forEach(p => {
    if (p.x < bx0) bx0 = p.x; if (p.y < by0) by0 = p.y;
    if (p.x > bx1) bx1 = p.x; if (p.y > by1) by1 = p.y;
  });
  if (!isFinite(bx0)) { bx0 = 0; by0 = 0; bx1 = 1000; by1 = 650; }
  // Pad for labels: they extend ~280px left (left-side nodes) or ~300px right.
  state.graphBounds = { x: bx0 - 280, y: by0 - 40, w: (bx1 - bx0) + 580, h: (by1 - by0) + 80 };

  // The SVG fills the viewport (no native scroll); all pan/zoom is done through
  // the content group's transform so Reset View can place it deterministically.
  const view = byId("graph-view");
  const vw = view.clientWidth || 1000, vh = view.clientHeight || 650;
  svg.style.width = "100%";
  svg.style.height = "100%";
  svg.setAttribute("viewBox", "0 0 " + vw + " " + vh);

  // Nodes are small LED buttons
  function radiusOf(item) { return item.kind === "store" ? 32 : 8; }
  function geom(item, pos) {
    const r = radiusOf(item);
    const cx = pos.x, cy = pos.y;
    // Edge connections return to center-based for radial lines,
    // but subtrees might benefit from horizontal logic.
    // For simplicity, we connect centers.
    return { r, cx, cy, center: { x: cx, y: cy } };
  }
  const nodeById = new Map(visibleNodes.map(n => [n.cid, n]));
  const allById = new Map(graph.nodes.map(n => [n.cid, n]));
  // A session reflects its records: fully locked if every record is locked,
  // partially locked if some are.
  // Decision 0026: everything is fenced from egress by default, so we surface the
  // *exceptions* — roots cleared for export. "cleared" = this record can leave the
  // device; "partial" = a tier whose descendants are only partly cleared.
  function egressStateOf(item) {
    if (item.cleared) return "cleared";
    // A calendar tier (or legacy session) reflects its records: fully cleared if
    // every descendant record is cleared, partial if only some are.
    if (["session", "year", "month", "day"].includes(item.kind)) {
      const kids = graph.edges.filter(e => e.from === item.cid).map(e => allById.get(e.to)).filter(Boolean);
      if (kids.length && kids.every(n => n.cleared || egressStateOf(n) === "cleared")) return "cleared";
      if (kids.some(n => n.cleared || ["partial","cleared"].includes(egressStateOf(n)))) return "partial";
    }
    return "";
  }

  visibleEdges.forEach(edge => {
    const fp = positions.get(edge.from), tp = positions.get(edge.to);
    if (!fp || !tp) return;
    const a = geom(nodeById.get(edge.from), fp).center;
    const b = geom(nodeById.get(edge.to), tp).center;
    container.append(svgNode("line", { x1: a.x, y1: a.y, x2: b.x, y2: b.y, class: "graph-edge" }));
  });
  visibleNodes.forEach(item => {
    const pos = positions.get(item.cid);
    const g = geom(item, pos);
    const childEdges = graph.edges.filter(e => e.from === item.cid);
    const unloadedExpandable = item.expandable && childEdges.length === 0;
    const hasChildren = childEdges.length > 0 || unloadedExpandable;
    const hasHiddenChildren = unloadedExpandable || childEdges.some(e => !state.visibleCids.has(e.to));

    const egressState = egressStateOf(item);
    const isStore = item.kind === "store";
    const classes = ["graph-node", "led-node"];
    if (item.cid === state.selected) classes.push("active");
    if (egressState === "cleared") classes.push("cleared-root");
    else if (egressState === "partial") classes.push("partial-cleared");
    if (item.known_public) classes.push("known-public");
    if (item.quarantined) classes.push("quarantined");
    if (item.encrypted_private) classes.push("encrypted-private");
    const group = svgNode("g", { class: classes.join(" "), tabindex: "0", role: "button" });

    // Labels sit on the same side the node fans out toward, so left-side
    // subtrees read leftward instead of overlapping the platter.
    const leftSide = (nodeDir.get(item.cid) || 1) < 0;
    const labelX = leftSide ? g.cx - 15 : g.cx + 15;
    const labelAnchor = leftSide ? "end" : "start";

    if (item.kind === "store") {
      group.append(brainGlyph(g.cx, g.cy, 64, "var(--patina)"));
    } else {
      group.append(svgNode("circle", { cx: g.cx, cy: g.cy, r: g.r, class: "graph-dot led" }));
    }

    if (item.kind === "store") {
      // CENTER: THE DATA PLATTER (brain icon replaces text)
    } else if (item.kind === "session") {
      const when = item.started_at ? new Date(item.started_at * 1000) : null;
      const date = when
        ? when.toLocaleDateString(undefined, { month: "short", day: "numeric" }) + " " +
          when.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
        : "undated";
      group.append(
        svgNode("text", { x: labelX, y: g.cy - 7, "text-anchor": labelAnchor, class: "graph-kind" }, date),
        svgNode("text", { x: labelX, y: g.cy + 9, "text-anchor": labelAnchor, class: "graph-preview" }, (item.description || item.preview || "session").slice(0, 34)),
        svgNode("text", { x: labelX, y: g.cy + 24, "text-anchor": labelAnchor, class: "graph-cid" }, (item.count || 0) + " records")
      );
    } else if (item.kind === "year" || item.kind === "month" || item.kind === "day") {
      // Calendar tier ring: store → year → month → day → event.
      const sub = item.kind === "day" ? ((item.count || 0) + " records") : "";
      group.append(
        svgNode("text", { x: labelX, y: g.cy - 5, "text-anchor": labelAnchor, class: "graph-kind" }, item.kind),
        svgNode("text", { x: labelX, y: g.cy + 11, "text-anchor": labelAnchor, class: "graph-preview" }, item.preview || ""),
        svgNode("text", { x: labelX, y: g.cy + 25, "text-anchor": labelAnchor, class: "graph-cid" }, sub)
      );
    } else {
      group.append(
        svgNode("text", { x: labelX, y: g.cy - 12, "text-anchor": labelAnchor, class: "graph-kind" }, item.kind),
        svgNode("text", { x: labelX, y: g.cy + 4, "text-anchor": labelAnchor, class: "graph-preview" }, (item.preview || "").slice(0, 30)),
        svgNode("text", { x: labelX, y: g.cy + 18, "text-anchor": labelAnchor, class: "graph-cid" }, shortCid(item.cid)),
        svgNode("text", { x: labelX, y: g.cy + 30, "text-anchor": labelAnchor, class: "graph-cid" }, fmtWhen(item.created_at))
      );
    }

    // Egress badge + expand/collapse affordance. Fenced is the default (no badge);
    // we mark the *exceptions* — subgraphs cleared for export / already known-public.
    // Leaf exceptions show the badge centered; parents keep +/− and get a corner badge.
    const exposed = egressState === "cleared" || item.known_public;
    const badgeColor = exposed ? "var(--vermilion)" : "var(--gold)";
    const showBadge = !!egressState || !!item.known_public;
    if (hasChildren) {
      group.append(svgNode("text", { x: g.cx, y: g.cy + 5, "text-anchor": "middle", class: "graph-plus" }, hasHiddenChildren ? "+" : "−"));
      if (showBadge) group.append(lockGlyph(g.cx + g.r - 1, g.cy - g.r + 1, badgeColor));
    } else if (showBadge) {
      group.append(lockGlyph(g.cx, g.cy, badgeColor));
    }

    const choose = () => safely(async () => {
      await toggleNode(item.cid);
      if (!isSynthetic(item.cid)) await loadRecord(item.cid, true);
    });
    group.addEventListener("click", choose);
    group.addEventListener("keydown", event => { if (event.key === "Enter" || event.key === " ") choose(); });
    container.append(group);
  });
  // First draw of a graph fits + centers it; later redraws (expand/collapse,
  // lock refresh) keep the user's current pan/zoom.
  if (!state.viewInitialized) { fitView(); state.viewInitialized = true; }
  else applyZoom();
}

// Synapse current: when memory is recalled/accessed/used, glowing pulses flow along
// the graph edges *inward to the brain* at the center — the user sees memory in use.
// Appends to the same transformed group as the edges, so it tracks pan/zoom. Each
// pulse travels from the endpoint farther from the brain to the one nearer it.
function flashSynapses() {
  const svg = byId("graph");
  if (!svg) return;
  const container = svg.querySelector(".graph-content");
  if (!container) return;
  const cx = 800, cy = 600; // graph center (the brain) — matches drawGraph
  container.querySelectorAll(".graph-edge").forEach((edge, idx) => {
    const x1 = +edge.getAttribute("x1"), y1 = +edge.getAttribute("y1");
    const x2 = +edge.getAttribute("x2"), y2 = +edge.getAttribute("y2");
    const inner1 = (x1 - cx) ** 2 + (y1 - cy) ** 2 <= (x2 - cx) ** 2 + (y2 - cy) ** 2;
    const ox = inner1 ? x2 : x1, oy = inner1 ? y2 : y1; // outer (start)
    const ix = inner1 ? x1 : x2, iy = inner1 ? y1 : y2; // inner (toward brain)
    const dot = svgNode("circle", { cx: ox, cy: oy, r: 3, class: "synapse-dot" });
    container.appendChild(dot);
    const run = dot.animate(
      [
        { transform: "translate(0px,0px)", opacity: 0 },
        { opacity: 1, offset: 0.25 },
        { opacity: 1, offset: 0.8 },
        { transform: "translate(" + (ix - ox) + "px," + (iy - oy) + "px)", opacity: 0 },
      ],
      { duration: 900, delay: (idx % 8) * 70, easing: "ease-in" }
    );
    run.onfinish = () => dot.remove();
  });
}
// The record is a popup window, not a tab: it opens only when a record is
// explicitly selected (Records tab, graph node, search hit, or a link within it).
// Passive refreshes (after a lock/clear) update the content but never pop it open.
function openRecordModal() { byId("record-modal").style.display = "flex"; }
function closeRecordModal() { byId("record-modal").style.display = "none"; }
async function loadRecord(cid, open = false) {
  setSelected(cid);
  flashSynapses(); // accessing a record = memory used → fire the current
  logSystem("access " + shortCid(cid), "dim");
  safely(refreshPrivacy);
  const record = await getJson("/api/record?cid=" + encodeURIComponent(cid));
  const container = byId("record"); clear(container); container.className = "content";
  const top = node("div", "record-top");
  top.append(node("span", "cid", record.cid));
  if (record.live !== false) top.append(node("span", "kind", record.kind));
  container.append(top);
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
byId("record-close").addEventListener("click", closeRecordModal);
byId("record-modal").addEventListener("click", e => { if (e.target === byId("record-modal")) closeRecordModal(); });
document.addEventListener("keydown", e => { if (e.key === "Escape" && byId("record-modal").style.display === "flex") closeRecordModal(); });
document.addEventListener("keydown", e => { if (e.key === "Escape" && byId("deploy-modal").style.display === "flex") depClose(); });

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
  const c = byId("system-console"); if (!c) return;
  const atBottom = c.scrollTop + c.clientHeight >= c.scrollHeight - 6;
  const ln = node("div", "ln");
  ln.append(node("span", "t", new Date().toLocaleTimeString(undefined, { hour12: false }) + "  "));
  ln.append(node("span", cls || "dim", text));
  c.append(ln);
  while (c.childElementCount > 200) c.firstChild.remove();
  if (atBottom) c.scrollTop = c.scrollHeight;
}
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
  if (state.lastStats) {
    const db = stats.blocks - state.lastStats.blocks, dn = stats.names - state.lastStats.names, dt = stats.tombstones - state.lastStats.tombstones;
    if (db > 0) logSystem("ingested " + db + " block" + (db > 1 ? "s" : "") + "  ·  " + stats.blocks + " total", "ev");
    if (dn > 0) logSystem("bound " + dn + " name" + (dn > 1 ? "s" : ""), "ev");
    if (dt > 0) logSystem("tombstoned " + dt + " block" + (dt > 1 ? "s" : ""), "wn");
  }
  state.lastStats = stats;
  const container = byId("stats"); clear(container);
  // "Reclaimable" = superseded immutable versions (old calendar/HAMT/checkpoint
  // blocks) no live name can reach. Normal for an append-only content-addressed
  // store — not corruption; reclaim them with Compact. Tombstones are the receipts
  // Compact leaves behind for each reclaimed block.
  [["Reachable", stats.reachable, "Blocks reachable from a named root — your live graph."],
   ["Blocks", stats.blocks, "Total content-addressed blocks on disk."],
   ["Reclaimable", stats.orphans, "Superseded versions no name references (old index/HAMT/checkpoint blocks). Normal for an append-only store — reclaim with Compact."],
   ["Tombstones", stats.tombstones, "Deletion receipts left by Compact, one per reclaimed block."],
   ["Names", stats.names, "Bound names — every one is a graph root."],
   ["CAR size", formatBytes(stats.car_size), "Size of the selected root's exportable CAR."]].forEach(([label, value, tip]) => {
    const item = node("div", "stat");
    if (tip) item.title = tip;
    item.append(node("b", "", value), node("span", "", label)); container.append(item);
  });
  // Phase B: publishing is opt-in. Remember readiness so the publish review can
  // present "not set up yet" guidance instead of a raw error.
  state.publishingReady = !!stats.publishing_ready;
  const backends = byId("backends"); clear(backends);
  stats.backends.forEach(info => {
    const item = node("div", "backend" + (info.selected && !info.reachable ? " optin" : ""));
    item.append(node("b", "", info.name + (info.selected ? " / selected" : "")), node("span", "", info.pin_status), node("span", "", info.requirements));
    backends.append(item);
  });
  byId("car-note").textContent = stats.root ? shortCid(stats.root) + " / " + stats.car_blocks + " blocks / " + formatBytes(stats.car_size) : "No root selected";
}
async function exportCar() {
  if (!state.root) { notice("Select a named root or graph node first."); return; }
  const plan = await getJson("/api/egress-plan?cid=" + encodeURIComponent(state.root));
  const warning = "Plaintext CAR export review\n\n" +
    plan.block_count + " blocks / " + formatBytes(plan.byte_size) + "\n" +
    "Backend posture: " + plan.network_posture + "\n" +
    (plan.blocking_locks.length ? "BLOCKED: " + plan.blocking_locks.length + " lock(s)\n" : "") +
    "This creates a portable plaintext copy outside the store. Continue?";
  if (!window.confirm(warning)) return;
  notice("Browser plaintext download is intentionally disabled. Use concierge-plugin export-car with explicit review.");
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
        await Promise.all([refreshPrivacy(), loadStats(), refreshGraphData()]);
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
      await Promise.all([refreshGraphData(), loadStats(), refreshPrivacy()]);
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
        await Promise.all([refreshGraphData(), loadStats(), refreshPrivacy()]);
      }));
    } else {
      panel.append(passwordAction("Clear session for export (" + remaining + " record" + (remaining === 1 ? "" : "s") + ")", async password => {
        if (!window.confirm("Clear all " + remaining + " fenced record(s) in this session for export?")) return;
        const n = await eachRecord("/api/clear-for-egress", { password });
        childNodes.forEach(node => { node.cleared = true; });
        notice("Cleared " + n + " record(s) for export.");
        await Promise.all([refreshGraphData(), loadStats(), refreshPrivacy()]);
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
        await Promise.all([refreshPrivacy(), loadStats(), refreshGraphData()]);
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
      focus.addEventListener("click", () => safely(async () => { await selectRoot(hit.lock_root); await loadRecord(hit.lock_root); }));
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
      await Promise.all([refreshPrivacy(), loadStats(), refreshGraphData()]);
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
          await Promise.all([refreshPrivacy(), loadStats(), refreshGraphData()]);
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
        await Promise.all([refreshPrivacy(), loadStats(), refreshGraphData()]);
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
  const rooms = await getJson("/api/rooms"); const list = byId("room-list"); clear(list);
  rooms.forEach(room => { const option = node("option"); option.value = room; list.append(option); });
}
async function loadThread() {
  const room = byId("room").value.trim(); if (!room) return;
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
  if (!visible.length) { container.append(node("div", "empty", "No visible messages in this room.")); return; }
  visible.forEach(message => {
    const item = node("article", "message"); const head = node("div", "message-head");
    const label = message.nickname || shortCid(message.author);
    head.append(node("span", "", "[" + message.clock + "] " + label + " / " + (state.roles.get(message.author) || "unclassified")));
    const role = node("button", "", state.roles.get(message.author) === "ai" ? "mark human" : "mark AI");
    role.addEventListener("click", () => { state.roles.set(message.author, state.roles.get(message.author) === "ai" ? "human" : "ai"); loadThread(); });
    const mute = node("button", "", "mute participant");
    mute.addEventListener("click", () => { state.muted.add(message.author); loadThread(); });
    const reveal = node("button", "", "reveal CID");
    reveal.addEventListener("click", () => { reveal.textContent = message.cid; });
    head.append(role, mute, reveal);
    // Phase I — trust thermometer + structural importance + follow lens (all local).
    const tier = node("span", "trust-tier trust-" + (message.trust_tier || "unverified"), message.trust_label || "Unverified");
    tier.title = "Authentication tier this message crossed (honest auth-strength, not reputation)";
    head.append(tier);
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
        // Rebuild the whole-store forest so the new import/session appears.
        state.fullGraph = null; state.rootMode = "default"; state.root = ""; state.selected = "";
        state.visibleCids.clear();
        await Promise.all([loadGraph(), loadStats(), loadNames()]);
      } finally {
        go.disabled = false; go.textContent = "Ingest";
      }
    });
  });
  document.body.append(overlay);
  input.focus();
}

byId("reset-view").addEventListener("click", () => fitView());

// Pillar A: pull Brave/Opera bookmarks into memory (they become searchable records).
byId("bookmarks-sync").addEventListener("click", () => safely(async () => {
  const st = byId("bookmarks-status"); clear(st);
  st.appendChild(document.createTextNode("Syncing…"));
  const res = await postJson("/api/bookmarks/sync", {});
  clear(st);
  const msg = res.added > 0
    ? "Added " + res.added + " bookmark" + (res.added === 1 ? "" : "s") + " to memory."
    : "Up to date — no new bookmarks.";
  st.appendChild(document.createTextNode(msg));
  logSystem("synced browser bookmarks · +" + (res.added || 0), "ok");
  await Promise.all([loadNames(), loadStats()]);
}));

// Scale + translate the graph so its full extent is centered in the viewport.
function fitView() {
  const view = byId("graph-view");
  if (!view) return;
  const vw = view.clientWidth || 1000, vh = view.clientHeight || 650;
  const svg = byId("graph");
  if (svg) svg.setAttribute("viewBox", "0 0 " + vw + " " + vh); // keep units = px after a resize
  const b = state.graphBounds;
  if (!b || b.w <= 0 || b.h <= 0) { state.zoom = 1.0; state.pan = { x: 0, y: 0 }; applyZoom(); return; }
  const margin = 48;
  const zoom = Math.max(0.1, Math.min(2, Math.min((vw - margin) / b.w, (vh - margin) / b.h)));
  const cx = b.x + b.w / 2, cy = b.y + b.h / 2;
  state.zoom = zoom;
  state.pan = { x: vw / 2 - zoom * cx, y: vh / 2 - zoom * cy };
  applyZoom();
}

// Phase N · Phase H — the Private Network Map: identity hierarchy, networks, this
// device's scopes + their validity, epoch health, and who is revoked.
async function loadNetwork() {
  let data;
  try { data = await getJson("/api/network"); } catch (e) { return; }
  const idBar = byId("network-identity"); clear(idBar);
  idBar.append(node("span", "", "UserID (root): "), Object.assign(node("b", "", data.user_id ? shortCid(data.user_id) : "—"), { title: data.user_id || "" }));
  idBar.append(node("span", "", "This device: "), Object.assign(node("b", "", data.device_id ? shortCid(data.device_id) : "—"), { title: data.device_id || "" }));

  const map = byId("network-map"); clear(map);
  if (!data.networks.length) { map.append(node("div", "empty", "No private network yet. Create one above, then pair another computer from the CLI (network pair).")); return; }
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
byId("network-create").addEventListener("click", () => safely(async () => {
  const name = byId("network-name").value.trim(); if (!name) return;
  await postJson("/api/network/create", { name });
  byId("network-name").value = "";
  await loadNetwork();
}));

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
function renderDiscovery(data) {
  const svg = byId("discovery-svg"); if (!svg) return; clear(svg);
  const W = 600, H = 360, cx = W / 2, cy = H / 2;
  const peers = (data && data.peers) || [];
  const rings = peers.length > 10 ? [112, 168] : [128];
  function pos(i, n) {
    let ring = 0, idx = i, count = n;
    if (rings.length > 1) {
      const inner = Math.min(10, n);
      if (i < inner) { ring = 0; idx = i; count = inner; }
      else { ring = 1; idx = i - inner; count = n - inner; }
    }
    const r = rings[ring];
    const a = (idx / Math.max(1, count)) * Math.PI * 2 - Math.PI / 2 + ring * 0.4;
    return { x: cx + Math.cos(a) * r, y: cy + Math.sin(a) * r };
  }
  // Edges first (under the nodes): solid for live connections, dashed for discovered.
  peers.forEach((p, i) => {
    const q = pos(i, peers.length);
    svg.append(svgNode("line", { x1: cx, y1: cy, x2: q.x, y2: q.y, class: "disc-edge " + (p.status === "connected" ? "on" : "off") }));
  });
  // Peer brains.
  peers.forEach((p, i) => {
    const q = pos(i, peers.length);
    const connected = p.status === "connected";
    const g = discBrain(q.x, q.y, connected ? 28 : 22, connected ? "connected" : "discovered");
    const label = svgNode("text", { x: q.x, y: q.y + (connected ? 27 : 22), class: "disc-label", "text-anchor": "middle" });
    label.textContent = (p.peer_id || "").slice(-6);
    g.append(label);
    const title = svgNode("title", {});
    title.textContent = (p.peer_id || "") + " · " + p.status + " · via " + p.source + (p.relayed ? " · relayed" : "");
    g.append(title);
    svg.append(g);
  });
  // This node at the centre, on top.
  const online = !!(data && data.self && data.self.online);
  const me = discBrain(cx, cy, 48, "disc-self");
  const slabel = svgNode("text", { x: cx, y: cy + 41, class: "disc-label self", "text-anchor": "middle" });
  slabel.textContent = online ? "your node" : "your node (offline)";
  me.append(slabel);
  const stitle = svgNode("title", {});
  stitle.textContent = "This node · " + ((data && data.self && data.self.peer_id) || "");
  me.append(stitle);
  svg.append(me);
  // Caption.
  const stat = byId("discovery-stat"); clear(stat);
  const total = (data && data.total) || peers.length;
  if (!online) stat.textContent = "Node offline — open this tab to bring it online and start discovering peers.";
  else if (!peers.length) stat.textContent = "Searching the network… no peers yet. mDNS finds LAN peers instantly; the DHT and rendezvous take a moment.";
  else stat.textContent = total + " node" + (total === 1 ? "" : "s") + " discovered · " + ((data && data.connected) || 0) + " connected" + (total > peers.length ? " · showing " + peers.length : "");
}
async function loadPeers() {
  let data; try { data = await getJson("/api/peers"); } catch (e) { return; }
  renderDiscovery(data);
}
function startDiscPoll() { stopDiscPoll(); safely(loadPeers); discPoll = setInterval(() => quietly(loadPeers), 4000); }
function stopDiscPoll() { if (discPoll) { clearInterval(discPoll); discPoll = null; } }

document.querySelectorAll("[data-view]").forEach(button => button.addEventListener("click", () => {
  // Clicking the Graph tab while focused on one root returns to the whole-store forest.
  if (button.dataset.view === "graph" && state.rootMode === "fixed") {
    state.rootMode = "default"; state.root = ""; state.selected = "";
    state.fullGraph = null; state.visibleCids.clear();
    safely(async () => { await Promise.all([loadGraph(), loadStats(), refreshPrivacy()]); });
  }
  showView(button.dataset.view);
  stopDiscPoll(); walletStopPoll(); // only poll these while their tab is open
  if (button.dataset.view === "search") byId("search-q").focus();
  if (button.dataset.view === "network") { safely(loadNetwork); startDiscPoll(); }
  if (button.dataset.view === "canvas") safely(cvLoadSites);
  if (button.dataset.view === "messenger") { safely(loadProfile); safely(loadContacts); }
  if (button.dataset.view === "wallet") { safely(walletInit); walletStartPoll(); }
}));

