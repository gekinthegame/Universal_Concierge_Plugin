# Codebase Audit Remediation Plan

## Objective

Resolve the correctness, durability, and maintainability issues identified in the
June 12, 2026 codebase audit without broad redesign. The work must preserve the
existing architecture and security posture while making behavior honest under
concurrency, failure, and mixed network traffic.

This plan is ordered by risk. Phases 1 through 4 are correctness gates and should
land before feature work that expands networking, pairing, or Sidekick behavior.

## Baseline

Current validation results:

- `cargo test --workspace`: 556 passed, 1 ignored, 0 failed.
- Python adapter tests: 84 passed, 0 failed.
- Python syntax parsing: 25 files passed.
- `git diff --check`: passed.
- `cargo fmt --all -- --check`: failed broadly.
- `cargo clippy --workspace --all-targets -- -D warnings`: failed.
- First-party source LOC, excluding `target/` and bundled minified JavaScript:
  54,460.

The passing tests establish a useful regression baseline, but they do not cover
the concurrency and event-routing failures addressed below.

## Non-Negotiable Rules

1. Never acknowledge a direct message until the application has durably accepted
   or intentionally rejected it according to an explicit protocol result.
2. A single-use pairing offer must have exactly one successful consumer across
   threads and processes.
3. Sidekick status must describe the dedicated private Sidekick node, never the
   public publishing node or an unrelated listener.
4. A successful sync receipt must mean the converged heads were durably saved.
5. Mutable local state must not lose updates or expose partial JSON during
   concurrent GUI, CLI, or background operations.
6. Do not weaken existing egress, capability, revocation, CSRF, or private-swarm
   controls while fixing these issues.
7. Every correctness fix requires a targeted regression test that fails before
   the fix.

## Phase 1 - Preserve Mixed Network Events During Sync

### Problem

`sync_from_peer` waits for specific head/block events by reading the shared
`NodeEvent` receiver. `await_event` currently discards unrelated events. This can
drop inbound direct messages, room messages, delivery acknowledgements, and
operational events. The direct-message sender may already receive `Delivered`,
making the loss permanent.

### Implementation

- Replace destructive filtering over the shared receiver with an event-routing
  design that preserves unrelated events.
- Prefer one long-lived event dispatcher per `ConciergeNode`:
  - correlate block/head/push responses by request ID;
  - route sync responses to per-request one-shot channels;
  - forward application events to the normal event consumer;
  - retain observable failures rather than silently consuming them.
- Extend request/response correlation so concurrent sync operations cannot steal
  each other's events.
- Change direct-message acknowledgement semantics:
  - do not send `Delivered` merely because bytes reached the transport;
  - return an explicit accepted/rejected response after the application consent
    gate and durable store operation complete;
  - keep the sender outbox entry when acceptance is not confirmed.
- Remove or redesign `await_event` once no caller depends on destructive filtering.

### Required Tests

- Inject a direct message while a block sync is waiting; prove the message reaches
  the application consumer and the sync still completes.
- Run two concurrent sync requests; prove responses are correlated correctly.
- Prove an inbound message that fails durable acceptance is not acknowledged as
  delivered and remains in the sender retry outbox.
- Prove room, listening, and operation-failure events remain observable during
  sync.

### Exit Criteria

- No unrelated `NodeEvent` is discarded by sync response waiting.
- Direct-message delivery acknowledgement means durable application acceptance.
- Existing network and sync tests remain green.

## Phase 2 - Make Pairing Offers Atomically Single-Use

### Problem

`MemCli::complete_pairing` checks consumed offers, issues a grant, then records
consumption through separate unlocked file operations. Two concurrent consumers
can both pass the initial check and receive valid grants.

### Implementation

- Acquire a cross-process pairing policy lock before reading or mutating pairing
  offer state. Reuse the existing security/policy lock discipline where practical.
- Under the lock:
  1. re-read the consumed set;
  2. reject an already-consumed offer;
  3. load and verify the pending offer;
  4. issue the grant;
  5. atomically persist the consumed marker;
  6. remove or atomically transition the pending offer.
