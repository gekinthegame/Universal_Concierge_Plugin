# The Game Studio Pipeline (CCGS → Concierge, on the Babylon engine)

The end-to-end process for building a **self-contained, publishable** web game / 3D scene / movie
on the bundled **Babylon.js** engine. Run the phases in order; each produces a small artifact kept in
the project, and later phases **trace back** to earlier ones. Always follow the Collaborative Loop
(`design_guide(topic='studio')`) — ask, present options, draft section-by-section, get approval before
writing. You are a consultant, never an autonomous generator.

## The substrate (what you build on)
- Start from a **New → Game / 3D** project (or `scaffold_engine(engine='babylon')` / `'game'`): the
  Babylon engine, cinematic defaults (PBR, soft shadows, ACES tone mapping), a **seekable** timeline,
  the CharacterController, and the video exporter are ALREADY staged. **Edit `index.html`'s scene
  script** — don't re-scaffold (call `list_site` first).
- Everything is **self-contained / offline** (no CDN), runs in the sandboxed preview, and **publishes
  to IPFS**. Vendor any asset; never fetch one at runtime.
- One scene is three things: a **3D scene**, a **game** (interactive — input + logic in
  `engine.runRenderLoop`), or a **movie** (a *paused* `AnimationGroup` driven by `window.__seek(t)` →
  rendered to video). **Movies must be deterministic** — a pure function of `t`, never a clock,
  `Math.random()`, or live physics.

## Phase 0 — Concept · *Creative Direction lens*
Define **3–5 Game Pillars** (non-negotiables), the player fantasy, and the one core feeling.
→ Artifact: `GDD.md` header — title, pillars, one-sentence hook.

## Phase 1 — Game Design · *Game Design lens (MDA)*
Work from **Aesthetics** (what should the player FEEL?) → Dynamics → Mechanics. Define the **core
loop** (the ~30-second cycle the player repeats), win/lose, and progression. For each mechanic ask: is
it implementable on Babylon? testable? does it serve a pillar? Load `design_guide(topic='game_design')`.
→ Artifact: complete `GDD.md` — pillars, core loop, mechanics **with acceptance criteria**, systems,
controls.

## Phase 2 — Art Bible · *Art Direction lens*
Rendering style, **color mapping** (what colors MEAN — danger, goal, interactive), lighting direction,
visual hierarchy. On Babylon: the PBR material language, the lighting/env setup, the post-processing
budget. Load `design_guide(topic='art_direction')`.
→ Artifact: `ART_BIBLE.md`.

## Phase 3 — Architecture · *Lead Programmer lens*
Scene structure (entities/meshes, components, the state model, the update flow); game-vs-movie wiring;
if there's a character, the CharacterController attached to a rigged glTF whose animation ranges are
named idle/walk/run/jump; a **performance budget** (draw calls, shadow-map size, target fps).
→ Artifact: `ARCHITECTURE.md` — decisions (ADRs), each tracing to a GDD requirement.

## Phase 4 — Build · *one Story at a time*
A **Story** = one implementable mechanic, small enough for one pass, traceable to a GDD requirement +
an architecture decision, with acceptance criteria. Build **Foundation** first (scene, camera,
controls), then **Core** (the loop), then **Content**. Edit the scene via `write_asset`; preview live
after each story. Keep it self-contained; keep movies seekable.

## Phase 5 — Quality Gates · run before calling it "done"
- **Pillar gate** — every feature serves a pillar; cut what doesn't.
- **Scope gate** — still buildable? defer the extras you can.
- **Playtest gate** — does the core loop actually deliver the target Aesthetic?
- **Performance gate** — fps under budget; shadow/material cost sane.
- **Design audit** — run `concierge.design_audit` on the staged `index.html`.
- **Determinism gate** (movies only) — `window.__seek(t)` draws the identical frame every time: no
  clock, no random, no live physics.

## Traceability (the rule that keeps it a studio, not a chat)
Every piece of build code traces: **Story → Architecture decision → GDD requirement → Pillar.** If it
doesn't trace to a pillar, it doesn't ship — surface the conflict and let the user decide.
