# Autoupdater Plan — App Channel + YARA-X Rules Channel

> **Why this exists.** Concierge ships as a self-contained binary (GitHub Releases,
> `releases/latest`). Today an install goes stale the moment `main` moves ahead of the
> last tag. And the YARA-X malware scanner is *worse than stale* without fresh rules — a
> scanner running last month's signatures gives a false sense of safety. The maintainer
> does **not** author YARA rules and should never have to. This plan makes both kinds of
> update arrive on their own.

---

## 0. The core insight: "autoupdater" is two channels, not one

| | **App channel** | **Rules channel** |
|---|---|---|
| What updates | the binary + GUI + the `yara-x` *engine* | the YARA *rules* (detection signatures) |
| Cadence | occasional (when we ship) | frequent (new malware daily) |
| Source | our GitHub release pipeline | curated **upstream feed**, re-published |
| Maintainer effort | cut a tag (already automated) | **zero** — a CI bot does it |
| Rail | GitHub Releases (existing) | **IPNS** through the user's Kubo node |

The confusion in "I don't update YARA-X, so how does the user get updates?" dissolves once
these are separated:

- **The engine** (`yara-x` crate, maintained by VirusTotal) updates via `cargo update` and
  rides into the next *app* release. We never hand-touch it.
- **The rules** we also never author. A bot mirrors an **upstream community feed** (YARA
  Forge — it aggregates signature-base, ReversingLabs, Elastic, etc., dedupes, validates),
  **signs** the result, and publishes it on the **rules channel**. The user's node pulls it
  automatically. We curate provenance and sign; we never write a signature.

---

## 1. Decisions (this plan locks these in)

- **D-AU-1 — Rules ride IPNS, sovereign-only.** No central download server. The rules
  bundle is content-addressed (CAR/UnixFS), pinned on the publisher's Kubo node, and pointed
  to by a single signed **IPNS** record. The user's Sidekick Kubo node resolves and fetches
  it. (Chosen over GitHub-asset distribution: on-brand with the sovereign thesis, rides
  infra we already have.)
- **D-AU-2 — Rule updates are fully automatic.** A deliberate, *scoped* exception to
  egress-locked-by-default: the rules-update poll + fetch is the one whitelisted silent
  egress channel, justified because stale malware rules are a *safety regression*, not a
  convenience gap. Everything else stays opt-in/password-gated. (See guardrails in §3.)
- **D-AU-3 — IPNS is the rail, not the trust anchor.** IPNS only provides a mutable
  "latest" pointer; a stolen IPNS key must not be able to ship poison. Trust comes from a
  **detached Ed25519 signature over the bundle, verified against a public key baked into the
  binary**, plus a **monotonic epoch** (anti-rollback) and a **freshness window**.
- **D-AU-4 — A baked-in baseline is the floor.** Because rules are IPNS-only, a fresh
  install with no node yet would have *zero* rules. So every app release **bundles a
  last-known-good ruleset at build time**; IPNS only ever *supersedes* it. IPNS-only governs
  *updates*, not the first byte.
- **D-AU-5 — Never break the scanner.** New rules are signature-gated → compiled/validated
  by `yara-x` → **atomically swapped** with **rollback to last-known-good**. A bad or
  un-verifiable bundle is rejected and the previous ruleset stays live.

---

## 2. Trust model (the part that actually matters for a scanner)

```
publisher (offline signing key)                         user's node
─────────────────────────────                           ───────────
rules bundle (CAR)  ──sign(Ed25519)──►  manifest{        resolve IPNS pointer
                                          epoch, version,  │
                                          cid, sha256,      ▼
                                          ts, sig }       fetch manifest + bundle by CID
        │ pin to Kubo                                       │
        ▼                                                   ▼
   IPNS pointer ──published──►  (mutable "latest")        VERIFY:
                                                           1. sig valid vs BAKED pubkey?   else reject
                                                           2. epoch > last_applied_epoch?  else reject (anti-rollback)
                                                           3. cid matches manifest.cid & sha256? else reject
                                                           4. ts within freshness window?  else warn(stale publisher)
                                                           5. yara-x compiles all rules?   else reject, keep LKG
                                                           ──► atomic swap active ruleset, persist epoch
```

- **Baked public key(s).** The verifying Ed25519 public key(s) ship *inside* the binary, so
  trust does not depend on IPNS, DNS, or GitHub. Multiple keys allowed (publisher + backup).