- Use atomic private writes for consumed-offer state.
- Treat a failed consumed-state write as a failed pairing. Do not return a grant
  unless one-use consumption is durable.
- Record the security event only after the committed transition.

### Required Tests

- Race multiple threads against one valid offer; assert exactly one grant succeeds.
- Race multiple processes if the test harness supports it; assert exactly one
  winner.
- Inject a consumed-state write failure; assert no grant is returned.
- Verify replay remains rejected after restart.

### Exit Criteria

- The documented atomic single-use guarantee is true across threads and processes.
- No successful grant can exist without durable offer consumption.

## Phase 3 - Separate Sidekick Node Status and Lifecycle

### Problem

Sidekick status currently probes the selected publishing backend. An unrelated or
public Kubo endpoint can therefore make the private Sidekick appear operational
and cause `enable_sidekick` to skip launching it. Disabling the Sidekick only
removes a sentinel even though the module claims the node and Sidekick stop
together.

### Implementation

- Define dedicated private-node API, gateway, and swarm addresses for the Sidekick
  repository. Do not share the publishing node's endpoint.
- Add a private-node readiness probe tied to the Sidekick repo and expected node
  identity, not only an open TCP port.
- Make `SidekickStatus.node_running` use that private-node probe.
- Make `enable_sidekick` launch the private node unless that exact node is ready.
- Track the launched daemon process or use a repo-scoped shutdown command so
  `disable_sidekick` actually stops the private node.
- Wait for readiness on enable and shutdown completion on disable, with bounded
  timeouts and actionable errors.
- Keep public publishing-node status separate in types, APIs, and GUI labels.

### Required Tests

- A fake service or public Kubo listener on the publishing port must not make the
  Sidekick operational.
- Enabling starts the dedicated private node and only reports operational after
  readiness succeeds.
- Disabling stops the dedicated node and reports non-operational.
- Public publishing and private Sidekick nodes can run simultaneously without
  status confusion.

### Exit Criteria

- Sidekick status is accurate and cannot be satisfied by the publishing endpoint.
- Enable and disable behavior matches the documented lifecycle.

## Phase 4 - Make Sync Commit Results Honest

### Problem

`sync_from_peer` ignores errors from `set_local_heads` and returns a successful
receipt that claims convergence even when durable head persistence failed.

### Implementation

- Propagate `set_local_heads` errors from `sync_from_peer`.
- Return a success receipt only after durable head persistence completes.
- Consider distinguishing imported-but-not-committed failures so callers can
  retry without claiming convergence.
- Ensure local-head writes use atomic replacement.

### Required Tests

- Force local-head persistence failure; assert sync returns an error and no success
  receipt.
- Retry after the failure is removed; assert convergence succeeds without corrupt
  state.
- Confirm existing successful-sync behavior is unchanged.

### Exit Criteria

- Every success receipt corresponds to durably saved converged heads.

## Phase 5 - Harden Mutable Local State

### Problem

Several local JSON registries use unlocked read-modify-write and direct writes.
Concurrent GUI requests, CLI processes, and background tasks can lose updates or
leave partially written files.

### Scope

Audit and harden at least:

- contacts and pending message requests;
- social/follow state;
- room policy state;
- DM outbox;
- sites registry;
- sync heads and local heads;
- pairing state;
- capture offsets;
- GUI lock metadata;
- identity/recovery/network metadata where writes are not already protected.

### Implementation

- Introduce one small, established persistence helper for:
  - parent-directory creation;
  - advisory cross-process locking;
  - read-modify-write while holding the lock;
  - temporary-file write, flush, and atomic rename;
  - owner-only permissions for security-sensitive state.
- Migrate state modules incrementally instead of broadly rewriting domain logic.
- Stop swallowing persistence errors where they change user-visible correctness.
- Define which best-effort caches may legitimately degrade and document those
  exceptions.

### Required Tests

- Concurrent updates to contacts, social state, room policy, and DM outbox preserve
  every update.
- Readers never observe truncated or malformed JSON during repeated writes.
- Inject write and rename failures; verify callers receive errors and prior state
  remains valid.

### Exit Criteria

- Mutable authoritative state has locking and atomic-write guarantees.
- Best-effort cache writes are clearly separated from authoritative writes.

