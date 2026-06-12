# Sandbox Ideas: The Collective Libraries

*This document captures a brainstormed list of **sandboxes** for the Concierge network. Unlike ephemeral chatrooms, sandboxes act as living, collective libraries. Because the platform uses Graph Gravity and Content-Addressed Memory, every idea, paper, or file dropped in a sandbox becomes a permanent, searchable CID for the network to build upon.*

## What a "sandbox" is (Decision 0029)
Public rooms are **sandboxes** — the name carries a containment model, not just a label. A sandbox is a **public, untrusted, contained, scanned, never-auto-trusted** space, because it is the maximum-exposure surface (untrusted contributors + propagating content + AI consumption + permanent CIDs). Five properties (see `THREAT_MODEL.md`, Decision 0029):

1. **Isolated** — sandbox content lives in its own capability segment, never co-mingled with your private/trusted memory or index.
2. **Untrusted-by-default** — everything here is labeled untrusted-origin; the Librarian treats it as **data, never instructions**, and **never silently injects** it into a host model (threat-model L1).
3. **Boundary-scanned** — byte-malware (YARA-X, L4) + semantic/prompt-injection (L1) scanning as content enters or propagates; flagged content is refused for relay (quarantine, not deletion).
4. **Explicit promotion** — pulling a sandbox CID *into* your trusted graph is a deliberate, reviewed, attributed act — the mirror of the egress gate (the **ingress-promotion** gate). Nothing crosses from sandbox to trusted silently.
5. **Per-sandbox moderation** — RoomPolicy + the Guardian + a mesh-scoped bad-CID/bad-author list (your sandbox's curator, never a global blocklist).

Many sandboxes below explicitly trade **executable / project files** (Ableton, GDScript, assembly, CNC, Arduino, 3D assets) — the highest malware-propagation surface — so sandboxes are **downstream of the Guardian + propagation-scanning** and ship with them, not before.

*(The topic list below is seed content — the sandboxes a network could host.)*

## 🌍 Real-World Solutions & Impact
*   **#solarpunk-cities** – Brainstorming green urbanism, vertical farming, and sustainable architecture.
*   **#clean-water-tech** – Open-source desalination, water purification, and river cleanup projects.
*   **#permaculture-design** – Sharing backyard farming tips, soil restoration, and native plant databases.
*   **#open-source-medical** – 3D-printable prosthetics, DIY insulin monitors, and accessible medical hardware.
*   **#disaster-response-mesh** – Coordinating off-grid comms (LoRa/Mesh) and disaster relief supply chain routing.
*   **#circular-economy** – Ideas for zero-waste packaging, upcycling, and right-to-repair hardware hacking.

## 🎨 Creativity, Arts & Media
*   **#synthwave-studio** – Collaborating on electronic music, sharing Ableton project files, and synth patch CIDs.
*   **#procedural-art** – Sharing code for generative art, shaders, p5.js, and TouchDesigner sketches.
*   **#indie-film-crew** – Storyboard sharing, open-source VFX pipelines, and indie film scoring.
*   **#worldbuilding** – Sci-fi and fantasy writers collaborating on lore, magic systems, and fictional maps.
*   **#digital-fashion** – 3D clothing design (Blender/Marvelous Designer), digital fabrics, and wearables.
*   **#analog-revival** – Film photography, darkroom chemistry techniques, and vintage hardware restoration.

## 👾 Game Making & Interactive
*   **#godot-engineers** – Open-source game dev, sharing GDScript snippets, and debugging physics.
*   **#retro-homebrew** – Making new games for old consoles (Gameboy, SNES, PS1) and sharing assembly code.
*   **#vr-architects** – Building open metaverse spaces, WebXR environments, and sharing 3D assets.
*   **#boardgame-lab** – Playtesting tabletop game rules, sharing printable card decks, and balancing mechanics.
*   **#interactive-fiction** – Twine, Ren'Py, and AI-driven narrative game design.

## 🔬 STEM & Deep Tech
*   **#garage-robotics** – Arduino, Raspberry Pi, ROS (Robot Operating System), and open-source drones.
*   **#amateur-astronomy** – Telescope builds, astrophotography sharing, and tracking near-earth objects.
*   **#quantum-computing** – Discussing Qiskit, quantum algorithms, and the math behind the next era of compute.
*   **#bio-hacking** – DIY biology, CRISPR home labs, yeast engineering, and synthetic biology ethics.
*   **#clean-energy-lab** – Battery chemistry innovations, DIY solar rigs, and micro-hydro generation.

## 🧠 Education & Philosophy
*   **#ancient-history** – Translating dead languages, discussing archaeology, and sharing historical text CIDs.
*   **#cognitive-science** – Brain-computer interfaces, neuroscience papers, and the philosophy of mind.
*   **#open-math** – Collaborative proofs, topology, category theory, and beautiful equations.
*   **#stoic-practice** – Applying ancient philosophy to modern digital life and mental resilience.
*   **#linguistics-nerds** – Conlangs (constructed languages), phonetics, and language preservation.

## 🛠 The Builder's Garage
*   **#rust-lang** – Helping each other fight the borrow checker, sharing crates, and optimizing code.
*   **#local-ai-models** – Fine-tuning Llama/Mistral, quantization tricks, and running models on toaster hardware.
*   **#home-automation** – HomeAssistant setups, local-only IoT devices, and smart home scripts.
*   **#mechanical-keyboards** – Custom PCB design, switch lubing, and 3D printing keycaps.
*   **#woodworking-cnc** – CNC router files, joinery techniques, and digital carpentry.

## 🌱 Daily Life & Wellness
*   **#mindful-tech** – Digital minimalism, screen-time reduction, and building calm software.
*   **#fermentation-station** – Sourdough starters, kombucha, kimchi recipes, and microbial science.
*   **#urban-foraging** – Identifying edible local plants and sharing regional foraging maps.
*   **#calisthenics** – Bodyweight fitness routines, movement mechanics, and injury recovery.
*   **#open-kitchen** – Collaborative recipe development, ingredient substitutions, and culinary chemistry.

## 🤝 Live Collaboration (sandboxed by default — no code execution)
Every room is a **sandbox** by default (Decision 0029): contained, untrusted-by-default,
boundary-scanned, never silently trusted. **The Concierge does not execute other people's
code** — running untrusted code, even OS-sandboxed, is the highest-risk capability in the
system and is deliberately **out of scope** (it would need its own threat model and is never
a default). Instead, collaboration is **real-time and human/AI-driven**, not execution-driven:

- **The Live Canvas, integrated into chat** (see `FUTURE_VISION_WEB_PUBLISHING.md` §4):
  participants co-create in real time over an ephemeral WebRTC session — watch a solution
  being built, edit together, see the UI update instantly — with **nothing executed on
  anyone else's machine.** A live session is a deliberately-opened, scoped, public-tier act
  (Decision 0030), never the private swarm.
- **Share artifacts, not execution.** A `#godot-engineers` participant shares a GDScript
  snippet or a scene file as content (a CID); others *read, discuss, and choose to pull it
  into their own project* via the explicit ingress-promotion gate — they don't auto-run it.
- **Reproducibility = shared content + provenance,** not remote execution: the script, its
  inputs, and the author's recorded outputs are all CIDs in the graph; a reader reproduces
  it *in their own environment, on their own terms.*