- **Key rotation/recovery** reuses the project's existing append-only rotation-log pattern
  (`recovery.rs` / did:plc-style): the active signing key can rotate without re-baking, as
  long as the rotation is signed by a recovery key whose pubkey is baked. No central service.
- **Anti-rollback.** `epoch` is monotonic and persisted; a replayed older-but-valid bundle
  is refused. **Freshness.** Manifest carries a signed timestamp; if the newest signed
  manifest is older than N days the GUI shows "rules may be stale — publisher quiet since X"
  (detects a dark/compromised publisher without trusting the clock for security).
- **Same anchor for the app channel.** Release tarballs are verified against `SHASUMS256.txt`
  **and** a detached minisign/Ed25519 signature with the same offline-key discipline —
  SHASUMS alone (hosted next to the file) proves nothing against a host compromise.

---

## 3. Guardrails that keep "fully automatic" from meaning "unsafe"

- Silent egress is **only** the rules manifest poll + bundle fetch. Nothing else egresses.
- Poll is a tiny IPNS resolve on an interval (default ~6h) + on node-online; jittered.
- Activation is the fail-safe pipeline in D-AU-5 — verify → validate → atomic swap → rollback.
- Quarantine actions a new ruleset triggers stay **reversible + block-scoped** (existing
  `moderation.rs` invariants) — a noisy rule can never irreversibly nuke the user's memory.
- A visible, honest GUI surface (see §6) shows current epoch, last fetch, publisher key, and
  a kill switch ("pause auto-rules"). Automatic ≠ hidden.

---

## 4. App channel (binary self-update)

1. **Check.** Background poll of the GitHub Releases API (`/releases/latest`), semver vs the
   running `CARGO_PKG_VERSION`. Cheap, transparent.
2. **Fetch.** Download the current platform asset (the `release.yml` matrix already emits
   mac arm64/x64, win `.exe`, linux, `.dmg`, install scripts, `SHASUMS256.txt`).
3. **Verify.** Check `SHASUMS256.txt` **and** detached signature vs baked key.
4. **Stage + swap.** Atomic rename into place; **apply on next launch** (stage-then-relaunch)
   rather than yanking the binary mid-run.
5. **macOS honesty.** Until a Developer ID exists, an unsigned `.app` autoupdate still hits
   Gatekeeper on first relaunch (one-time right-click→Open). The updater downloads + verifies
   + stages cleanly; we document the one-time prompt. Signed `.dmg`/`.msi` is the fast-follow.
6. **Rust impl.** Either the `self_update` crate (GitHub-releases-aware) or a thin hand-rolled
   check→download→verify→swap. Lean: hand-rolled verify path so we control the signature gate.

---

## 5. Rules channel (YARA-X)

### 5a. Publisher pipeline (CI bot — runs without the maintainer)
Scheduled GitHub Action (e.g. daily):
1. Pull the latest **YARA Forge** package (pinned to a tier: `core` to start).
2. **Compile/validate** against the exact `yara-x` version the current app ships.
3. Strip/normalize, build a deterministic **CAR/UnixFS bundle**.
4. **Sign** bundle (Ed25519, offline/secret key in Actions secret or, better, an external
   signer) → emit `manifest.json {epoch, version, cid, sha256, ts, sig}`.
5. **Pin** to the publisher Kubo node; **publish** the IPNS pointer to the manifest.
6. (Optional later) mirror the manifest CID to a GitHub release for transparency/audit.

### 5b. Node consumer (in the app)
- On node-online + interval: resolve IPNS → fetch manifest → run the §2 verify ladder →
  on pass, fetch bundle by CID, compile, **atomic swap**, persist `epoch`.
- On any failure: keep last-known-good, log, surface state in GUI.

### 5c. First-run / no-node bootstrap (D-AU-4)
- The app embeds a **baked baseline ruleset** (the bundle CID that was current at build time,
  vendored into the binary). Scanner is functional from byte one, offline.
- When a Kubo node is enabled, IPNS updates supersede the baseline. The baseline also serves
  as the rollback target before the first successful IPNS fetch.

---

## 5d. Incentive / flywheel — why this is a *pillar*, not a chore

The IPNS-only choice (D-AU-1) turns the rules channel into a **first-class node-uptime
incentive**, and a uniquely strong one:

- **Self-interested, not altruistic.** The existing reasons to keep a node up — network
  participation (Decision 0022), serving the librarian, re-pinning peers' ciphertext — are
  good-citizen reasons, easy to rationalize away. **Fresh malware rules are self-defense.** A
  node that's been down two weeks doesn't merely "not contribute"; *the user's own*
  protection is measurably older than the threats. It's a ticking clock pointed at the user,
  not the network — which is exactly what makes it convert "keep your node up" from a favor
  into something selfish.
