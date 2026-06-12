//! Unified Memory Layer - an IPLD-backed, content-addressed store for all
//! durable agent state. See `UNIFIED_MEMORY_LAYER.md` for the full spec.

pub(crate) mod audit; // deterministic cross-file auditor (empty files, phantom imports)
pub mod backend; // Backend trait + manifest + registry (network plugins)
pub mod backends; // the network plugins themselves (feature-gated)
pub mod blockstore; // Blockstore trait + LocalBlocks
pub mod cid; // CIDv1 / dag-cbor / sha2-256 + re-export of the multiformats Cid
pub(crate) mod commands; // pure REPL command parsing
pub mod config; // TOML config: [trace], [model], backend env keys
pub mod dag; // reachable_from traversal
pub(crate) mod design; // deterministic frontend-design anti-pattern rules (ported from Impeccable)
pub(crate) mod diagnostics; // deterministic per-file syntax/structure checks
pub mod gc; // Phase 5.3 — retention/GC: trim the checkpoint chain, sweep orphans
pub mod hamt; // Phase A.5 — sharded content-addressed map (IPLD HAMT) for the day tier
pub mod ingest; // Phase 1.2 — filesystem to IPLD memory
pub mod ingestors; // Phase III — the ingest plugins
pub mod model; // minimal client for the one configured model
pub mod names; // NameIndex — the one mutable seam
pub mod node; // the typed node taxonomy + DAG-CBOR encode/decode
pub mod repl; // terminal REPL facade and command dispatch
pub(crate) mod session; // chat/work/checkpoint state machine behind the REPL
pub mod store; // put_node/get_node, generic sync/pin, wires blockstore + names
pub mod tombstones; // the GC death-certificate ledger (a receipt for every prune)
pub mod trace; // transparency — every river joint prints what moved
pub(crate) mod verifier; // sandboxed real-tool build/test verification
pub mod work; // whole-plan worker handoff helpers
