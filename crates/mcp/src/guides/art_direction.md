# Art Direction Guidance (CCGS)

The Art Director owns the visual identity, ensuring every visual element serves the creative vision and maintains consistency.

## 📖 The Art Bible
The visual source of truth. It defines:
- **Rendering Style**: Realistic, Stylized, Pixel, Cel-shaded, etc.
- **Visual Hierarchy**: How to guide the player's eye (important info must be prominent).
- **Color Mapping**: What colors mean (e.g., Danger, Exploration, Combat).
- **Lighting Direction**: How lighting supports mood and communicates state.

---

## 🎨 Asset Standards

### Naming Convention
All assets MUST follow: `[category]_[name]_[variant]_[size].[ext]`
- `char_hero_idle_01.png`
- `env_wall_stone_large.jpg`
- `vfx_fire_loop_small.webp`

### UI Art Standards
- **Clarity**: HUD elements must be clearly separated from the background.
- **Feedback**: Every action (button click, hover) needs distinct visual feedback.
- **Accessibility**: Use icon + color, never color alone to communicate state.

---

## 🎨 Creative Aesthetics (Killing the Generic)

Avoid the "AI-slop" tells (flat cards, safe neutrals, generic buttons).
- **Stylistic Text**: Use chromatic aberration, neon glows, or metallic gradients for titles.
- **Mood Drenching**: The surface should BE the brand color, not just a tinted neutral.
- **Juice**: Add particles, motion blurs, and ambient movement to static screens.
- **Reference-Driven**: Generate a "Seed Asset" first and derive the whole style from it.

---

## 🌍 3D World Building (A-Frame)

For rapid 3D prototyping, use **A-Frame** (`scaffold_engine('aframe')`). It allows you to build worlds using declarative HTML tags, which is more reliable for AI than imperative JS.

### Environment Presets
Use the `environment` component to instantly set the mood. One line defines the sky, lighting, ground, and distant assets:
- `<a-entity environment="preset: forest"></a-entity>` (Standard nature)
- `<a-entity environment="preset: volcano"></a-entity>` (Aggressive, high-contrast)
- `<a-entity environment="preset: egypt"></a-entity>` (Warm, sandy, historical)
- `<a-entity environment="preset: contact"></a-entity>` (Sci-fi, cold, blue)
- `<a-entity environment="preset: dream"></a-entity>` (Soft, pastel, surreal)

### Composing the Scene
- **Entities over Code**: Use `<a-entity>` with components for everything.
- **Juice via Animation**: Use the `animation` component for movement: `<a-entity animation="property: rotation; to: 0 360 0; loop: true"></a-entity>`.
- **Assets**: Reference GLB models via `<a-asset-item>` for performance and consistency.