## Phase 6 - Restore Rust Quality Gates

### Implementation

- Run `cargo fmt --all` and commit only formatting changes that belong to the
  current code.
- Resolve strict Clippy findings, beginning with production findings:
  - move regex compilation out of loops in `crates/core/src/design.rs`;
  - use current standard-library idioms;
  - remove dead and unused imports/functions;
  - address test-only lint findings or explicitly justify narrow allowances.
- Add repository CI that runs:
  - `cargo fmt --all -- --check`;
  - `cargo clippy --workspace --all-targets -- -D warnings`;
  - `cargo test --workspace`;
  - Python adapter tests;
  - Python syntax/compile checks;
  - `git diff --check`.
- Commit `Cargo.lock` for reproducible application builds, or document a concrete
  reason not to treat this workspace as an application workspace.
- Add a pinned Rust toolchain if release reproducibility requires it.

### Exit Criteria

- Formatting, strict Clippy, tests, and whitespace checks pass in CI.
- A clean checkout can reproduce the validated dependency graph.

## Phase 7 - Add Missing MCP Regression Coverage

### Problem

The MCP crate exposes read and write capabilities over JSON-RPC but currently has
no Rust tests.

### Implementation and Required Tests

- Add unit-level dispatch tests for initialization, notifications, unknown methods,
  malformed requests, and resource reads.
- Prove write tools are absent and rejected while write mode is disabled.
- Prove write tools remain staging-only and cannot publish or egress.
- Test safe site/path handling, including traversal and malformed base64 cases.
- Test MCP output framing so stdout contains only valid JSON-RPC messages.

### Exit Criteria

- MCP behavior and its security boundaries have targeted automated coverage.

## Phase 8 - Reduce Maintenance Hotspots

This phase follows correctness stabilization. Do not mix it into the fixes above.

### Refactor Targets

- Split `crates/gui/src/lib.rs` by responsibility:
  - HTTP parsing/security gate;
  - read routes;
  - mutation routes;
  - messaging runtime;
  - canvas/site preview;
  - capture/background services.
- Split `crates/core/src/binding.rs` into focused binding extensions or domain
  services while preserving `CoreBinding` and `MemCli` behavior.
- Split CLI command groups into modules with shared argument helpers.
- Separate network transport behavior, sync orchestration, and messaging event
  routing in `crates/net`.

### Rules

- Preserve public APIs unless a change removes a demonstrated correctness problem.
- Move existing tests with their behavior; do not reduce coverage.
- Keep refactors mechanical and independently reviewable.

### Exit Criteria

- No core runtime module combines unrelated transport, persistence, routing, and
  presentation responsibilities.
- Module boundaries match the existing domain concepts.

## Final Verification Gate

Before declaring the remediation complete:

1. Run all quality gates from Phase 6 on a clean checkout.
2. Run targeted race tests repeatedly to expose timing-sensitive failures.
3. Run the real localhost libp2p sync and messaging tests together.
4. Demonstrate:
   - a message arriving during sync is retained and correctly acknowledged;
   - one pairing offer produces exactly one grant under a race;
   - Sidekick status distinguishes private and public Kubo nodes;
   - a failed sync-state commit never reports success;
   - concurrent local-state updates survive restart.
5. Recalculate LOC and identify any remaining files above 2,000 LOC.

## Recommended Execution Order

1. Phase 1 - mixed network event preservation and acknowledgement semantics.
2. Phase 2 - pairing atomicity.
3. Phase 3 - Sidekick status and lifecycle.
4. Phase 4 - honest sync commits.
5. Phase 5 - mutable-state durability.
6. Phase 6 and Phase 7 - quality gates and MCP coverage.
7. Phase 8 - module decomposition.

Phases 1 through 5 are release blockers. Phase 6 is the merge-quality gate.
Phase 7 is required before expanding MCP capabilities. Phase 8 is maintainability
work and should begin only after the correctness fixes are stable.

---

## Re-Audit Addendum - June 12, 2026

### Re-Audit Conclusion

The codebase is **not release-ready** after the latest changes. The original
Phases 1 through 5 remain open, and the new Studio live-share, multi-platform
deployment, and sovereign-naming work introduces additional release-blocking
security and correctness issues.