- **P2P re-serve = network effect.** Because the bundle is content-addressed, every live node
  that has fetched the current epoch can **re-serve it to peers**. Uptime keeps *you* fresh
  *and* makes you a replica in the rule-distribution swarm: more uptime → more replicas →
  faster, more censorship-resistant propagation, **no central download server** to trust or
  take down. The same sovereign property that motivated IPNS-only pays off here as resilience
  instead of fragility. Reuses the existing pin/serve paths (`node.rs`, `car.rs`).

**The honesty guardrail (non-negotiable).** The baked baseline (D-AU-4) deliberately *softens*
this incentive so a node-less install is never defenseless — which means a user can coast on
stale-but-functional rules. How visibly we let protection *decay* when the node is down is a
design dial, and we point it at the **honest** end: surface `"rules are N days old — enable
your node to refresh"` as a *true, quantified status*, never a manufactured-fear nag. Stale
malware signatures are a genuine, measurable regression; we report that plainly and stop. The
moment it tips into dark-pattern fear ("YOU ARE AT RISK"), it betrays the sovereignty ethos
that makes the whole design coherent. Incentive by honest consequence, not by anxiety.

---

## 6. Surfaces (CLI + GUI)

- **CLI:** `concierge update check|apply` (app); `concierge rules status|refresh|pin <key>`
  (rules — show epoch/age/publisher, force a refresh, pin a publisher key).
- **GUI:** a small "Updates" panel — app version + "update available", rules epoch + last
  fetch + publisher key fingerprint + freshness state, and a **pause auto-rules** kill switch.
  Honest, not hidden — consistent with the egress-transparency ethos.

---

## 7. Workspace layout

- `crates/core/src/update/` — `app.rs` (binary channel), `rules.rs` (IPNS rules channel),
  `verify.rs` (signature/epoch/freshness ladder, shared), `baseline.rs` (baked floor).
- Reuse: `node.rs` (Kubo resolve/fetch/pin), `recovery.rs` (key rotation pattern),
  `moderation.rs` (reversible quarantine the scanner feeds), `car.rs` (bundle pack/unpack).
- `.github/workflows/rules-publish.yml` — the §5a bot. `release.yml` — extend to emit the
  detached release signature + bake the current rules baseline CID.

---

## 8. Phase ladder (each phase independently shippable)

- **A — App self-update (check + notify).** Poll releases, compare semver, surface "update
  available" in GUI/CLI. *Exit:* a stale install tells the user a newer version exists.
- **B — App self-update (verified apply).** Download → SHASUMS + signature verify → stage →
  apply-on-relaunch. *Exit:* one click moves v0.1.3 → next with verification, mac caveat documented.
- **C — Rules verify core.** `verify.rs` ladder + baked baseline + baked pubkey + `epoch`
  persistence; load/validate/atomic-swap/rollback with a *local* bundle. *Exit:* a tampered
  or downgraded bundle is rejected; LKG stays live; good bundle activates atomically.
- **D — Rules over IPNS.** Wire the node consumer (§5b) to a real IPNS pointer; first-run
  baseline → IPNS supersede. *Exit:* publishing a new signed bundle to IPNS reaches a running
  node automatically and activates after verification.
- **E — Publisher pipeline.** `rules-publish.yml` pulls YARA Forge → validate → sign → pin →
  publish IPNS, on a schedule. *Exit:* a new upstream rule appears on user nodes within a day
  with zero maintainer action.
- **F — Polish.** GUI Updates panel, kill switch, freshness warnings, key-rotation path,
  signed `.dmg`/`.msi` once Developer ID lands.

---

## 9. Open questions / risks

- **Signing-key custody.** Where does the offline Ed25519 secret live (HSM? hardware key?
  Actions secret as the weak v1)? The whole scanner's integrity rests here.
- **YARA Forge tier + license review.** Start with `core`; confirm aggregate licensing is
  fine to redistribute (it generally is, but verify before shipping).
- **Bundle size / bake cost.** Full rule sets are large; baking a baseline bloats the binary.
  Likely bake `core` only, fetch `extended/full` over IPNS.
- **`yara-x` version skew.** Rules validated against engine vX may warn on vY; the manifest
  should record the engine version it was validated against and the node should tolerate/skip
  gracefully rather than reject the whole bundle.
- **macOS notarization** gates a truly seamless app autoupdate; tracked as a fast-follow, not
  a blocker for B.
