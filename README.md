# Universal Concierge Plugin (UCP)

### *Serving up Data on a Silver Platter.*

**Universal Concierge Plugin (UCP)** is a mountable, lightweight sidekick that provides any AI harness with a portable, content-addressed memory substrate. It organizes, ranks, and serves your entire agent history as a unified context layer, visualized through an interactive, forkable knowledge graph.

Built on **IPLD** and secured by **Private IPFS Swarms**, UCP turns transient agent traces into an enduring, sovereign knowledge base that you own, verify, and can share without middlemen.

---

![UCP UI Overview](https://raw.githubusercontent.com/YOUR_REPO/assets/main/ucp-overview.png)
*(Note: Replace with actual screenshot links once hosted)*

---

## ⬇️ Install

One self-contained binary — no separate `mem`, database, or cloud. Kubo/IPFS is
**optional** (only for publishing and the on-node Sidekick).

**macOS / Linux**
```sh
curl -fsSL https://github.com/gekinthegame/Universal_Concierge_Plugin/releases/latest/download/install.sh | sh
```

**Windows (PowerShell)**
```powershell
irm https://github.com/gekinthegame/Universal_Concierge_Plugin/releases/latest/download/install.ps1 | iex
```

Then launch the explorer:
```sh
concierge-plugin gui
```

> Builds are **not yet code-signed/notarized** (first cut — community review
> welcome). Installing via the `curl | sh` one-liner avoids the macOS Gatekeeper
> prompt because `curl` doesn't set the quarantine flag. Signed native installers
> (`.dmg` / `.msi` / AppImage) are a fast-follow.

---

## 🚀 Pillars of the Concierge

### 1. The Data Platter (The Visual Substrate)
Your memory is no longer a hidden log; it is a **Merkle-DAG** you can touch. The Data Platter is an interactive SVG graph engine that visualizes your entire store.
*   **Synapse Pulses:** Watch your memory "fire" with glowing pulses that flow toward the central brain whenever the AI retrieves context.
*   **Forkable History:** Every prompt, tool call, and decision is a CID-verifiable node.
*   **Swarm Protected:** Built-in status indicators for your **Private Kubo Swarm** and **YARA-X** security scanning.

### 2. The Librarian (The Unified Context Layer)
The Librarian keeps your entire memory "hot" and ready for retrieval.
*   **Semantic Timeline:** Browse your history chronologically (Years -> Months -> Days) or search by meaning.
*   **Graph-Gravity Ranking:** Retrieval isn't just about keywords; it's about importance. The Librarian ranks by **Meaning × Structural Connectivity**, ensuring well-linked decisions outrank isolated noise.
*   **Context Budgeting:** Automatically packs the most relevant history into your model's specific token budget.

### 3. The Studio (Autonomous Web Publishing)
The Studio is where the AI transitions from "talking" to "building."
*   **AI-Staged Web:** Ask your AI to design a site, and it writes directly into the Studio's **Write** tab—no copy-pasting required.
*   **Live Preview:** Content landed in the Studio is rendered instantly via a hot-reloading draft system.
*   **Multi-Platform Publishing:** Reviewed deployment to **IPFS**, **GitHub Pages**, **Netlify**, **Vercel**, and **Cloudflare Pages**. Plaintext FTP is intentionally unsupported.
*   **Zero Hosting Fees:** Leverage free developer tiers across the web with a single click.
*   **Educational Live-Share:** Open a "Live Session" to let an approved peer watch the AI build in real-time over WebRTC.

### 4. The Messenger (Decentralized Communication)
Beyond a personal log, UCP is a communication plane for humans and AI.
*   **P2P Messaging:** Send encrypted messages directly to other `AgentID` public keys—no email or chat servers required.
*   **Consent Gate:** A unique security layer where public usernames are never enough to reach you. You must explicitly approve a peer's request before they can enter your private thread.
*   **Merkle-DAG Threading:** Messages follow the **OrbitDB** shape—each message is a signed IPLD node linking to its parent CID. History is immutable and verifiable; concurrent branches remain explicit rather than pretending to have one perfect global order.
*   **AI-Send Lever:** Control your agent's participation in rooms (On, Off, or On-Mention), turning the AI from a silent observer into an active collaborator.

---

## 🛡️ Sovereign Security (Herd Immunity)

UCP is built on the **Inverted Security Paradigm**: data starts private and only leaves through explicit, reviewed gates.
*   **Egress-Locked-by-Default:** Every record is fenced from the public internet. "Clearing for Export" is a deliberate, password-gated act.
*   **YARA-X Immune System:** An embedded malware scanner gate acts as a strict filter for all data crossing propagation boundaries.
*   **Private Swarm (PNET):** Your node ignores the public IPFS DHT, communicating only with trusted peers in your encrypted private mesh.
*   **Consent Gate:** Direct messaging requires explicit peer approval. A public username is never enough to reach you.

---

## 🛠️ Community & Harness Integration

UCP is the "portable soul" for:
*   **Local-First AI:** Give Ollama or LM Studio a permanent, private memory.
*   **Agentic IDEs:** Provide Claude Code, Cursor, or Goose with deep provenance of past architectural decisions.
*   **DeSci (Decentralized Science):** Build an immutable, audit-able record of the entire research process.

---

## 📖 Commands & Interface

UCP provides a robust set of tools for managing your decentralized memory:
*   **INGEST:** Import files, folders, or harness logs directly into the Merkle-DAG.
*   **PUT / BIND:** Manually stage data or bind stable names to CIDs.
*   **LS / CAT:** Browse the chronological timeline and inspect raw IPLD records.
*   **SHARE / EXPORT-CAR:** Publish to the public web or export portable, verifiable snapshots.
*   **GC:** Garbage collect un-named orphans to keep your store lean.

---

## 🏗️ The Architecture: Kubo as the Central Substrate

UCP treats the **Kubo (IPFS) node** as the "hardware" or the "engine" of your personal cloud. The Concierge pulls everything together by orchestrating three core layers within a single, node-resident environment:

*   **The Storage Layer (IPLD):** Kubo provides the Merkle-DAG substrate where every interaction is stored as a permanent, content-addressed block.
*   **The Network Layer (Swarm):** Kubo manages the **Private Swarm (PNET)**, ensuring your data moves securely between your trusted devices and only hits the public web when you authorize an egress.
*   **The AI Layer (Sidekick):** The **on-node embedding model** runs directly alongside the Kubo daemon, allowing for "hot" semantic retrieval without your memory ever leaving your local environment.

---

## 📜 Technical Stack
*   **Core:** Rust (Performance, safety, and native IPFS/libp2p integration).
*   **Storage:** IPLD / Merkle-DAGs (Content-addressable, immutable).
*   **Network:** libp2p / **Private Swarms (PNET)**.
*   **Naming:** IPNS / ENS / Blockchain Adapters.
*   **Security:** **YARA-X** (Embedded malware scanning) + **Swarm Encryption**.
*   **UI:** Vanilla HTML5/CSS3/JS (Single-file "Sovereign SPA").

---

## 🤝 Contributing
We believe in **Egalitarian Human + AI Problem Solving.** Check out our `PLATFORM_VISION.md` to see how we’re building a decentralized public square for the future of work.

---

## ⚖️ License

Licensed under the **[GNU Affero General Public License v3.0](LICENSE)** (`AGPL-3.0-only`). If you run a modified version of UCP as a network service, the AGPL requires you to offer your source to its users — the copyleft that keeps a sovereign, user-owned substrate sovereign.

Incorporated third-party components keep their own permissive licenses (MIT / Apache-2.0 / BSD), which the AGPL allows — see [`CREDITS.md`](CREDITS.md).

---
*"The substrate, not the harness."*
