# Plan — Cinematic 3D defaults for the `three` scaffold

> **Problem.** AI-written Three.js defaults to flat: `MeshBasic/Phong`, a single ambient light,
> no environment, no shadows, and stiff `rotation += 0.01` motion. Our `scaffold_engine('three')`
> currently hands the model a minimal scene (StandardMaterial cube, ambient+directional, linear
> spin), so 3D looks "cheap and 2D." Fix it by baking professional defaults into the scaffold the
> model inherits — not by nagging it with a prompt.

## Hard constraints (non-negotiable — these shape every choice)

1. **Deterministic & seekable.** Video export renders frame-by-frame via `window.__seek(t)`; the
   same `t` MUST draw the same frame. So easing is a pure function of `t` — **never** `clock.getDelta()`
   or rAF lerp-toward-target (the common "cinematic easing" advice, which is non-deterministic and
   would desync/stutter on capture). GSAP timelines (`tl.time(t)`) or `ease(t)` math are the allowed
   forms.
2. **Offline / self-contained.** No CDN, no HDR download, no external assets. Only the vendored
   `three.module.min.js` (confirmed to include `PMREMGenerator`, `ACESFilmicToneMapping`,
   `EquirectangularReflectionMapping`, `SRGBColorSpace`, `PCFSoftShadowMap`).
3. **Capture-compatible.** Renders into `<canvas id="stage">`, `preserveDrawingBuffer: true`, sets
   `window.__canvas/__fps/__duration`, `__seek` renders synchronously.

## Changes

### 1. Rewrite the `three` scaffold snippet (`crates/mcp/src/lib.rs`, `tool_scaffold_engine`)
Replace the minimal scene with cinematic, deterministic, offline defaults:
- **Environment map** — `PMREMGenerator.fromEquirectangular` over a **canvas-generated gradient**
  (studio sky→ground). This is the single biggest upgrade: PBR materials need something to reflect.
  Offline, no HDR file.
- **Color & tone** — `outputColorSpace = SRGBColorSpace`, `toneMapping = ACESFilmicToneMapping`,
  `toneMappingExposure ≈ 1.1` (filmic grade, not washed-out).
- **Lighting** — three-point (key with shadows / fill / rim); env map carries soft ambient, so drop
  the flat `AmbientLight`.
- **Shadows** — `shadowMap.enabled` + `PCFSoftShadowMap`, key light `castShadow` with 2048² map +
  configured shadow camera, **plus a ground plane** that `receiveShadow` (without a receiver,
  shadows do nothing — the advice omits this).
- **PBR hero** — `MeshPhysicalMaterial` with metalness/roughness/clearcoat **matched to the surface**
  (not chrome-everything cargo-culting).
- **Eased seekable motion** — inline `easeInOutCubic(t)` driving rotation + an eased camera orbit;
  deterministic. Comment points to `scaffold_engine('motion')` + `tl.time(t)` for richer GSAP timelines.

### 2. Tighten the scaffold's instruction text
The returned guidance demands: PBR + env map, 3-point + soft shadows, ACES tone mapping, and
**seekable** easing (explicitly forbidding clock-delta / rAF-lerp). The snippet IS the carrier of 3D
guidance — we do **not** edit the bundled Impeccable `motion.md` (licensed/attributed, and it's a 2D
UI-motion guide).

### 3. Lock it with a test
Extend `scaffold_engine_three_gives_a_capturable_seekable_3d_contract` to assert the snippet contains
the cinematic tokens (`PMREMGenerator`, `ACESFilmicToneMapping`, `PCFSoftShadowMap`,
`MeshPhysicalMaterial`, `receiveShadow`, `easeInOut`) and is free of the determinism-breaking
`getDelta`.

## Out of scope (and why)
- **`<model-viewer>` / Sketchfab** — external web component + its own render loop (breaks frame-stepped
  capture) + mixed licensing. The on-brand path is `GLTFLoader` a `.glb` into our seekable scene; noted
  as a future follow-up (GLTFLoader is an addon we'd vendor), not this change.
- **Editing `motion.md`** — licensed Impeccable content; 3D guidance lives in the scaffold instead.

## Ship
Through the standing evergreen **v0.1.3** release (force-move tag after CI-parity green).
