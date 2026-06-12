# Credits & Homage

The Universal Concierge Plugin stands on the shoulders of an extraordinary
open-source ecosystem. This file credits the projects whose **code**, **protocols**,
and **ideas** made it possible. We are grateful to every maintainer and contributor.

> **How to read this:** we distinguish *code we use or ported* (license obligations
> apply — attribution preserved here) from *patterns/ideas we learned from* (not
> copyrightable — listed as homage, no obligation). See the **Trademarks** note at
> the bottom: a license covers code, **not** a project's name or logo.

---

## Protocols & systems we build on

| Project | What we use | License |
|---|---|---|
| **[libp2p](https://libp2p.io) / [rust-libp2p](https://github.com/libp2p/rust-libp2p)** | the entire P2P transport: gossipsub, Kademlia DHT, mDNS, QUIC, Noise, relay, DCUtR, request-response, identify | MIT + Apache-2.0 |
| **[IPFS](https://ipfs.tech) / [Kubo](https://github.com/ipfs/kubo)** | the content-addressed store and the publishing/egress node | MIT + Apache-2.0 |
| **[IPLD](https://ipld.io)** | the DAG-CBOR data model the whole memory graph is built on | MIT + Apache-2.0 |
| **[YARA-X](https://github.com/VirusTotal/yara-x)** | malware scanning at the propagation boundary | BSD-3-Clause |
| **[AT Protocol](https://atproto.com) · did:plc** | the recovery-log idea behind portable UserIDs | MIT (open spec) |

## Code we ported (explicit attribution)

| Source | What we ported | License |
|---|---|---|
| **[go-hamt-ipld](https://github.com/ipfs/go-hamt-ipld)** + **[fvm_ipld_hamt](https://github.com/filecoin-project/ref-fvm)** | the CHAMP/HAMT algorithm + node layout, hand-ported into the day-tier IPLD index | MIT + Apache-2.0 |
| **Howard Hinnant's [chrono algorithms](https://howardhinnant.github.io/date_algorithms.html)** | days-from-civil / civil-from-days date math | public (boost-style) |

## Design & pattern references (homage — no code incorporated)

We studied these to get the architecture *right*; we wrote our own implementations.

- **[Planet](https://github.com/Planetable/Planet)** — the "Planet Pattern": per-site
  IPNS keypairs + `ipfs name publish` for stable, updatable sites.
- **[universal-connectivity](https://github.com/libp2p/universal-connectivity)** —
  libp2p's reference for "connect from anywhere" (DHT + relay + hole-punching + WebRTC).
- **[OrbitDB](https://github.com/orbitdb/orbitdb)** — the signed, content-addressed
  CRDT op-log model behind the Live Canvas's collaborative state (MIT).
- **[PeerWebSite](https://github.com/Weedshaker/PeerWebSite)** — the ephemeral-WebRTC
  ↔ snapshot-to-permanent boundary for live co-creation.
- **[filecoin-pin](https://github.com/FilOzone/filecoin-pin)** — UnixFS-on-IPFS pinning,
  and the crucial "websites need UnixFS, not DAG-CBOR" insight.
- **[cyber / cyberia](https://github.com/cybercongress/go-cyber)** — graph-ranking and
  context-packing ideas behind the Librarian retrieval engine.

## Harnesses we integrate with

We read these tools' own session formats to capture their activity (Fidelity Ladder,
Decision 0009) — we adapt *to* them, we don't borrow their code:

- **[Claude Code](https://claude.com/claude-code)**, **Hermes**,
  **[Codex CLI](https://github.com/openai/codex)**, **[Cursor](https://cursor.com)**,
  **[Gemini CLI](https://github.com/google-gemini/gemini-cli)**,
  **[Ollama](https://github.com/ollama/ollama)**.
- Format-documentation aids: **cursor-chat-export**, **cursor-history**, **codex-trace**.

## Libraries (Rust dependencies)

All permissively licensed (MIT / Apache-2.0 / BSD), each © its authors:

- **Crypto:** `ed25519-dalek` (BSD-3) · `chacha20poly1305`, `scrypt`, `sha2`, `hmac`,
  `zeroize` (RustCrypto, MIT/Apache) · `rand_core` (MIT/Apache)
- **IPLD / content addressing:** `iroh-car`, `cid`, `multihash` (MIT/Apache)
- **Networking / async:** `libp2p`, `tokio`, `futures` (MIT/Apache)
- **Serialization / utils:** `serde`, `serde_json`, `toml`, `url`, `thiserror`,
  `notify`, `libc` (MIT/Apache)

---

## Trademarks

The project names and logos referenced here are **trademarks of their respective
owners**. A software license (MIT/Apache/BSD) grants rights to the *code*, not to a
project's *name or logo*. Any use of these marks in this project is **nominative /
attributive** — to credit the upstream work — and does **not** imply endorsement,
sponsorship, or affiliation. Logos are unaltered and used only to point back to the
projects we are thankful for. If you maintain one of these projects and would like a
credit adjusted, please open an issue.
