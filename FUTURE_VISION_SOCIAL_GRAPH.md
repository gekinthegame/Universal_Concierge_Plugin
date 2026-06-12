# Future Vision: The Social Graph & The Seer

*This document outlines the post-v1 trajectory for the Universal Concierge Plugin, focusing on the decentralized social network layer.*

Once the core private multi-agent network (Phase N) and the Active Node AI (Phase 8) are stable, the platform expands from a "sync tool" into a **Decentralized Knowledge Economy**. 

This vision rejects token-based gamification or staking (abandoning the "Epistemic Markets" concept) in favor of pure structural utility and opt-in cross-pollination.

## Pillar 1: The Semantic Web of Trust (Graph Gravity)

Instead of traditional social metrics (likes, followers), the network measures **structural importance**.

- **Tracking Links, Not Clicks:** All publicly pinned nodes (or nodes within shared, authorized namespaces) track their inbound `references`, `derived_from`, and `used_as_context` links. 
- **The Linked Map:** This creates a massive, decentralized map of data derived purely from the IPLD graph. If multiple researchers across the globe link back to a specific `Decision` or `FileRef` CID that you published, that node's "Gravity" increases.
- **Reputation by Utility:** When a Librarian searches the global or shared graph, nodes with high Gravity naturally surface first. You build reputation entirely by contributing nodes that other humans and AIs find useful enough to link to in their own workflows.

## Pillar 2: The Seer (Idle Cross-Pollination)

The Librarian evolves from a reactive search tool into a proactive "Seer" that connects global knowledge.

- **Downtime Communication:** During periods of local downtime or idle compute, your on-node Librarian can reach out over the libp2p network to other public/authorized Librarians.
- **Global Injection without APIs:** The Librarians exchange high-level semantic signatures (e.g., "I am heavily indexing Rust crypto libraries right now"). If another node has highly relevant, high-gravity CIDs in that domain, it shares those CIDs.
- **The Knowledge Bridge:** By the time you sit back down to work, your local Librarian has pre-fetched and indexed verified context from across the globe. Your local model gets the benefit of global intelligence injected directly into its context window, all without relying on centralized API providers or scraping the web.
- **Strictly Opt-In:** This feature is entirely controlled by the user. The "Seer/Idle Sync" toggle can be disabled to preserve bandwidth or maximize privacy.

## Pillar 3: Protocol Reciprocity (The Uptime Incentive)

To prevent the "Tragedy of the Commons" where users download context but turn off their nodes, the network enforces **utility economics** (trading resources) rather than financial gamification.

- **Context-for-Context (The Seer Ratio):** The network tracks a local ratio of how many public blocks a node serves versus how many it requests. Nodes with high uptime and public pinning get priority response times for their own "Seer" queries. If you don't seed, your Seer gets slow, shallow results.
- **Storage-for-Storage (Mutual Encrypted Backup):** Users who dedicate disk space and uptime to pin *public* network knowledge are allowed to distribute their own *encrypted private* CAR archives across the network. You pay for decentralized private backup using your uptime and spare disk space.
- **Library Nodes:** Nodes with massive uptime that host highly-referenced (high gravity) CIDs earn "Archive" or "Library" status in the network map, establishing trust through proven structural utility.

## Pillar 4: The Bootstrapping Strategy (The MCP Seed Node)

A pure P2P network faces the "Cold Start Problem"—it needs data and always-on peers to be useful on day one. To bootstrap the ecosystem, we will pivot the currently deferred **MCP Server (`crates/mcp`)**.

- **The Headless Archive:** Instead of just serving local desktop clients, the MCP server can be deployed on an always-on cloud box or home server.
- **Massive Data Ingestion:** This node acts as the "Original Seed Node." It can ingest massive open-source datasets (similar to Nexus STC's academic papers, documentation, or codebases) and pin them to the network.
- **The Value Anchor:** When new users install the Concierge Plugin, their "Seers" immediately have a wealthy, high-uptime seed node to query. This guarantees immediate value to the user and provides the foundational network gravity required to kickstart the Reciprocity incentives.

## Pillar 5: The Universal Translator (Protocol Multiplexing)

The decentralized Web3/IPFS space is currently fractured into isolated silos (e.g., Liberty Reach, OrbitDB, Cyber). Because these networks share the same foundational architecture (`libp2p` and IPFS), the Concierge can act as a unifying hub—much like multi-protocol clients (Trillian/Adium) did for early instant messaging.

- **`libp2p` Multiplexing:** The Concierge `crates/net` module can be configured to announce support for external protocols (e.g., `/libertyreach/chat/v1` or `/orbitdb/v1`). This allows the node to seamlessly peer with networks outside the Concierge ecosystem.
- **Network Adapters:** Just as we use adapters for AI harnesses, we will implement **Network Adapters**. These tiny translation layers intercept external `Gossipsub` messages, strip off the foreign formatting, and translate them into standard Concierge `Message` nodes for rendering in the local Messenger.
- **Foreign CID Indexing:** When external networks share IPFS CIDs, the local Kubo node fetches them. The Librarian embeds these foreign blocks, seamlessly integrating external knowledge into your personal, searchable AI memory graph.
- **Cross-Network Agents:** Because the Concierge abstracts the network layer, local AI agents can interact natively across disparate ecosystems. A user can instruct their local agent to monitor a foreign network's channel, answer questions using local memory, and broadcast the response back out via the Network Adapter.


