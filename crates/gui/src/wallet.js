// ── Concierge Wallet (Pillar C): the UX is ours; the browser's wallet does custody. ──
const WALLET = { provider: null, account: null, agentId: "" };
const WALLET_CHAINS = { "0x1": "Ethereum", "0x2105": "Base", "0x89": "Polygon", "0xa": "Optimism", "0xa4b1": "Arbitrum", "0xaa36a7": "Sepolia" };
function walletShort(a) { return a ? a.slice(0, 6) + "…" + a.slice(-4) : "—"; }
function walletChainName(cid) { return WALLET_CHAINS[cid] || ("Chain " + cid); }
function walletEth(weiHex) { try { return (Number(BigInt(weiHex)) / 1e18).toFixed(4); } catch (e) { return "?"; } }
// Find the wallet provider robustly: Brave/Opera inject `window.ethereum`
// asynchronously and also announce via EIP-6963, so we listen + poll briefly.
async function walletDetectProvider() {
  let found = null;
  const onAnnounce = e => { if (!found && e.detail && e.detail.provider) found = e.detail.provider; };
  window.addEventListener("eip6963:announceProvider", onAnnounce);
  window.dispatchEvent(new Event("eip6963:requestProvider"));
  for (let i = 0; i < 12 && !found; i++) {
    if (window.ethereum) { found = window.ethereum; break; }
    await new Promise(r => setTimeout(r, 150));
  }
  window.removeEventListener("eip6963:announceProvider", onAnnounce);
  return found || window.ethereum || null;
}
async function walletShowUnavailable() {
  let inBrave = false;
  try { inBrave = !!(navigator.brave && await navigator.brave.isBrave()); } catch (e) {}
  const msg = byId("wallet-unavailable-msg"); clear(msg);
  if (inBrave) {
    msg.append(document.createTextNode("No active wallet yet. Click “Create my wallet” to open Brave Wallet setup — create or import a wallet, then come back and press Refresh."));
    byId("wallet-create").style.display = "";
  } else {
    msg.append(document.createTextNode("No in-browser wallet detected. Open the Concierge in Brave or Opera (both have a built-in wallet) to use this tab."));
    byId("wallet-create").style.display = "none";
  }
}
async function walletInit() {
  byId("wallet-status").textContent = "Looking for your wallet…";
  WALLET.provider = await walletDetectProvider();
  const has = !!WALLET.provider;
  byId("wallet-unavailable").style.display = has ? "none" : "";
  byId("wallet-main").style.display = has ? "" : "none";
  if (!has) { await walletShowUnavailable(); return; }
  try { const me = await getJson("/api/me"); WALLET.agentId = me.username || ""; } catch (e) {}
  await walletLoadState();
  try {
    const accts = await WALLET.provider.request({ method: "eth_accounts" });
    if (accts && accts[0]) { WALLET.account = accts[0]; await walletRefresh(); }
    else { byId("wallet-status").textContent = "No wallet connected — click “Create or connect wallet”."; }
  } catch (e) { byId("wallet-status").textContent = "Click “Create or connect wallet”."; }
}
async function walletConnect() {
  if (!WALLET.provider) { await walletInit(); if (!WALLET.provider) return; }
  // eth_requestAccounts opens Brave/Opera's onboarding if no wallet exists yet.
  const accts = await WALLET.provider.request({ method: "eth_requestAccounts" });
  WALLET.account = accts && accts[0];
  await walletRefresh();
  notice(WALLET.account ? "Wallet connected." : "Wallet setup started — finish it in your browser, then Refresh.");
}
// Open a browser-wallet page (onboarding or settings) as a small app window via
// the backend (web pages can't navigate to brave://). target ∈ "wallet"|"settings".
async function walletOpen(target) {
  const res = await postJson("/api/wallet/setup", { target: target });
  if (res.ok) {
    notice(target === "settings" ? "Opening your wallet settings…" : "Opening your wallet… set it up, then click Refresh.");
  } else {
    notice(res.error || "Open your browser's built-in wallet, then Refresh.");
  }
}
function walletCreate() { return walletOpen("wallet"); }
// Pop Brave/Opera's own wallet panel (the small window shown for permissions) — the
// full portfolio. wallet_requestPermissions re-prompts it even when already connected.
async function walletPanel() {
  if (!WALLET.provider) { await walletInit(); if (!WALLET.provider) { notice("No wallet detected — open the Concierge in Brave or Opera."); return; } }
  try {
    await WALLET.provider.request({ method: "wallet_requestPermissions", params: [{ eth_accounts: {} }] });
  } catch (e) { /* user closed the panel — fine */ }
  await walletRefresh();
}
async function walletRefresh() {
  if (!WALLET.provider || !WALLET.account) return;
  byId("wallet-status").textContent = "Connected";
  byId("wallet-addr").textContent = walletShort(WALLET.account);
  try { byId("wallet-net").textContent = walletChainName(await WALLET.provider.request({ method: "eth_chainId" })); } catch (e) {}
  try {
    const wei = await WALLET.provider.request({ method: "eth_getBalance", params: [WALLET.account, "latest"] });
    byId("wallet-bal").textContent = walletEth(wei) + " ETH";
  } catch (e) {}
}
async function walletLoadState() {
  let st; try { st = await getJson("/api/wallet"); } catch (e) { st = { links: [], settings: {} }; }
  const wrap = byId("wallet-links"); clear(wrap);
  (st.links || []).forEach(l => {
    const row = node("div", "wallet-link-row");
    row.append(node("span", "mono", walletShort(l.address)));
    row.append(node("span", "eyebrow", " · " + (l.chain || "evm")));
    const rm = node("button", "tool-button", "Unlink");
    rm.addEventListener("click", () => safely(async () => {
      await postJson("/api/wallet/unlink", { address: l.address });
      await walletLoadState();
    }));
    row.append(rm);
    wrap.append(row);
  });
  if (!(st.links || []).length) wrap.append(node("div", "eyebrow", "No wallet linked yet."));
  const s = st.settings || {};
  byId("wallet-agent").checked = !!s.agent_access;
  byId("wallet-cap").value = s.spend_cap || "";
  byId("wallet-allow").value = (s.allowlist || []).join("\n");
  if (s.preferred_chain) byId("wallet-chain").value = s.preferred_chain;
}
async function walletLink() {
  if (!WALLET.provider || !WALLET.account) { notice("Connect your wallet first."); return; }
  if (!WALLET.agentId) { notice("Your AgentID isn't ready yet."); return; }
  const msg = "Link this wallet to my Concierge identity (AgentID): " + WALLET.agentId;
  const sig = await WALLET.provider.request({ method: "personal_sign", params: [msg, WALLET.account] });
  await postJson("/api/wallet/link", { address: WALLET.account, chain: "evm", signature: sig });
  notice("Linked " + walletShort(WALLET.account) + " to your identity.");
  logSystem("wallet · linked " + walletShort(WALLET.account), "ok");
  await walletLoadState();
}
async function walletSwitchChain(cid) {
  if (!WALLET.provider || !cid) return;
  try { await WALLET.provider.request({ method: "wallet_switchEthereumChain", params: [{ chainId: cid }] }); await walletRefresh(); }
  catch (e) { notice("Couldn't switch network — add it in your wallet first."); }
}
async function walletSaveSettings() {
  const settings = {
    agent_access: byId("wallet-agent").checked,
    spend_cap: byId("wallet-cap").value.trim(),
    allowlist: byId("wallet-allow").value.split("\n").map(s => s.trim()).filter(Boolean),
    preferred_chain: byId("wallet-chain").value,
  };
  await postJson("/api/wallet/settings", settings);
  if (settings.preferred_chain) await walletSwitchChain(settings.preferred_chain);
  notice("Wallet settings saved.");
}
// AI transaction proposals: the AI can only *propose*; the user approves here, then
// the browser wallet confirms again. Polled while the Wallet tab is open.
let walletPoll = null;
function walletEthToWeiHex(ethStr) {
  const n = parseFloat(ethStr); if (!isFinite(n) || n <= 0) return "0x0";
  return "0x" + BigInt(Math.round(n * 1e18)).toString(16);
}
async function walletLoadProposals() {
  let list; try { list = await getJson("/api/wallet/proposals"); } catch (e) { return; }
  const wrap = byId("wallet-proposals"); clear(wrap);
  byId("wallet-proposals-wrap").style.display = (list && list.length) ? "" : "none";
  (list || []).forEach(p => {
    const card = node("div", "wallet-card");
    card.append(node("div", "wallet-v", "Send " + p.value + " ETH"));
    card.append(Object.assign(node("div", "wallet-k mono", "to " + p.to), { title: p.to }));
    if (p.reason) card.append(node("div", "eyebrow", "“" + p.reason + "”"));
    const row = node("div", "modal-actions");
    const ok = node("button", "tool-button", "Approve & send");
    ok.addEventListener("click", () => safely(() => walletApprove(p)));
    const no = node("button", "tool-button", "Reject");
    no.addEventListener("click", () => safely(async () => {
      await postJson("/api/wallet/proposals/resolve", { id: p.id, status: "rejected" });
      await walletLoadProposals();
    }));
    row.append(ok, no); card.append(row);
    wrap.append(card);
  });
}
async function walletApprove(p) {
  if (!WALLET.provider) { notice("Connect your wallet first."); return; }
  if (!WALLET.account) { await walletConnect(); if (!WALLET.account) return; }
  const tx = { from: WALLET.account, to: p.to, value: walletEthToWeiHex(p.value) };
  if (p.data) tx.data = p.data;
  // Brave/Opera shows its own confirm dialog here — the final human gate.
  const hash = await WALLET.provider.request({ method: "eth_sendTransaction", params: [tx] });
  await postJson("/api/wallet/proposals/resolve", { id: p.id, status: "approved", tx_hash: hash || "" });
  notice("Sent. " + (hash ? hash.slice(0, 12) + "…" : ""));
  logSystem("wallet · approved AI tx " + p.id, "ok");
  await walletLoadProposals();
}
function walletStartPoll() { walletStopPoll(); safely(walletLoadProposals); walletPoll = setInterval(() => quietly(walletLoadProposals), 5000); }
function walletStopPoll() { if (walletPoll) { clearInterval(walletPoll); walletPoll = null; } }

