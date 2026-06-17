# Plan — Babylon.js as the Concierge's game + animation + movie engine

> **Shift.** Today the Studio ships *libraries* (Three.js, A-Frame, Phaser) and the AI hand-assembles
> a renderer/lights/materials/loop every time — the source of every footgun we've fixed (flat
> materials, blank captures, `THREE is not defined`, A-Frame load order). Embed a **medium engine
> (Babylon.js)** as a batteries-included substrate. Its scene graph + **seekable animation timeline**
> unifies games and movies: a game is the timeline run interactively; a movie is the same timeline
> *recorded* by our existing WebCodecs capture. One substrate, two playback modes.

## Hard constraints (carry every lesson forward)

1. **Deterministic & seekable** — movie export renders frame-by-frame; `window.__seek(t)` must draw
   the identical frame each time. Use Babylon **`AnimationGroup.goToFrame(t*fps)`** on *paused* groups
   + `scene.render()`. NEVER a real-time clock / `engine.runRenderLoop` during capture.
2. **Self-contained / offline** — vendor the **UMD global build** (`window.BABYLON`), loaded with a
   classic `<script>`. No ES modules, no importmap, no CORS in the sandboxed preview (the lesson from
   the Three.js global-build fix).
3. **Capturable** — runs in the null-origin preview iframe; our scratch-canvas blit in `capture.js`
   already handles any WebGL canvas, so Babylon captures with zero capture changes.

## Assets to vendor (licenses tracked)

- **Babylon core** — UMD `babylon.js` (global `BABYLON`), pinned. *(Source below — the local master
  has no prebuilt bundle.)* Apache-2.0.
- **CharacterController** — local `dist/CharacterController.js` (31 KB, Apache-2.0). 3rd/1st-person
  movement with full animation states (idle/walk/run/jump/strafe/slide), **no physics engine needed**
  (kinematic + `moveWithCollision`). Needs only the `BABYLON` global. The hardest game piece, solved.
- **Later phases** — `ammo.js` (1.8 MB, local) for full physics; glTF loaders for asset import; both
  available locally.

## `scaffold_engine('babylon')` — the cinematic, seekable starter

- Classic `<script src="./babylon.js"></script>` → global `BABYLON`.
- Engine + scene into `<canvas id="stage">`.
- **Cinematic defaults built-in** (carries the Three.js cinematic work, now native): PBR materials,
  IBL environment (`scene.createDefaultEnvironment` / `EnvironmentHelper`), **ACES tone mapping**
  (`imageProcessingConfiguration.toneMappingType = ACES`), soft shadows (`ShadowGenerator` +
  blur-exponential).
- **Seekable contract**: `window.__canvas/__fps/__duration`; `window.__seek(t)` advances paused
  `AnimationGroup`s via `goToFrame(t*fps)` then `scene.render()` — deterministic.
- **Two modes in one scene**: MOVIE (seekable timeline → captured to video) and GAME
  (`engine.runRenderLoop` + input). Capture always uses `__seek`; the live preview uses the loop.
- **Start screen as an overlay removed on click** — never `display:none` on the canvas (A-Frame
  lesson: a hidden parent gives a 0×0 renderer).

## `scaffold_engine('game')` — Babylon + CharacterController
Writes `babylon.js` + `CharacterController.js` + a starter that drops in a character (primitive or
glTF), wires WASD/jump, and a follow camera. Game-mode (real-time loop).

## Wiring (supersede the raw-Three steering)
- Studio **New → "Game / 3D"** project type scaffolds the Babylon starter.
- The movie-3D path and website-3D-hero guidance point at Babylon (seekable) as the premium 3D route;
  raw `three` stays for low-level/bespoke needs.
- AI guidance: scaffold instruction text + a `design_guide` topic (scene setup, PBR, AnimationGroups,
  the seekable contract, game vs movie, CharacterController usage).

## Phases (each shippable via evergreen v0.1.3)
1. **Substrate** — vendor Babylon core + CharacterController; `scaffold_engine('babylon')` cinematic +
   seekable + game/movie modes; capture compat; tests.
2. **Studio integration** — New → Game/3D; wire movie/website 3D → Babylon; `game` scaffold; AI guide.
3. **Methodology** — the CCGS-retargeted studio workflow (GDD → design → build → QA) building on the
   engine (the previously-chosen "retarget to web").
4. **Depth** — physics (ammo), glTF asset import, post-processing FX.

## Risks / caveats
- **Bundle size** — Babylon core (~4–5 MB) gets `include_bytes!`'d into the binary. Acceptable per the
  project's features-over-lean stance; a slim custom build is a later option.
- **Physics ≠ seek-deterministic** — live physics depends on accumulated steps, so **movies must use
  keyframe/AnimationGroup animation, not live physics** (physics is for games). Documented in the
  scaffold guidance.
- **Three/A-Frame/Phaser stay** — Babylon becomes the premium 3D/game/movie path; 2D motion graphics
  (GSAP/Lottie) and 2D games (Phaser) keep their own scaffolds.

## Open decision — how to obtain Babylon core
The local `Babylon.js-master` is a TS monorepo with **no prebuilt bundle**. Either (A) fetch the
official prebuilt UMD once and vendor it (pinned; identical code; fast), or (B) build the UMD from the
local master (offline-pure but a heavy npm install + build pipeline).