The new Phase 0 work below must land before the original remediation sequence.
Passing tests do not cover the newly exposed browser-origin, deployment-folder,
credential-routing, replay, or concurrency boundaries.

### Current Validation and Size

- `cargo test --workspace`: 569 passed, 1 ignored, 0 failed.
- Python adapter tests: 84 passed, 0 failed.
- Embedded GUI JavaScript syntax (`node --check`): passed.
- `git diff --check`: passed.
- `cargo fmt --all -- --check`: failed broadly.
- `cargo clippy --workspace --all-targets -- -D warnings`: failed.
- First-party Rust/Python/HTML source LOC: 56,848, up 2,388 from the prior audit.
- All repository code LOC including the untracked `3mail-main/`,
  `Planet-main/`, and bundled engines: 130,734.
- `3mail-main/` and `Planet-main/` add about 41 MB and 1,475 files at the
  repository root.
- Largest first-party files:
  - `crates/gui/src/lib.rs`: 5,918 LOC.
  - `crates/core/src/binding.rs`: 3,678 LOC.
  - `crates/gui/src/index.html`: 2,892 LOC.
  - `crates/cli/src/main.rs`: 2,614 LOC.
  - `crates/net/src/lib.rs`: 2,145 LOC.

### Status of the Original Plan

- **Phase 1 remains open:** `await_event` still consumes and drops unrelated
  network events, and the transport still sends `Delivered` before application
  acceptance.
- **Phase 2 remains open:** pairing consumption is still an unlocked,
  multi-step read-modify-write operation.
- **Phase 3 remains open:** Sidekick status still probes the selected publishing
  backend, and disable still leaves the node running.
- **Phase 4 remains open:** sync still ignores `set_local_heads` failure and
  returns a success receipt.
- **Phase 5 remains open and expanded:** contact cards, introductions, site
  checkpoints, social state, and other new JSON stores add more unlocked and
  non-atomic writes.
- **Phase 6 remains open:** formatting and strict Clippy fail, no repository CI
  was found, and `Cargo.lock` remains ignored.
- **Phase 7 remains open:** the expanded MCP crate still has zero Rust tests.
- **Phase 8 pressure increased:** the largest GUI and binding modules grew
  substantially and continue to combine unrelated responsibilities.

## Phase 0A - Isolate Untrusted Studio and Live-Share Content

### Problems

- The Studio preview iframe combines `allow-scripts` and `allow-same-origin`.
  AI-staged or remotely shared HTML can therefore act as the loopback GUI
  origin, fetch `/api/meta` for the CSRF token, and invoke privileged APIs.
- Remote canvas signals are handled before the approved-contact consent gate.
  The transport sender is discarded, and the claimed `from` identity is not
  bound to the authenticated peer.
- Canvas session queues are capped per session, but the session map is unbounded.
  An unapproved remote peer can create unlimited session keys.
- Multi-file folder preview responses use the default `X-Frame-Options: DENY`
  and `frame-ancestors 'none'`, contradicting the feature's requirement to load
  those responses in the Studio iframe.

### Implementation

- Treat every staged, opened-folder, and live-shared page as hostile active
  content.
- Serve previews from a separate origin where practical. At minimum, remove
  `allow-same-origin`, `allow-popups`, and `allow-forms` from the iframe sandbox
  unless a narrowly reviewed feature requires them.
- Allow only the dedicated preview responses to be framed by the Studio while
  preserving the opaque sandbox origin.
- Route remote canvas signaling through authenticated application handling:
  - bind the message to the transport peer;
  - require approved-contact or explicit session-invite authorization;
  - reject claimed sender identities that do not match the transport peer;
  - use unguessable, scoped, expiring session capabilities.
- Bound the total number of sessions, per-message size, and total queued signal
  bytes.

### Required Tests

- Browser-level test: hostile preview HTML cannot read the parent DOM, read
  `/api/meta`, obtain the CSRF token, call privileged APIs, open popups, or submit
  forms.
