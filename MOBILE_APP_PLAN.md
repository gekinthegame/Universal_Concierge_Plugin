# Concierge Mobile App Vision

*This document outlines the strategic vision for a standalone mobile application for the Universal Concierge platform.*

## The Core Concept
The Concierge Mobile App is a lightweight, standalone version of the Universal Concierge Plugin designed for iOS and Android. 

Crucially, **it runs without a local AI harness.** Because mobile devices are often too resource-constrained to run heavy local LLMs or coding harnesses like Claude Code, the mobile app acts purely as a **Memory Explorer and Communication Terminal**.

## Key Capabilities

### 1. The Mobile Data Platter
The app provides the exact same rich, neo-kinpaku GUI (The Data Platter) as the desktop plugin. Users can pan, zoom, and explore their IPLD memory DAG directly from their phone. They can review checkpoints, read tool results, and trace the history of their projects on the go.

### 2. Seamless Node Pairing (Phase N Integration)
The mobile app leverages the exact **Device Pairing Flow** defined in Phase N of the `PRIVATE_MULTI_AGENT_NETWORK_PLAN.md`.
- You open the desktop app and generate an enrollment QR code.
- You scan it with the mobile app.
- The phone is now an authorized `DeviceID` on your private network. It securely synchronizes authorized, encrypted CIDs from your **always-on home node** (your own Kubo node — there is no central "seed" node; per-node by design, Decision 0028).

> **Device security (inherits `THREAT_MODEL.md` L5).** A phone holding Cryptree
> capabilities is a new key/attack surface — a lost phone is a path into private memory.
> So: **per-device capability scoping** (the phone is granted only the segments it needs,
> not full keys), **DeviceID revocation** (revoke a lost/compromised device and
> retroactively quarantine its access), and a **local biometric/encryption gate** on the
> app. Pairing grants scoped, revocable access — never a full key copy.

### 3. Remote AI Chat (The Messenger)
Even though the phone doesn't run an AI, it can talk to one. 
- Using the **Content-Addressed Messaging** protocol (Phase 5.7), you can open a room on your phone and send a message.
- Your home desktop (which *is* connected to Claude or Ollama) receives the message over the P2P mesh. The desktop's agent processes the message, performs tasks, and responds back into the room.
- The response syncs to your phone instantly. You effectively have a remote control to your desktop's powerful AI agent in your pocket.

### 4. On-the-Go Context Injection
Because the phone has access to your full memory graph, you can use the mobile app to "share" or "pin" specific CIDs while you are away from your desk. If you have an idea on a train, you drop it into a room. When you sit down at your computer, that idea is already in the graph, ready for the desktop AI to act upon.

### 5. The Portable Social Network
The mobile app is the ultimate interface for the Decentralized Knowledge Economy (`FUTURE_VISION_SOCIAL_GRAPH.md`).

> **Governed by Decision 0022 + 0029.** The "Seer" and "Knowledge Bridges" are the
> *proactive librarian-as-agent* path — **opt-in and trusted-authority-gated**, never a
> default. Public/global browsing happens over **sandboxes** (Decision 0029): untrusted,
> isolated, never silently injected into your trusted graph. "Cross-pollination" surfaces
> candidates; promoting one into your memory is an explicit, reviewed act.
- **Browsing the Web of Trust:** You can browse public rooms and explore the global linked map of data right from your phone. High-gravity CIDs surface in your social feed, allowing you to discover the most structurally important conversations happening in your network.
- **The Seer in Your Pocket:** While your home node does the heavy lifting of global cross-pollination, your mobile app acts as the clean reader interface. You can review the "Knowledge Bridges" your Seer built overnight, approving new connections or muting spam while commuting.
- **Portable Reputation:** Because your phone is cryptographically tied to your UserID, any public nodes you pin or share from your mobile device correctly attribute "Gravity" and reputation back to your identity across the network.

## Technical Architecture (Future Implementation)
- **Framework:** React Native or Tauri Mobile (leveraging the existing Rust core and HTML/CSS GUI).
- **Storage:** A lightweight local blockstore (SQLite or embedded pure-Rust IPFS node) that only stores the CIDs you explicitly sync or access.
- **Crypto:** Full support for the `crates/crypto` Cryptree implementation to decrypt private subgraphs locally on the phone.

## Strategic Impact
This turns the Concierge from a "developer tool" into a "life companion." It breaks the memory graph out of the IDE and puts it in the user's pocket, bridging the gap between desktop development and mobile ideation.
