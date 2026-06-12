# Future Vision: Autonomous Web Publishing & Sovereign Computing

*This document outlines the vision of the Universal Concierge Plugin (UCP) not just as an AI memory tool, but as a completely decentralized Operating System for the web. It returns power to the user, reviving the "OG Internet" ethos where publishing requires zero middlemen.*

## The Core Concept: The "No Middleman" Reality
Currently, digital real estate is rented from centralized landlords (AWS, Vercel, Apple, Google, Substack). By treating the local Kubo (IPFS) node as the primary egress layer, the Concierge abstracts away the complexity of decentralized hosting. 

The user simply interacts with their AI harness. The Concierge translates the AI's output into static, verifiable content and—**on an explicit, reviewed, password-gated publish**—pins it to the global network. **Zero hosting fees, zero deployment pipelines.**

> **Inherits from Decision 0026 + `THREAT_MODEL.md`.** "Zero-click" applies to
> *preparation*, never *egress*. The AI **prepares** the site (generates HTML, stages
> the IPLD directory); **publishing is always the explicit, reviewed, password-gated
> egress act** — never autonomous. Autonomous publish would break the system's primary
> worm/exfiltration control (egress-locked-by-default). And "zero censorship" is more
> precisely **mesh-scoped hygiene**: a mesh's Guardian + bad-CID list can refuse to
> relay abusive/illegal content (Decision 0029) — it is not an unmoderated free-for-all.

## Key Egress Capabilities

### 1. One-Click Website Publishing
A user can instruct their agent (e.g., Claude, Ollama): *"Design a landing page for my new project and stage it for publish."*
- **The Flow:** The AI generates the HTML/CSS/JS. The Concierge wraps the files into an IPLD Directory node and **stages an egress plan**. The user reviews it and authorizes one publish (password-gated, Decision 0026); only then is it pinned and the IPNS pointer updated.
- **The Result:** Seconds after the user approves, the site is live globally. Users can point a standard DNS domain (via DNSLink) to their IPNS address, offering a seamless web2-style browsing experience entirely powered by their local machine. The *preparation* is zero-friction; the *publish* is always a deliberate act.

### 2. Decentralized App & Binary Distribution
The Concierge bypasses centralized app stores and release platforms.
- **The Flow:** An AI compiles a binary (`.apk`, `.dmg`, `.exe`) or a game. The Concierge ingests the blob and returns a CID. Because distributing executables is the **highest-risk content type**, the publish runs through the propagation-boundary malware scan (YARA-X, `THREAT_MODEL.md` L4) as part of the egress review.
- **The Result:** The user shares that CID/IPNS link. Anyone in the world can download the exact, cryptographically verified software directly from the creator's node. No App Store review delays or revenue cuts. (Recipient nodes likewise refuse to relay anything their own scan flags — network herd immunity, Decision 0029.)

### 3. Media Streaming & Sovereign Content — *your own work*
IPFS is highly capable of streaming media.
- **The Flow:** A creator publishes **their own** media — an MP4 they shot, a podcast they recorded, a gallery they made — clearing it for egress like any other publish.
- **The Result:** The Concierge can dynamically generate an HTML front-end (a gallery or media player) mapping to those CIDs — a creator hosting and streaming *their own catalog* directly from their machine, with no platform taking a cut.

> **Publish what you own (Decision 0031 + `ACCEPTABLE_USE.md`).** This is for **your own or
> licensed** media — **not** a media locker for copyrighted films/music/software. Sharing
> content you don't have rights to is illegal and against the rules. The platform does **not**
> police copyright, but public sharing is **not anonymous**: pinning content publicly attaches
> your node's peer ID + IP to IPFS provider records, so public infringement is self-identifying
> and traceable (this is anti-torrent by design, and the responsibility is yours). *(Distinct
> from malware, which the Guardian actively quarantines as a network-health threat.)*

### 4. The Live Collaborative Canvas (Ephemeral Co-Creation)
Publishing isn't just for finished products. Adapting the [PeerWebSite](https://github.com/Weedshaker/PeerWebSite) model (WebRTC/WebTorrent), the Concierge enables real-time, zero-latency collaboration.

> **Tier-2, public-by-explicit-choice (Decisions 0030 + 0026).** A live session that
> remote participants can reach is a **public** act — it is *never* the private swarm node
> (a PNET node refuses non-key peers and can't serve the public network). So a live canvas
> is either (a) within your own trusted device-mesh, or (b) a **deliberately-opened,
> ephemeral, scoped** public session — opened the same way you clear a subgraph for egress.
> The WebRTC tunnel is ephemeral (nothing pinned, no private memory exposed); only the
> explicit **Snapshot** crosses into permanence and runs through the reviewed publish gate.
- **The Flow:** A user and their AI agent enter a "Live Mode" session. Instead of pinning every keystroke to IPFS, the Concierge opens a direct WebRTC tunnel.
- **The Result:** Global participants can connect directly to the user's node to watch a solution being built in real-time. For example, an engineer and their AI could live-code an open-source disaster response map. The audience watches the UI update instantly.
- **The Snapshot:** Once the live collaboration yields a successful solution, the user issues a "Snapshot" command. The Concierge seamlessly transitions the ephemeral WebRTC state into a permanent IPLD graph, pins the CIDs, and publishes it via IPNS (The Planet Pattern) for permanent global access.

### 5. Identity & Communication (Beyond Email)
Using the existing `libp2p` transport and cryptographic keys (`AgentID`/`UserID`), the Concierge replaces traditional communication silos.
- Users send encrypted data directly to other public keys. It functions as a robust alternative to email, avoiding servers that scan or harvest metadata.

## Architectural Blueprint (The Planet Pattern, Cross-Platform)
The feasibility of this zero-click publishing flow is validated by the [Planet](https://github.com/Planetable/Planet) open-source architecture. However, while Planet is a macOS-exclusive Swift app, the Concierge adapts this pattern in **pure, cross-platform Rust**, ensuring it works flawlessly on Windows, Linux, and macOS.
- **Per-Site Keys:** The Concierge generates a unique cryptographic keypair for every new website/project the AI spins up (using the IPFS `key gen` primitive).
- **IPNS Publishing:** When the AI updates a site's HTML, the Concierge pins the new folder, yielding a new CID. It then automatically executes an IPNS publish command (`ipfs name publish --key=<site_key> <new_cid>`), pointing the stable public address to the new content.
- **Key Portability:** The Concierge's Key Manager ensures that the keys controlling these decentralized sites are securely backed up locally and can be synced across the user's private device mesh, preventing loss of access to published domains regardless of the host OS.

## The Concierge as the Decentralized OS
If the Kubo node is the hardware, the Concierge is the Operating System. It hides the brutal command-line complexity of DHTs, Merkle-DAGs, and IPNS key management. It allows anyone to orchestrate a personal, sovereign cloud infrastructure purely through natural language interaction with their AI.