- Browser-level test: a valid multi-file folder preview renders successfully.
- An unapproved peer cannot inject a canvas signal or contact card.
- A forged `from` identity is rejected.
- Session-map and queue limits remain bounded under hostile input.

### Exit Criteria

- Previewed or remotely shared HTML has no path to the privileged loopback GUI
  origin.
- Live-share signaling obeys the same authenticated consent boundary as other
  direct communication.

## Phase 0B - Put External Website Deployment Behind Reviewed Egress

### Problems

- `publish_site` verifies only the password and directly uploads an arbitrary
  local folder. It bypasses the reviewed `EgressPlan`, lock registry, exact
  manifest comparison, one-shot authorization, quarantine checks, and security
  event discipline used by public publishing.
- `walk_files` follows symlinked files and directories. A symlink inside the
  selected folder can publish files outside that folder or recurse indefinitely.
- Only a few names are skipped. Secret-bearing files such as `.env.production`,
  `.npmrc`, credentials, and keys can be uploaded.
- There are no file-count, per-file-size, or total-byte limits; deployments load
  the full folder into memory.
- External receipts sign only the returned URL and use a generic
  `external:<platform>` root, so the receipt does not prove which files were
  published.

### Implementation

- Build a deterministic deployment manifest containing canonical relative paths,
  content digests, sizes, destination, and platform.
- Route external deployment through the reviewed egress authorization machinery.
  Recompute and compare the exact manifest while holding the policy lock before
  sending bytes.
- Reject every symlink and special file. Canonicalize the root and verify each
  file remains under it.
- Add explicit allow/deny rules for deployable content and surface excluded or
  sensitive files in the review.
- Enforce file-count, per-file-size, total-byte, recursion-depth, and memory
  limits; stream where the platform API permits it.
- Sign receipts over the exact manifest digest, destination, platform, and live
  URL.

### Required Tests

- A symlink to a file, directory, ancestor, or outside path is rejected.
- Secret-like files are excluded or block publication and appear in review.
- A folder or policy change after review invalidates the deployment.
- Locked or quarantined content cannot deploy.
- Size, count, and depth limits fail before network egress.
- Receipt verification proves the exact deployed manifest.

### Exit Criteria

- No external deployment can send bytes that were not explicitly and exactly
  reviewed under the existing egress policy.

## Phase 0C - Harden Deployment Credentials and Destinations

### Problems

- Production deployment functions honor `CONCIERGE_DEPLOY_<PLATFORM>_BASE`
  environment variables intended for tests. Those overrides can redirect stored
  bearer tokens to an attacker-controlled endpoint.
- Generic FTP sends usernames, passwords, and site content without transport
  encryption while the UI promises tokens go only to the selected platform API.
- GitHub deployment force-updates the configured branch and can overwrite
  concurrent or unrelated branch history.
- Deploy credential reads do not call the existing private-file validator before
  parsing the secret file.

### Implementation

- Make mock base URLs test-only or require an explicit development build/config
  that cannot be enabled accidentally in production.
- Remove plaintext FTP deployment. Use a verified FTPS/SFTP implementation or
  clearly separate it as an unsupported unsafe legacy option that is disabled by
  default.
- Replace force-updates with optimistic branch updates that fail on concurrent
  movement and require the user to review/retry.
- Validate ownership, permissions, and non-symlink status on every credential
  read.
- Zeroize secret-bearing values where practical and keep errors from echoing
  tokens or passwords.

### Required Tests

- Production builds cannot redirect credential-bearing requests through test
  environment overrides.
- Plaintext credential transport is impossible.
- Concurrent GitHub branch movement fails without rewriting history.
- Symlinked, permissive, or wrong-owner credential files fail closed.

### Exit Criteria

- Stored deployment secrets are sent only over authenticated encrypted transport
  to the exact reviewed destination.

## Phase 0D - Add Freshness, Limits, and Durable Naming State

### Problems

- Any older valid signed contact card can overwrite a newer cached card.
- Any older valid introduction can replace a newer one from the same introducer.
- Card and introduction fields have no practical size limits.
- Contact cards are accepted before the normal direct-message consent gate.
- Card, introduction, social, and checkpoint writes use direct or unlocked
  read-modify-write operations.
