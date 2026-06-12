//! Typed error model (Phase 1 deliverable, declared here in Phase 0).
//!
//! Callers must never have to parse error strings. Every failure the core
//! binding can produce is a distinct variant.

use thiserror::Error;

/// Result alias used throughout the core binding.
pub type Result<T> = std::result::Result<T, Error>;

/// Every failure mode the Concierge core binding can surface.
#[derive(Debug, Error)]
pub enum Error {
    /// The on-disk `.concierge` store could not be found or opened.
    #[error("concierge store not found or unreadable: {0}")]
    StoreNotFound(String),

    /// A name was looked up but is not bound to any CID.
    #[error("name is not bound: {0}")]
    NameUnbound(String),

    /// A CID was requested but no block with that CID exists in the store.
    #[error("cid not found: {0}")]
    CidNotFound(String),

    /// The record exists but has been tombstoned; callers should expect a
    /// receipt, not the original content.
    #[error("record is tombstoned: {0}")]
    Tombstoned(String),

    /// A configured publishing backend was unreachable (e.g. the user's local
    /// IPFS node is not running).
    #[error("backend unavailable: {0}")]
    BackendDown(String),

    /// The operation is not available over the current binding mechanism.
    /// Phase 1 shells out to the `mem` CLI, whose `put` is body-only (no edges,
    /// no CID-link fields), so edge/link/blob operations need the direct
    /// mem-crate-link binding the plan reserves as a later seam (Phase 1, Step 1).
    #[error("operation `{op}` is unsupported by the CLI binding: {reason}")]
    Unsupported {
        op: &'static str,
        reason: &'static str,
    },

    /// An egress was refused by the privacy-lock guard: the publication's
    /// reachable manifest intersects one or more locked subgraphs. Blocked
    /// before any bytes leave the process (Data Platter Privacy Lock, Phase B).
    #[error("{operation} blocked: this root reaches locked data - {summary}. Blockers: {blockers:?}. Review and authorize from the Data Platter.")]
    PublicationBlocked {
        operation: &'static str,
        summary: String,
        blockers: Vec<crate::egress::LockHit>,
    },

    /// A reviewed manifest or local policy changed before execution.
    #[error("egress plan changed before execution: {0}")]
    EgressPlanChanged(String),

    /// A security overlay path or file failed closed.
    #[error("security policy failure: {0}")]
    SecurityPolicy(String),

    /// The supplied store password did not verify.
    #[error("store password authentication failed")]
    AuthenticationFailed,

    /// Password verification is temporarily throttled after repeated failures.
    #[error("store password authentication rate limited; retry in {retry_after_secs} second(s)")]
    AuthenticationRateLimited { retry_after_secs: u64 },

    /// Public egress was refused because review found likely secrets.
    #[error("{operation} blocked by high-confidence sensitive-content findings: {findings:?}")]
    SensitiveContentBlocked {
        operation: &'static str,
        findings: Vec<String>,
    },

    /// A persisted grant failed authentication or replay checks.
    #[error("grant integrity failure: {0}")]
    GrantIntegrity(String),

    /// The legacy ambiguous share operation cannot publish.
    #[error("public publication requires the explicit publish-public operation")]
    ExplicitPublicPublishRequired,

    /// Capability-encryption (Cryptree) failure: build, decrypt, or vault. A
    /// wrong store password surfaces here as a closed vault (Phase E).
    #[error("capability encryption error: {0}")]
    Encryption(String),

    /// Wraps the underlying `mem` invocation or I/O failure.
    #[error("core binding I/O error: {0}")]
    Io(String),
}
