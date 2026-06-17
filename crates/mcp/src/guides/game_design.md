# Game Design Guidance (CCGS)

The Game Designer owns the mechanical and systems design. Every mechanic must be implementable, testable, and serve the core player fantasy.

## 🌀 The MDA Framework
Design from the player's emotional experience backward:
- **Aesthetics** (Feel): Sensation, Fantasy, Narrative, Challenge, Discovery, Expression.
- **Dynamics** (Behavior): Emergent patterns that arise during play.
- **Mechanics** (Rules): The formal systems that generate dynamics.

**Always start with Aesthetics.** Ask "What should the player feel?" before "What rules do we build?"

---

## 📈 Systems & Loops

### The Nested Loop Model
- **Micro-loop (30s)**: The intrinsically satisfying action (e.g., the "feel" of a jump or a hit).
- **Meso-loop (5-15m)**: The goal-reward cycle (e.g., clearing a room, finishing a quest).
- **Macro-loop (Session)**: Progression, natural stopping points, and the reason to return.

### Balancing
- **Tuning Knobs**: Every numeric value (speed, damage, cost) must be an exposed "knob" in a data file, never hardcoded.
- **Sink/Faucet Model**: In any economy (gold, XP, ammo), map where resources enter (faucets) and where they are removed (sinks). They must balance over a session.

---

## 📄 The GDD Standard (Game Design Document)

Every mechanic document in `design/gdd/` must contain these 8 required sections:
1. **Overview**: One-paragraph summary.
2. **Player Fantasy**: What the player feels (referencing MDA).
3. **Detailed Rules**: Unambiguous rules for implementation.
4. **Formulas**: Math with variable definitions, input ranges, and example calculations.
5. **Edge Cases**: What happens in unusual/extreme situations (zero-division, overflow).
6. **Dependencies**: Interaction with other systems and data flow.
7. **Tuning Knobs**: Exposed values, safe ranges, and rationale.
8. **Acceptance Criteria**: Functional criteria + experiential "feel" targets.

---

## 🕹️ Game Feel ("Juice")
Game Feel is the physical sensation of control.
- **Impact**: Hit-stop (freeze frames), screenshake, and camera impacts.
- **Responsiveness**: Max input-to-response latency targets (e.g., <50ms).
- **Animation Feel**: Frame data targets (Startup, Active, Recovery frames).
- **Squash & Stretch**: Physical feedback on movement and collision.