- Publish checkpoints use second-resolution filenames, so two publishes of one
  site in the same second can overwrite history; manifest write failures are
  ignored.

### Implementation

- Reject stale card and introduction updates using monotonic freshness rules and
  deterministic tie-breaking.
- Define and enforce field, card, introduction, and cached-entry limits.
- Require approved-contact or explicit invite policy before accepting profile
  metadata.
- Move naming and checkpoint state onto the locked atomic persistence helper from
  Phase 5.
- Give checkpoints collision-resistant identifiers and return persistence errors
  instead of silently claiming success.

### Required Tests

- Replayed older cards and introductions cannot roll back newer state.
- Oversized cards, fields, and cache floods are bounded.
- Unapproved peers cannot populate the card cache.
- Concurrent naming/checkpoint updates preserve every committed update.
- Two publishes in the same second create distinct restorable checkpoints.

### Exit Criteria

- Signed identity metadata is authenticated, fresh, bounded, consent-aware, and
  durably persisted.

## Phase 0E - Repository and Supply-Chain Hygiene

### Problems

- Large untracked reference trees sit at the repository root and expand the code
  audit surface from 56,848 to 130,734 physical code lines.
- `3mail-main/` contains ignored `.env` files and no license file was found in the
  copied tree during this audit.
- Bundled engines add about 1.8 MB to the MCP binary while MCP still has no tests.
- Documentation contains placeholder repository/image links and overstates
  security properties that the new deployment and preview paths do not satisfy.

### Implementation

- Keep reference checkouts outside the product repository, or document and
  deliberately vendor only the exact files required.
- Complete license, notice, provenance, checksum, and update-policy review before
  committing any imported or bundled code.
- Add size and supply-chain checks to CI.
- Update security and feature documentation only after the corresponding
  guarantees are enforced.

### Exit Criteria

- The committed repository contains only deliberate, attributable, reviewed
  product and vendored code.

## Revised Execution Order

1. Phase 0A - isolate untrusted previews and authenticate live-share signaling.
2. Phase 0B - put external deployment behind exact reviewed egress.
3. Phase 0C - harden credential destinations and remove plaintext FTP.
4. Original Phases 1 through 4 - network, pairing, Sidekick, and sync correctness.
5. Phase 0D and original Phase 5 - naming freshness and durable mutable state.
6. Original Phases 6 and 7 plus Phase 0E - quality gates, MCP coverage, and
   repository hygiene.
7. Original Phase 8 - decompose maintenance hotspots after correctness is stable.

Phases 0A through 0D and original Phases 1 through 5 are release blockers.

---

## Implementation Completion Addendum - June 12, 2026

### Completion Status

- **Completed:** Phases 0A through 0E and original Phases 1 through 7.
- **Partially completed:** Phase 8. Mutable-state, deployment, design, and naming
  responsibilities now have focused modules, but the largest GUI, binding, CLI,
  and network files still require a separate mechanical decomposition pass.
- The release-blocking correctness, security, persistence, deployment, and
  quality-gate findings from the audit and re-audit are fixed.

### Implemented Remediation

- Isolated Studio previews in opaque-origin sandboxed frames, rejected symlinked
  preview content, bounded live-share registries, and authenticated remote canvas
  and contact-card metadata against approved transport peers.
- Rebuilt external website deployment around deterministic reviewed manifests,
  exact destination binding, bounded safe file walking, post-review mutation
  detection, signed receipts, private credential validation, non-force GitHub
  updates, and HTTPS-only supported providers. Plaintext FTP is removed.
- Preserved unrelated network events during sync with per-request response
  channels. Direct messages now require explicit application acceptance before a
  delivery acknowledgement clears the sender's retry state.
- Made pairing offers atomically single-use, made sync-state commit failures
  observable, and separated Sidekick's private-node status/lifecycle from public
  publishing backends.
- Added locked atomic JSON persistence and migrated mutable contact, social,
  room, direct-message, site, sync-head, naming, and checkpoint state onto it.
- Added naming freshness and size limits, collision-safe site checkpoints, and
  deterministic content-derived blob CIDs.
- Added MCP JSON-RPC, framing, read-only, staging-only, resource, path, and
  malformed-input regression coverage.
