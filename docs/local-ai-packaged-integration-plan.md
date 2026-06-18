# Plan: Integrate the Concierge into the `local-ai-packaged` Ecosystem

_Status: draft · Owner: Jason · Last updated: 2026-06-18_

## 1. Goal

Get the Universal Concierge Plugin in front of the self-hosted / local-AI
community by integrating it with **`coleam00/local-ai-packaged`** — Cole Medin's
Docker-Compose bundle (Ollama, n8n, Open WebUI, Flowise, Supabase, Qdrant, Neo4j,
Langfuse, SearXNG, Caddy; Apache-2.0; ~3.7k★ / 1.4k forks; oTTomator "Local AI"
Think Tank community).

The Concierge's role: the **sovereign, portable memory layer** for that stack.

## 2. Why this fits

- **Community = bullseye.** Self-hosting, Ollama-running, privacy- and
  data-sovereignty-minded users — exactly the Concierge's audience. Real
  distribution (active forum + large YouTube following).
- **Values match.** Local-first, own-your-data, open infrastructure.
- **Gap it fills.** The bundle has DB/vector/graph pieces (Supabase, Qdrant,
  Neo4j) but no *portable, signed, agent-owned* memory that travels across
  harnesses/models. That's the Concierge's thesis.

## 3. Honest constraint (drives the approach)

`local-ai-packaged` is **infrastructure, not a coding harness** — its components
store conversations in **databases**, not transcript files. So the Aider /
Claude Code "watch a transcript on disk" auto-capture model does **not** apply.
Integration is via container + API/hooks, not file-tailing.

## 4. Two integration angles

### A. Concierge AS the memory component (primary)
Run the Concierge as a service in the stack that agents (n8n nodes, Open WebUI,
Flowise) **write to and recall from** — competing with / complementing
Supabase+Qdrant+Neo4j as the memory/RAG layer, but sovereign and portable.

### B. Capture FROM the stack (secondary / additive)
Ingest what the stack already logs:
- **Langfuse** — captures every agent trace/conversation (richest source).
- **Open WebUI** — chat history.
- **n8n** — execution data in Supabase/Postgres.

These are DB/API-hook adapters, not file watchers.

## 5. Technical approach

1. **Concierge container** — package the plugin as a Docker image exposing its
   MCP/HTTP API (memory write + retrieve) on the stack's internal network.
2. **Overlay file** — ship a `docker-compose.override.yml` (idiomatic to this
   project, which already uses override files) that drops the Concierge service
   into an existing install with zero forking.
3. **n8n integration** — a starter workflow / custom node that calls the
   Concierge API to store and retrieve memory (mirrors how their RAG workflows
   already hit Qdrant/Supabase). Drop it in `n8n/backup/workflows/`-style.
4. **(Angle B) Langfuse hook** — a small adapter that pulls Langfuse traces into
   the Concierge store on a schedule or via webhook.
5. **Docs** — a short "Add sovereign memory to your local AI stack" guide.

## 6. Distribution strategy (in order)

1. **Build the integration first** — the container + n8n workflow + (optional)
   Langfuse hook. This is the substance; fork-vs-contribute is just packaging.
2. **Ship as an add-on overlay** — own lightweight repo (e.g.
   `concierge-local-ai-overlay`): the override file + guide. Fastest, zero
   maintenance burden for the other 10 services, fully under our control.
3. **Offer upstream** — PR an optional `--profile concierge` / override to
   `local-ai-packaged`. Propose, don't block on acceptance (maintainer may
   hesitate to add a memory service overlapping Supabase/Qdrant/Neo4j).
4. **Engage the community** — share the integration in the oTTomator Think Tank
   "Local AI" forum and relevant channels. This is the real reach, fork or not.

License note: Apache-2.0 permits fork / modify / redistribute (incl. commercial),
no permission required.

## 7. Phases / milestones

- **P0 — Spike (1–2 days):** Concierge runs as a container, reachable from n8n on
  the compose network; manual write+retrieve round-trip proven.
- **P1 — Overlay add-on:** override file + n8n starter workflow + guide; works on
  a fresh `local-ai-packaged` install. Publish the companion repo.
- **P2 — Capture hook (Angle B):** Langfuse → Concierge ingestion.
- **P3 — Upstream + community:** open the PR; post the walkthrough in the forum;
  short demo video.

## 8. Risks / open questions

- Overlap with existing memory components — position as *sovereign + portable*,
  not "another vector DB."
- Maintainer receptiveness to an upstream addition (mitigated: overlay stands
  alone regardless).
- Keeping the override current as upstream services churn env vars/images.
- Decide auth/network exposure for the Concierge service inside `private` vs
  `public` compose environments.

## 9. Success signals

- A user can add the Concierge to their stack with one override file + one guide.
- An n8n agent stores and recalls memory through the Concierge in a demo.
- Forum post / video drives installs; first external contributors or issues.

## 10. Decision

Pursue **contributor + independent at once**: an overlay add-on we control, plus
an upstream offer. Start with P0/P1. Revisit a full fork only if a distinct,
"Concierge-first" distribution becomes a deliberate product bet.