byId("wallet-connect").addEventListener("click", () => safely(walletConnect));
byId("wallet-refresh").addEventListener("click", () => safely(walletInit));
byId("wallet-create").addEventListener("click", () => safely(walletCreate));
byId("wallet-recheck").addEventListener("click", () => safely(walletInit));
byId("wallet-panel").addEventListener("click", () => safely(walletPanel));
byId("wallet-link").addEventListener("click", () => safely(walletLink));
byId("wallet-save").addEventListener("click", () => safely(walletSaveSettings));
byId("wallet-chain").addEventListener("change", () => safely(() => walletSwitchChain(byId("wallet-chain").value)));
byId("search-form").addEventListener("submit", event => { event.preventDefault(); safely(runSearch); });
document.querySelectorAll("[data-command]").forEach(button => button.addEventListener("click", () => {
  if (button.dataset.command === "Ingest") { openIngestModal(); return; }
  commandHelp(button.dataset.command);
}));
// Compact = run GC to reclaim "Reclaimable" (superseded) blocks. Safe by
// Compaction now runs automatically in the background (daily) — no manual button.
byId("view-room").addEventListener("click", () => safely(loadThread));
byId("human-only").addEventListener("click", () => {
  state.humanOnly = !state.humanOnly; byId("human-only").classList.toggle("on", state.humanOnly); safely(loadThread);
});
byId("room").addEventListener("keydown", event => { if (event.key === "Enter") safely(loadThread); });

// Direct private chat bar (bottom of the center panel). A 64-hex "to" is a
// username (a peer DM, delivered concierge-to-concierge); anything else is a room.
async function sendChat() {
  const to = byId("chat-to").value.trim();
  const text = byId("chat-msg").value.trim();
  if (!to) { byId("chat-to").focus(); notice("Enter a recipient — a username or a room."); return; }
  if (!text) { byId("chat-msg").focus(); return; }
  const res = await postJson("/api/message", { room: to, body: text });
  byId("chat-msg").value = "";
  const room = res.room || to;        // a direct message resolves to a dm-room id
  byId("room").value = room;          // keep the Messenger view in sync
  state.room = room;
  const how = res.direct ? "direct message" : "message";
  logSystem(how + " → " + (res.delivered ? "delivered" : "queued (peer offline)"), res.delivered ? "ok" : "warn");
  await loadMe();                     // the node just came online on first send
  await loadRooms();
  await loadContacts();               // messaging a username approves them as a peer
  await loadThread();
}
byId("chat-bar").addEventListener("submit", event => { event.preventDefault(); safely(sendChat); });