- Added pinned Rust tooling, a committed lockfile path, CI quality/security
  gates, repository hygiene checks, bundled-engine size checks, embedded GUI
  JavaScript parsing, and ignored local reference checkouts.

### Verification Results

- `cargo test --locked --workspace`: **590 passed, 0 failed, 1 ignored**.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- Python adapter checks: **84 passed**; `python3 -m compileall -q adapters`
  passed.
- Embedded GUI JavaScript: **1 script parsed successfully**.
- `git diff --check`, locked dependency metadata, credential/reference-tree
  hygiene checks, and the 4 MiB bundled-engine cap: passed.
- The direct-message/sync acknowledgement regression passed five consecutive
  localhost libp2p runs. Pairing and merge race regressions also passed repeated
  runs.

### Final LOC Snapshot

- First-party product code, excluding `target/`, local reference checkouts,
  dependency trees, and virtual environments: **61,323 physical LOC**.
- All code physically present under the repository root with the same build and
  dependency exclusions, including ignored `3mail-main/` and `Planet-main/`
  reference checkouts: **135,829 physical LOC**.
- Remaining files above 2,000 LOC:
  - `crates/gui/src/lib.rs`: 6,562.
  - `crates/core/src/binding.rs`: 4,315.
  - `crates/gui/src/index.html`: 2,919.
  - `crates/cli/src/main.rs`: 2,884.
  - `crates/net/src/lib.rs`: 2,455.

### Remaining Structural Work

The code is materially safer, more deterministic, and better covered, but Phase
8's full maintenance exit criterion is not met. The five files above remain
large enough to slow review and increase change risk. Their decomposition should
be performed as independent mechanical refactors with no behavior changes and
the full verification gate after each split.

---

## Phase 8 Completion Addendum - June 13, 2026

### Completion Status

- **Completed:** Phase 8 and the full remediation plan.
- The five remaining maintenance hotspots were mechanically decomposed along
  existing domain boundaries without changing the public APIs.
- No first-party source file is above 2,000 physical lines.

### Structural Remediation

- Split `crates/core/src/binding.rs` into deployment, identity/naming,
  messaging, publication, wallet, and binding-test modules.
- Split `crates/net/src/lib.rs` into transport, sync orchestration, and network
  test modules while preserving the crate's public re-exports.
- Split CLI command handling into messaging, networking, publishing, and test
  modules.
- Split GUI Rust responsibilities into read routes, mutations, canvas/site
  preview, server/runtime, and tests.
- Extracted the GUI's CSS and JavaScript from `index.html` into `app.css`,
  `app.js`, `wallet.js`, and `studio.js`; updated server routes, regression
  coverage, and CI syntax checks for those assets.

### Final Verification Results

- `cargo check --workspace`: passed.
- `cargo test --locked --workspace`: **602 passed, 0 failed, 1 ignored**.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- Python adapter checks: **84 passed**; `python3 -m compileall -q adapters`
  passed.
- GUI JavaScript syntax: all three JavaScript assets passed `node --check`.
- Repository hygiene, locked dependency metadata, and `git diff --check`:
  passed.
- Direct-message/sync acknowledgement, pairing single-use, and concurrent-state
  race regressions each passed five consecutive runs.

### Final Size Snapshot

- First-party project code (`.rs`, `.py`, `.html`, `.js`, and `.css`, excluding
  ignored dependencies/build output): **63,525 physical LOC**.
- Largest remaining first-party source file:
  `crates/mem/src/audit.rs` at **1,676 LOC**.
- Former hotspots after decomposition:
  - `crates/gui/src/lib.rs`: 1,253 LOC.
  - `crates/core/src/binding.rs`: 1,479 LOC.
  - `crates/gui/src/index.html`: 297 LOC.
  - `crates/cli/src/main.rs`: 1,358 LOC.
  - `crates/net/src/lib.rs`: 64 LOC.

### Final Assessment

The codebase now meets the remediation plan's correctness, security, quality,
coverage, and maintainability gates. The remaining large files are focused
domain modules below the plan's 2,000-line hotspot threshold rather than
cross-responsibility runtime modules.
