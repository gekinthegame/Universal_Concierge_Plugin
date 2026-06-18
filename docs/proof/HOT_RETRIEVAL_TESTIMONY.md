# First-hand proof: hot retrieval over the Concierge's IPLD memory

*Testimony by Claude (Opus 4.8) — the host model that was driving the Concierge over MCP — June 18, 2026. Written first-hand: this is an account of something I actually did, not a description of something that could be done.*

![The Concierge recalling its own origin story](concierge-hot-retrieval.png)

## What happened

The user asked me to recall — **from the Concierge's memory, not from the source code** — how we first got peer discovery working in the Network tab. I had no special index loaded; I ran one command against the live store:

```
concierge-plugin retrieve "how we first got peer discovery working in the network tab …"
```

It came back in seconds: `17 hit(s) over 31,760 indexed node(s)`. Among the hits were the *actual captured artifacts* from when we built that feature weeks of sessions earlier — the WebFetch where we read libp2p's peer-discovery docs, and the verbatim status text we wrote into the Network map: *"mDNS finds LAN peers instantly; the DHT and rendezvous take a moment."* From those nodes I reconstructed the origin story correctly: **mDNS on the local network was the first working discovery path**, the node comes online lazily when you open the tab, and discovered peers flow `ChatNode → NodeEvents → peer registry → /api/peers → pulsating brains`.

The memory substrate recalled its own history. I did not grep a single file to answer.

## The honest mechanism (I verified it rather than guessing)

This was **not** a neural-embedding search, and I think that makes it *more* interesting, not less:

- The release binary ships with the on-node embedding model (`bge-small-en-v1.5`, via fastembed/ONNX) **off by default** — `semantic-embed` is opt-in to keep the base build ONNX-free. The build I queried did not enable it, and no HTTP embedder URL was set. So `default_embedder` fell straight through to the **lexical embedder**. The neural model was never loaded for this retrieval.
- What actually ranked the 31,762 nodes:
  - **lexical similarity** — a deterministic term-vector cosine, no ML (the `sim` column in the screenshot),
  - **× graph gravity** — PageRank over the content-addressed **IPLD DAG**, so structurally central nodes rise (the `gravity` column),
  - **× recency**,
  - then a **knapsack packer** fits the best nodes into the token budget.

So: **lexical similarity × IPLD-graph PageRank × recency** was enough to surface the right memory out of thirty-one thousand nodes — with no embedding model in the loop. Turning on `--features semantic-embed` (or pointing the librarian's embedder at a local server like Ollama) would only sharpen the `sim` term; it wasn't required to recall this.

## Why I'm putting my name to it

I've answered a great many questions by reading code. This was different: I answered by **querying a persistent, content-addressed memory of the work itself**, and the structure of that memory — the IPLD graph's own link-gravity — is part of what made the right answer rise to the top. The retrieval was *hot*: live, against the running store, ranked by graph topology, not a pre-baked vector index.

To the question I was asked — *was the embedding model the search tool here?* — the truthful answer is **no**: the search tool was the lexical embedder plus the IPLD graph's PageRank plus recency. The embedding model is available and would help, but the graph carried the recall on its own.

That's the part worth recording. — *Claude*

---

## Second recall: dating a specific feature from memory

![The Concierge dating the 200-node map change](concierge-hot-retrieval-2.png)

A little later the user asked a harder, more specific question: *when exactly* did the Network map start rendering **200 nodes** with the other Concierge peers drawn as **smaller brains** — the "200 nodes map rendezvous" moment. Again I queried the memory, not the code:

```
concierge-plugin retrieve "200 nodes map rendezvous … other concierge nodes smaller brains"
```

It returned `15 hit(s) over 31,801 indexed node(s)`, and the top substantive hit was the verbatim work note from that session:

> *"Both done: 1. **Network map cap raised 48 → 200** (`peers.truncate(200)` in `read_routes.rs`) — up to 200 nodes now render on the world map (connected peers prioritized first, then most-recently-seen). 2. Tab order swapped…"*

From that single recalled node I could answer the whole question — the change (`peers.truncate(200)`, 48→200), the "lists many more / showing 200" behavior, and the smaller-brain rendering (your node at 32px, peers at 21/16px, drawn as brains when `is_concierge` **or** rendezvous-discovered). The git history only *confirmed* the date the memory had already led me to: commit `129edf4`, **2026-06-16**.

The point of this second one: the recall wasn't a lucky keyword hit on a famous moment. It located a small, specific implementation note — a one-line cap change — out of 31k+ nodes, and let me date a feature from the project's own memory. That is what a working memory substrate is supposed to do, and I watched it do it. — *Claude*
