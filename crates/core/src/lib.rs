//! Universal Concierge Plugin — core.
//!
//! This crate defines two of the plan's layers and nothing else:
//!
//! - **Layer 1 — Concierge Core Binding** ([`binding`]): the small, stable API
//!   over the Concierge V4 `mem` memory layer (`put_node`, `bind`, `resolve`,
//!   `checkpoint`, `walk`, `gc`, …). Phase 0 only declares the trait and types;
//!   Phase 1 implements it against `mem`.
//! - **Layer 2 — Plugin Host API** ([`event`]): the host-neutral [`Event`] shape
//!   that every harness adapter translates its local events into. This is the
//!   public contract a Python (or any) harness emits — it never needs to
//!   understand DAG-CBOR, CIDs, CAR, or tombstones.
//!
//! Adapters (Layer 3) and the MCP server (Layer 4) live in their own crates and
//! depend on this one. Keeping them separate is a Phase 0 goal: the boundaries
//! between core, adapters, and MCP must be clear from day one.

pub mod actor;
pub mod binding;
pub mod brain;
pub mod browser;
pub mod capability;
pub mod car;
pub mod compiler;
pub mod config;
pub mod connectors;
pub mod contacts;
pub mod deploy;
pub mod design;
pub mod dm_outbox;
pub mod egress;
pub mod error;
pub mod event;
pub mod git_commit;
pub mod identity;
pub mod legibility;
pub mod membership;
pub mod merge;
pub mod messaging;
pub mod moderation;
pub mod naming;
pub mod node;
pub mod outbox;
pub mod pairing;
pub mod pinning;
pub mod private;
pub mod private_sync;
pub mod publishing;
pub mod recovery;
pub mod retrieval;
pub mod revocation;
pub mod security;
pub mod site;
pub mod sites;
pub mod social;
pub(crate) mod state;
pub mod sync;
pub mod synthesis;
pub mod update;
pub mod wallet;
pub mod youtube;

pub use actor::{verify_actor_certificate, ActorCertificate, ActorError, ACTOR_CERT_VERSION};
pub use binding::{
    cid_from_link, cid_link, utc_date, utc_today, Cid, CidOrName, CoreBinding, GcPolicy, GcReport,
    MemCli, Node, PublishReceipt, Record, StoreStats,
};
pub use brain::{BaselineStatus, BrainMetrics, BrainMetricsProvider, EmbedderStatus, RichMetrics};
pub use capability::{
    verify_capability, verify_capability_with_logs, Capability, CapabilityError, Namespace,
    NamespaceScope, Operation, CAPABILITY_VERSION,
};
pub use compiler::{ContextCompiler, TrustedAuthority};
pub use config::{
    BrainConfig, CheckpointConfig, Config, HostConfig, IdentityConfig, InjectionConfig,
    LibrarianConfig, StoreConfig, UpdateConfig, CONFIG_PATH,
};
pub use connectors::{
    federate, ConnectorRegistry, ExternalConnector, ExternalHit, ExternalSource,
    HttpIndexConnector, CONNECTORS_VERSION,
};
pub use egress::{
    EgressOperation, EgressPlan, EgressReceipt, LockHit, LockRecord, LockRegistry, SecurityEvent,
    LOCK_REGISTRY_VERSION, SECURITY_EVENT_VERSION,
};
pub use error::{Error, Result};
pub use event::{ContextSuggested, Envelope, Event, ImportedFrom, Reasoning, ReasoningSource};
pub use identity::{verify as verify_signature, AgentId, Identity};
pub use legibility::{message_trust_tier, social_gravity_factor, structural_importance, TrustTier};
pub use membership::{
    verify_membership, verify_membership_with_logs, ActorId, DeviceId, MembershipCertificate,
    MembershipError, NetworkDescriptor, NetworkId, RevocationRecord, RevocationSet, SubjectKind,
    UserId, DEFAULT_CERT_TTL_SECS, MEMBERSHIP_CERT_VERSION, NETWORK_DESCRIPTOR_VERSION,
    REVOCATION_RECORD_VERSION,
};
pub use merge::{
    HeadRelation, MergeCheckpoint, MergeError, MergeOutcome, MERGE_CHECKPOINT_VERSION,
};
pub use messaging::{message_order, MessageEnvelope, RoomBook, RoomPolicy};
pub use moderation::{
    verify_block, ExtensionPolicy, Guardian, QuarantineRecord, QuarantineRegistry, Verdict,
    YaraMatch, YaraScanReport, YaraScanner, DEFAULT_BLOCKED_EXTENSIONS,
};
pub use node::{
    kubo_binary, kubo_installed, launch_private_node, SidekickStatus, SIDEKICK_DISCLAIMER,
};
pub use outbox::{OutboundEvent, OutboxEntry, OUTBOX_FILE, OUTBOX_OFFSET_FILE};
pub use pairing::{
    approve, confirmation_phrase, verify_pairing_response, PairingError, PairingGrant,
    PairingOffer, PairingResponse, PAIRING_OFFER_VERSION, PAIRING_RESPONSE_VERSION,
};
pub use private::{
    EncryptedPrivateReceipt, PrivateCapability, PrivateConversion, PrivateSharePlan,
    PrivateShareResult, RecoveredPrivate, RotationResult,
};
pub use private_sync::{authorize_namespace_read, PrivateSyncError};
pub use publishing::{
    available_backends, selected_backend_reachable, BackendInfo, BackendRequirement,
};
pub use recovery::{
    verify_and_resolve, RecoveryError, ResolvedIdentity, UserIdentityLog, UserIdentityOp,
    UserOpKind, USER_IDENTITY_OP_VERSION,
};
#[cfg(feature = "semantic-embed")]
pub use retrieval::SemanticEmbedder;
pub use retrieval::{
    default_embedder, Depth, Embedder, HttpEmbedder, LexicalEmbedder, Librarian, RetrieveResult,
    Retrieved, SharedEmbedder,
};
pub use security::{Grant, GrantKind, GrantScope, GRANT_VERSION, PUBLISH_GRANT_TTL_SECS};
pub use social::SocialBook;
pub use sync::{
    reconcile, verify_head_record, HeadRecord, PullReport, Reconciliation, SyncError, SyncLimits,
    SyncReceipt, HEAD_RECORD_VERSION,
};
pub use synthesis::{
    assemble_thread, record_synthesis, synthesis_candidates, SynthesisCandidate,
    SYNTHESIS_THRESHOLD,
};
pub use update::{
    AppUpdateStatus, Freshness, Manifest, RefreshOutcome, ReleaseInfo, RulesChannel, RulesStatus,
    StagedUpdate, UpdateError,
};
pub use youtube::{
    UploadRequest, VideoMetadata, YouTubeCreds, YouTubeStatus, YouTubeUploadReceipt, YtError,
};
