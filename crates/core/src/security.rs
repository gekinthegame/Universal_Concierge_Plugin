//! Data Platter Privacy Lock - store password + one-shot egress grants (Phase C).
//!
//! Phase B blocks every public egress whose manifest reaches a locked subgraph.
//! Phase C is the only way to *authorize* such an egress: the store owner proves
//! the **store password** and the system mints a short-lived, single-use
//! [`Grant`] bound to one exact reviewed [`EgressPlan`]. Execution
//! ([`MemCli::execute_approved_egress`]) consumes a matching grant atomically,
//! inside the cross-process policy lock, before any bytes leave.
//!
//! Secret handling follows the plan's non-negotiables: the password is hashed
//! with **scrypt** (a proven primitive, via the RustCrypto `scrypt`/`password-hash`
//! crates) behind a random salt; only the PHC verifier is stored, never the
//! password; verification is constant-time inside `password-hash`; failed
//! attempts are rate-limited with exponential backoff; and password buffers are
//! zeroized after use. There is **no password CLI argument, env var, or stored
//! plaintext** — the authorization surface is the local Data Platter (Phase D).
//!
//! On-disk grants live under `.concierge/security/unlock-grants/` (owner-only).
//! They are bound to an exact reviewed plan and Data Platter session, expire quickly,
//! and are authenticated by an owner-only local guard key. A stale or modified
//! grant can never silently authorize a later, different publication.

use std::collections::BTreeSet;
use std::path::PathBuf;

use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use scrypt::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use scrypt::Scrypt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::binding::{Cid, MemCli};
use crate::egress::{
    atomic_private_write, ensure_private_dir, hex, now_secs, security_io, validate_private_dir,
    validate_private_file, EgressOperation, EgressPlan,
};
use crate::error::{Error, Result};
use crate::private::PrivateSharePlan;

pub const PASSWORD_FILE_VERSION: u16 = 1;
pub const AUTH_STATE_VERSION: u16 = 1;
pub const GRANT_VERSION: u16 = 1;

/// How long a one-shot public-publish grant stays valid.
pub const PUBLISH_GRANT_TTL_SECS: u64 = 120;

/// Failed verifications allowed before exponential backoff begins.
const FREE_ATTEMPTS: u32 = 3;
/// Maximum backoff between attempts, in seconds.
const BACKOFF_CAP_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPassword {
    version: u16,
    /// scrypt PHC string (`$scrypt$...`): embeds salt + params + verifier.
    phc: String,
    created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthState {
    version: u16,
    failures: u32,
    last_attempt: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredGrant {
    grant: Grant,
    mac: String,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            version: AUTH_STATE_VERSION,
            failures: 0,
            last_attempt: 0,
        }
    }
}

/// What an unlock grant authorizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    /// One reviewed public egress of one exact manifest.
    Publish,
    /// One reviewed local plaintext-to-ciphertext conversion and capability
    /// handoff to one exact private namespace and recipient set.
    EncryptAndSharePrivate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantScope {
    Publish {
        plan_digest: String,
        root: String,
        manifest_digest: String,
        lock_registry_digest: String,
        backend: String,
        backend_target: String,
        network_posture: String,
        operation: EgressOperation,
    },
    EncryptAndSharePrivate {
        plan_digest: String,
        source_root: String,
        source_manifest_digest: String,
        lock_registry_digest: String,
        destination_namespace: String,
        recipients: Vec<String>,
    },
}

/// A short-lived, single-use authorization. Publish grants are bound to the
/// exact plan they were minted for; nothing about a grant is secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub version: u16,
    pub id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub session_id: String,
    pub scope: GrantScope,
}

impl Grant {
    fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at
    }

    pub fn kind(&self) -> GrantKind {
        match &self.scope {
            GrantScope::Publish { .. } => GrantKind::Publish,
            GrantScope::EncryptAndSharePrivate { .. } => GrantKind::EncryptAndSharePrivate,
        }
    }

    /// Whether this publish grant authorizes exactly `plan`.
    fn authorizes(&self, plan: &EgressPlan) -> Result<bool> {
        let current_digest = egress_plan_digest(plan)?;
        Ok(matches!(
            &self.scope,
            GrantScope::Publish {
                plan_digest,
                root,
                manifest_digest,
                lock_registry_digest,
                backend,
                backend_target,
                network_posture,
                operation,
            } if *plan_digest == current_digest
                && *operation == plan.operation
                && *root == plan.root.0
                && *manifest_digest == plan.manifest_digest
                && *lock_registry_digest == plan.lock_registry_digest
                && *backend == plan.backend
                && *backend_target == plan.backend_target
                && *network_posture == plan.network_posture
        ))
    }

    fn authorizes_private_share(&self, plan: &PrivateSharePlan) -> Result<bool> {
        let current_digest = egress_plan_digest(&plan.source)?;
        Ok(matches!(
            &self.scope,
            GrantScope::EncryptAndSharePrivate {
                plan_digest,
                source_root,
                source_manifest_digest,
                lock_registry_digest,
                destination_namespace,
                recipients,
            } if *plan_digest == current_digest
                && *source_root == plan.source.root.0
                && *source_manifest_digest == plan.source.manifest_digest
                && *lock_registry_digest == plan.source.lock_registry_digest
                && *destination_namespace == plan.destination_namespace
                && *recipients == plan.recipients
        ))
    }
}

pub(crate) fn egress_plan_digest(plan: &EgressPlan) -> Result<String> {
    let bytes = serde_json::to_vec(plan)
        .map_err(|error| Error::Io(format!("serialize egress plan digest: {error}")))?;
    Ok(hex(&Sha256::digest(bytes)))
}

impl MemCli {
    fn password_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("password.json"))
    }

    fn auth_state_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("auth-state.json"))
    }

    fn grants_dir(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("unlock-grants"))
    }

    fn guard_key_path(&self) -> Result<PathBuf> {
        Ok(self.security_dir()?.join("guard.key"))
    }

    /// Whether a store password has been set.
    pub fn password_is_set(&self) -> Result<bool> {
        let path = self.password_path()?;
        path.try_exists()
            .map_err(|e| security_io("inspect password file", e))
    }

    /// Create the store password (first-time setup). Refuses to overwrite an
    /// existing password — a reset must go through a deliberate recovery flow,
    /// never a silent re-set (plan: "Password reset requires a deliberate local
    /// recovery flow and must never silently unlock existing policy").
    pub fn set_password(&self, password: &str) -> Result<()> {
        let _policy_lock = self.policy_lock()?;
        self.set_password_unlocked(password)
    }

    fn set_password_unlocked(&self, password: &str) -> Result<()> {
        if self.password_is_set()? {
            return Err(Error::SecurityPolicy(
                "a store password is already set; use the recovery flow to change it".to_string(),
            ));
        }
        if password.is_empty() {
            return Err(Error::SecurityPolicy(
                "store password must not be empty".to_string(),
            ));
        }
        ensure_private_dir(&self.security_dir()?)?;
        let secret = Zeroizing::new(password.as_bytes().to_vec());
        let mut salt_bytes = [0u8; 16];
        OsRng.fill_bytes(&mut salt_bytes);
        let salt = SaltString::encode_b64(&salt_bytes)
            .map_err(|e| Error::SecurityPolicy(format!("encode salt: {e}")))?;
        // Versioned, explicit scrypt params (log_n=15, r=8, p=1, 32-byte key) —
        // the Wuala Cryptree parameters from CRYPTREE_PORT_SPEC.md. The PHC string
        // embeds these, so verification needs no out-of-band params.
        let params = scrypt::Params::new(15, 8, 1, 32)
            .map_err(|e| Error::SecurityPolicy(format!("scrypt params: {e}")))?;
        let phc = Scrypt
            .hash_password_customized(&secret, None, None, params, salt.as_salt())
            .map_err(|e| Error::SecurityPolicy(format!("hash password: {e}")))?
            .to_string();
        let stored = StoredPassword {
            version: PASSWORD_FILE_VERSION,
            phc,
            created_at: now_secs(),
        };
        let bytes = serde_json::to_vec_pretty(&stored)
            .map_err(|e| Error::Io(format!("serialize password file: {e}")))?;
        atomic_private_write(&self.password_path()?, &bytes)?;
        self.append_security_event_unlocked(
            "password_set",
            &Cid(String::new()),
            "store password set",
        )
    }

    fn load_stored_password(&self) -> Result<StoredPassword> {
        let path = self.password_path()?;
        if !path
            .try_exists()
            .map_err(|e| security_io("inspect password file", e))?
        {
            return Err(Error::SecurityPolicy(
                "no store password is set; set one from the Data Platter first".to_string(),
            ));
        }
        validate_private_file(&path)?;
        let text =
            std::fs::read_to_string(&path).map_err(|e| Error::Io(format!("read password: {e}")))?;
        let stored: StoredPassword = serde_json::from_str(&text)
            .map_err(|e| Error::SecurityPolicy(format!("parse password file: {e}")))?;
        if stored.version != PASSWORD_FILE_VERSION {
            return Err(Error::SecurityPolicy(format!(
                "unsupported password file version {}",
                stored.version
            )));
        }
        Ok(stored)
    }

    fn load_auth_state(&self) -> Result<AuthState> {
        let path = self.auth_state_path()?;
        if !path
            .try_exists()
            .map_err(|e| security_io("inspect auth state", e))?
        {
            return Ok(AuthState::default());
        }
        validate_private_file(&path)?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Io(format!("read auth state: {e}")))?;
        let state: AuthState = serde_json::from_str(&text)
            .map_err(|e| Error::SecurityPolicy(format!("parse auth state: {e}")))?;
        if state.version != AUTH_STATE_VERSION {
            return Err(Error::SecurityPolicy(format!(
                "unsupported auth state version {}",
                state.version
            )));
        }
        Ok(state)
    }

    fn save_auth_state(&self, state: &AuthState) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| Error::Io(format!("serialize auth state: {e}")))?;
        atomic_private_write(&self.auth_state_path()?, &bytes)
    }

    /// Verify the store password. Constant-time inside `password-hash`, gated by
    /// exponential backoff after repeated failures, and resets the failure
    /// counter on success. Returns `Ok(())` only on a correct password.
    pub fn verify_password(&self, password: &str) -> Result<()> {
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)
    }

    pub(crate) fn verify_password_unlocked(&self, password: &str) -> Result<()> {
        let stored = self.load_stored_password()?;
        let mut state = self.load_auth_state()?;
        let now = now_secs();

        if state.failures >= FREE_ATTEMPTS {
            let wait = backoff_secs(state.failures);
            let ready_at = state.last_attempt.saturating_add(wait);
            if now < ready_at {
                return Err(Error::AuthenticationRateLimited {
                    retry_after_secs: ready_at - now,
                });
            }
        }

        let secret = Zeroizing::new(password.as_bytes().to_vec());
        let parsed = PasswordHash::new(&stored.phc)
            .map_err(|e| Error::SecurityPolicy(format!("parse stored verifier: {e}")))?;
        let ok = Scrypt.verify_password(&secret, &parsed).is_ok();

        if ok {
            if state.failures != 0 {
                state.failures = 0;
                state.last_attempt = now;
                self.save_auth_state(&state)?;
            }
            Ok(())
        } else {
            state.failures = state.failures.saturating_add(1);
            state.last_attempt = now;
            self.save_auth_state(&state)?;
            self.append_security_event_unlocked(
                "auth_failed",
                &Cid(String::new()),
                &format!("failed attempt #{}", state.failures),
            )?;
            Err(Error::AuthenticationFailed)
        }
    }

    fn load_or_create_guard_key_unlocked(&self) -> Result<Zeroizing<Vec<u8>>> {
        let path = self.guard_key_path()?;
        if path
            .try_exists()
            .map_err(|error| security_io("inspect guard key", error))?
        {
            validate_private_file(&path)?;
            let key = std::fs::read(&path)
                .map_err(|error| Error::Io(format!("read guard key: {error}")))?;
            if key.len() != 32 {
                return Err(Error::SecurityPolicy(
                    "guard key must be exactly 32 bytes".to_string(),
                ));
            }
            return Ok(Zeroizing::new(key));
        }
        let mut key = Zeroizing::new(vec![0u8; 32]);
        OsRng.fill_bytes(&mut key);
        atomic_private_write(&path, &key)?;
        Ok(key)
    }

    fn grant_mac_unlocked(&self, grant: &Grant) -> Result<String> {
        let key = self.load_or_create_guard_key_unlocked()?;
        let bytes = serde_json::to_vec(grant)
            .map_err(|error| Error::Io(format!("serialize grant MAC: {error}")))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&key)
            .map_err(|error| Error::SecurityPolicy(format!("initialize grant MAC: {error}")))?;
        mac.update(&bytes);
        Ok(hex(&mac.finalize().into_bytes()))
    }

    fn verify_grant_mac_unlocked(&self, stored: &StoredGrant) -> Result<()> {
        let key = self.load_or_create_guard_key_unlocked()?;
        let bytes = serde_json::to_vec(&stored.grant)
            .map_err(|error| Error::Io(format!("serialize grant MAC: {error}")))?;
        let expected = decode_hex(&stored.mac)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&key)
            .map_err(|error| Error::SecurityPolicy(format!("initialize grant MAC: {error}")))?;
        mac.update(&bytes);
        mac.verify_slice(&expected)
            .map_err(|_| Error::GrantIntegrity("MAC verification failed".to_string()))
    }

    #[cfg(test)]
    fn write_grant(&self, grant: &Grant) -> Result<()> {
        let _policy_lock = self.policy_lock()?;
        self.write_grant_unlocked(grant)
    }

    fn write_grant_unlocked(&self, grant: &Grant) -> Result<()> {
        ensure_private_dir(&self.grants_dir()?)?;
        let stored = StoredGrant {
            grant: grant.clone(),
            mac: self.grant_mac_unlocked(grant)?,
        };
        let bytes = serde_json::to_vec_pretty(&stored)
            .map_err(|e| Error::Io(format!("serialize grant: {e}")))?;
        atomic_private_write(
            &self.grants_dir()?.join(format!("{}.json", grant.id)),
            &bytes,
        )
    }

    fn random_grant_id() -> String {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        hex(&bytes)
    }

    /// Mint a one-shot grant authorizing exactly `reviewed`, after verifying the
    /// store password. Re-derives the live plan and refuses if anything changed
    /// since review, so a grant can never be bound to a stale manifest.
    pub fn create_publish_grant(&self, reviewed: &EgressPlan, password: &str) -> Result<Grant> {
        if !reviewed.operation.is_public() {
            return Err(Error::Unsupported {
                op: "grant",
                reason: "only public egress operations can be authorized by a grant",
            });
        }
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let target = reviewed
            .resolved_name
            .clone()
            .unwrap_or_else(|| reviewed.root.0.clone());
        let current = self.build_egress_plan_for_target_and_backend(
            &target,
            reviewed.operation,
            &reviewed.backend,
            &reviewed.backend_target,
            &reviewed.network_posture,
        )?;
        if current != *reviewed {
            return Err(Error::EgressPlanChanged(
                "plan changed since it was reviewed; re-review before authorizing".to_string(),
            ));
        }
        if !current.is_blocked() {
            return Err(Error::SecurityPolicy(
                "a public-publish grant is only valid for a lock-blocked plan".to_string(),
            ));
        }
        let sensitive = current.blocking_sensitive_findings();
        if !sensitive.is_empty() {
            return Err(Error::SensitiveContentBlocked {
                operation: current.operation.label(),
                findings: sensitive.into_iter().map(str::to_string).collect(),
            });
        }
        let now = now_secs();
        let grant = Grant {
            version: GRANT_VERSION,
            id: Self::random_grant_id(),
            created_at: now,
            expires_at: now.saturating_add(PUBLISH_GRANT_TTL_SECS),
            session_id: self.security_session_id.clone(),
            scope: GrantScope::Publish {
                plan_digest: egress_plan_digest(&current)?,
                root: current.root.0.clone(),
                manifest_digest: current.manifest_digest.clone(),
                lock_registry_digest: current.lock_registry_digest.clone(),
                backend: current.backend.clone(),
                backend_target: current.backend_target.clone(),
                network_posture: current.network_posture.clone(),
                operation: current.operation,
            },
        };
        self.write_grant_unlocked(&grant)?;
        self.append_security_event_unlocked(
            "publish_grant_created",
            &current.root,
            &format!("{} via {}", current.operation.label(), current.backend),
        )?;
        Ok(grant)
    }

    /// Mint the exact one-shot grant required by the reviewed Data Platter
    /// convert-and-share-private flow.
    pub fn create_encrypt_and_share_private_grant(
        &self,
        reviewed: &PrivateSharePlan,
        password: &str,
    ) -> Result<Grant> {
        if reviewed.source.operation != EgressOperation::EncryptAndSharePrivate {
            return Err(Error::SecurityPolicy(
                "private-share grant requires an encrypt-and-share-private source plan".to_string(),
            ));
        }
        let _policy_lock = self.policy_lock()?;
        self.verify_password_unlocked(password)?;
        let current = self.build_encrypt_and_share_plan(
            reviewed
                .source
                .resolved_name
                .as_deref()
                .unwrap_or(&reviewed.source.root.0),
            &reviewed.destination_namespace,
            &reviewed.recipients,
        )?;
        if current != *reviewed {
            return Err(Error::EgressPlanChanged(
                "private-share plan changed since review".to_string(),
            ));
        }
        let now = now_secs();
        let grant = Grant {
            version: GRANT_VERSION,
            id: Self::random_grant_id(),
            created_at: now,
            expires_at: now.saturating_add(PUBLISH_GRANT_TTL_SECS),
            session_id: self.security_session_id.clone(),
            scope: GrantScope::EncryptAndSharePrivate {
                plan_digest: egress_plan_digest(&current.source)?,
                source_root: current.source.root.0.clone(),
                source_manifest_digest: current.source.manifest_digest.clone(),
                lock_registry_digest: current.source.lock_registry_digest.clone(),
                destination_namespace: current.destination_namespace.clone(),
                recipients: current.recipients.clone(),
            },
        };
        self.write_grant_unlocked(&grant)?;
        self.append_security_event_unlocked(
            "encrypt_and_share_private_grant_created",
            &current.source.root,
            &format!(
                "namespace {} for {} recipient(s)",
                current.destination_namespace,
                current.recipients.len()
            ),
        )?;
        Ok(grant)
    }

    #[cfg(test)]
    fn load_grants(&self) -> Result<Vec<(PathBuf, Grant)>> {
        let _policy_lock = self.policy_lock()?;
        self.load_grants_unlocked()
    }

    fn load_grants_unlocked(&self) -> Result<Vec<(PathBuf, Grant)>> {
        let dir = self.grants_dir()?;
        if !dir
            .try_exists()
            .map_err(|e| security_io("inspect grants dir", e))?
        {
            return Ok(Vec::new());
        }
        validate_private_dir(&dir)?;
        let mut grants = Vec::new();
        let mut seen_ids = BTreeSet::new();
        for entry in
            std::fs::read_dir(&dir).map_err(|e| Error::Io(format!("read grants dir: {e}")))?
        {
            let path = entry
                .map_err(|e| Error::Io(format!("read grant entry: {e}")))?
                .path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            validate_private_file(&path)?;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| Error::Io(format!("read grant: {e}")))?;
            // A grant file that no longer deserializes (e.g. a legacy view-scope
            // grant after that scope was removed) cannot authorize anything, so it
            // is fail-safe to drop it rather than brick the whole grants dir.
            let stored: StoredGrant = match serde_json::from_str(&text) {
                Ok(stored) => stored,
                Err(_) => {
                    std::fs::remove_file(&path)
                        .map_err(|error| Error::Io(format!("clear unreadable grant: {error}")))?;
                    continue;
                }
            };
            self.verify_grant_mac_unlocked(&stored)?;
            let grant = stored.grant;
            if grant.version != GRANT_VERSION {
                return Err(Error::GrantIntegrity(format!(
                    "unsupported grant version {}",
                    grant.version
                )));
            }
            if grant.session_id != self.security_session_id {
                std::fs::remove_file(&path)
                    .map_err(|error| Error::Io(format!("clear stale grant: {error}")))?;
                continue;
            }
            let expected_name = format!("{}.json", grant.id);
            if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
                return Err(Error::GrantIntegrity(
                    "grant filename does not match its authenticated id".to_string(),
                ));
            }
            if !seen_ids.insert(grant.id.clone()) {
                return Err(Error::GrantIntegrity(
                    "duplicate grant id detected; refusing possible replay".to_string(),
                ));
            }
            grants.push((path, grant));
        }
        Ok(grants)
    }


    /// Delete every grant. The Data Platter calls this on startup so grants are
    /// ephemeral across restarts (plan: "Restart clears ephemeral grants").
    pub fn clear_all_grants(&self) -> Result<()> {
        let _policy_lock = self.policy_lock()?;
        for (path, _) in self.load_grants_unlocked()? {
            std::fs::remove_file(&path).map_err(|e| Error::Io(format!("remove grant: {e}")))?;
        }
        Ok(())
    }

    /// Remove expired grants; returns how many were purged.
    pub fn purge_expired_grants(&self) -> Result<usize> {
        let _policy_lock = self.policy_lock()?;
        let now = now_secs();
        let mut purged = 0;
        for (path, grant) in self.load_grants_unlocked()? {
            if grant.is_expired(now) {
                std::fs::remove_file(&path)
                    .map_err(|e| Error::Io(format!("remove expired grant: {e}")))?;
                purged += 1;
            }
        }
        Ok(purged)
    }

    /// Find and atomically consume a one-shot publish grant authorizing exactly
    /// `plan`. MUST be called inside the policy lock (it is, via
    /// `execute_approved_egress`). Expired grants are purged in passing. Returns
    /// whether a valid grant was consumed.
    fn consume_matching_publish_grant(&self, plan: &EgressPlan) -> Result<bool> {
        let now = now_secs();
        for (path, grant) in self.load_grants_unlocked()? {
            if grant.is_expired(now) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            if grant.authorizes(plan)? {
                // Consume before the egress runs (single-use, fail-safe: a failed
                // publish does not leave a reusable grant).
                std::fs::remove_file(&path)
                    .map_err(|e| Error::Io(format!("consume grant: {e}")))?;
                self.append_security_event_unlocked(
                    "publish_grant_consumed",
                    &plan.root,
                    &format!(
                        "{} via {} (grant {})",
                        plan.operation.label(),
                        plan.backend,
                        grant.id
                    ),
                )?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn consume_matching_private_share_grant_unlocked(
        &self,
        plan: &PrivateSharePlan,
    ) -> Result<bool> {
        let now = now_secs();
        for (path, grant) in self.load_grants_unlocked()? {
            if grant.is_expired(now) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            if grant.authorizes_private_share(plan)? {
                std::fs::remove_file(&path)
                    .map_err(|error| Error::Io(format!("consume private-share grant: {error}")))?;
                self.append_security_event_unlocked(
                    "encrypt_and_share_private_grant_consumed",
                    &plan.source.root,
                    &format!("grant {}", grant.id),
                )?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The execution-time authorization gate. Private ciphertext replication
    /// must match its registered private posture, conversion must use its
    /// dedicated grant flow, and public lock bypass consumes an exact one-shot
    /// publish grant.
    pub(crate) fn authorize_egress(&self, plan: &EgressPlan) -> Result<()> {
        if plan.operation == EgressOperation::PrivateEncryptedReplicate {
            return self.check_private_replication_posture(plan);
        }
        if plan.operation == EgressOperation::EncryptAndSharePrivate {
            return Err(Error::SecurityPolicy(
                "encrypt-and-share-private must execute through the exact conversion grant flow"
                    .to_string(),
            ));
        }
        let sensitive = plan.blocking_sensitive_findings();
        if plan.operation.is_public() && !sensitive.is_empty() {
            return Err(Error::SensitiveContentBlocked {
                operation: plan.operation.label(),
                findings: sensitive.into_iter().map(str::to_string).collect(),
            });
        }
        if !plan.is_blocked() {
            return Ok(());
        }
        if self.consume_matching_publish_grant(plan)? {
            return Ok(());
        }
        Err(Error::PublicationBlocked {
            operation: plan.operation.label(),
            summary: plan.blocker_summary(),
            blockers: plan.blocking_locks.clone(),
        })
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(Error::GrantIntegrity(
            "grant MAC is not valid hex".to_string(),
        ));
    }
    (0..value.len())
        .step_by(2)
        .map(|offset| {
            u8::from_str_radix(&value[offset..offset + 2], 16)
                .map_err(|_| Error::GrantIntegrity("grant MAC is not valid hex".to_string()))
        })
        .collect()
}

/// Exponential backoff in seconds once failures exceed the free-attempt grace.
fn backoff_secs(failures: u32) -> u64 {
    if failures < FREE_ATTEMPTS {
        return 0;
    }
    let shift = failures - FREE_ATTEMPTS;
    let scaled = 1u64.checked_shl(shift).unwrap_or(BACKOFF_CAP_SECS);
    scaled.min(BACKOFF_CAP_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::{CoreBinding, Node};
    use crate::egress::EgressOperation;
    use std::sync::{Arc, Barrier};

    fn temp_workdir(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "concierge-security-{tag}-{}-{}",
            std::process::id(),
            crate::egress::now_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn memory_node(text: &str) -> Node {
        Node {
            kind: "memory".to_string(),
            fields_json: format!(r#"{{"text":"{text}","kind":"project"}}"#),
        }
    }

    #[test]
    fn password_sets_once_and_verifies() {
        let dir = temp_workdir("password");
        let mem = MemCli::new(&dir);
        assert!(!mem.password_is_set().unwrap());
        mem.set_password("correct horse battery staple").unwrap();
        assert!(mem.password_is_set().unwrap());
        // Cannot silently re-set.
        assert!(matches!(
            mem.set_password("another"),
            Err(Error::SecurityPolicy(_))
        ));
        mem.verify_password("correct horse battery staple").unwrap();
        assert!(matches!(
            mem.verify_password("wrong"),
            Err(Error::AuthenticationFailed)
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plaintext_password_never_enters_events_grants_car_or_receipts() {
        let dir = temp_workdir("password-leakage");
        let mem = MemCli::new(&dir);
        let password = "phase-c-unique-plaintext-password";
        mem.set_password(password).unwrap();
        let grant_plan = locked_plan(&mem);
        mem.create_publish_grant(&grant_plan, password).unwrap();
        let wrong = "phase-c-unique-wrong-password";
        let error = mem.verify_password(wrong).unwrap_err().to_string();
        let root = mem.put_node(&memory_node("ordinary content")).unwrap();
        let car = mem.export_car(&root).unwrap();

        assert!(!String::from_utf8_lossy(&car).contains(password));
        assert!(!error.contains(wrong));
        assert!(!serde_json::to_string(&mem.security_events().unwrap())
            .unwrap()
            .contains(password));
        assert!(!serde_json::to_string(&mem.load_grants().unwrap())
            .unwrap()
            .contains(password));
        assert!(!std::fs::read_to_string(mem.password_path().unwrap())
            .unwrap()
            .contains(password));
        assert!(mem.publish_receipts().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_attempts_increment_and_correct_password_resets() {
        let dir = temp_workdir("backoff-reset");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        // One failure (under the grace threshold, so no wait imposed).
        assert!(mem.verify_password("nope").is_err());
        assert_eq!(mem.load_auth_state().unwrap().failures, 1);
        // Correct password resets the counter.
        mem.verify_password("pw").unwrap();
        assert_eq!(mem.load_auth_state().unwrap().failures, 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rate_limit_blocks_after_repeated_failures() {
        let dir = temp_workdir("rate-limit");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        // Simulate the failure counter being at the backoff threshold, just now.
        mem.save_auth_state(&AuthState {
            version: AUTH_STATE_VERSION,
            failures: FREE_ATTEMPTS + 2,
            last_attempt: now_secs(),
        })
        .unwrap();
        // Even the *correct* password is refused while the backoff window is open.
        assert!(matches!(
            mem.verify_password("pw"),
            Err(Error::AuthenticationRateLimited { .. })
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_password_failures_are_counted_without_lost_updates() {
        let dir = temp_workdir("concurrent-rate-limit");
        let mem = Arc::new(MemCli::new(&dir));
        mem.set_password("pw").unwrap();
        let barrier = Arc::new(Barrier::new(FREE_ATTEMPTS as usize));
        let handles = (0..FREE_ATTEMPTS)
            .map(|_| {
                let mem = Arc::clone(&mem);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    assert!(mem.verify_password("wrong").is_err());
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(mem.load_auth_state().unwrap().failures, FREE_ATTEMPTS);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn concurrent_first_password_setup_has_exactly_one_winner() {
        let dir = temp_workdir("concurrent-password-setup");
        let mem = Arc::new(MemCli::new(&dir));
        let barrier = Arc::new(Barrier::new(2));
        let handles = ["first", "second"].map(|password| {
            let mem = Arc::clone(&mem);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                mem.set_password(password)
            })
        });
        let successes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(Result::is_ok)
            .count();
        assert_eq!(successes, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn locked_plan(mem: &MemCli) -> EgressPlan {
        let root = mem.put_node(&memory_node("secret")).unwrap();
        mem.lock_subgraph(&root, "private").unwrap();
        mem.build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap()
    }

    #[test]
    fn grant_authorizes_exactly_one_matching_egress() {
        let dir = temp_workdir("grant-oneshot");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        assert!(plan.is_blocked());

        // No grant yet -> blocked.
        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::PublicationBlocked { .. })
        ));

        // Mint a grant, then exactly one egress is authorized.
        let _grant = mem.create_publish_grant(&plan, "pw").unwrap();
        let mut runs = 0;
        mem.execute_approved_egress(&plan, |_| {
            runs += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(runs, 1);

        // The grant is consumed: a second attempt is blocked again.
        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::PublicationBlocked { .. })
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn grant_requires_correct_password() {
        let dir = temp_workdir("grant-wrong-pw");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        assert!(matches!(
            mem.create_publish_grant(&plan, "WRONG"),
            Err(Error::AuthenticationFailed)
        ));
        // No grant was written.
        assert!(mem.load_grants().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clear_plan_does_not_create_an_unconsumable_grant() {
        let dir = temp_workdir("grant-clear-plan");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let root = mem.put_node(&memory_node("public")).unwrap();
        // Cleared for egress (Decision 0026) so the plan is unblocked — a clear
        // plan needs no one-shot grant.
        mem.clear_for_egress(&root, "public", "pw").unwrap();
        let plan = mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap();
        assert!(!plan.is_blocked());
        assert!(matches!(
            mem.create_publish_grant(&plan, "pw"),
            Err(Error::SecurityPolicy(_))
        ));
        assert!(mem.load_grants().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn expired_grant_does_not_authorize() {
        let dir = temp_workdir("grant-expired");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        let mut grant = mem.create_publish_grant(&plan, "pw").unwrap();
        // Force expiry and rewrite.
        grant.expires_at = 1;
        mem.write_grant(&grant).unwrap();
        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::PublicationBlocked { .. })
        ));
        // Consumption purged the expired grant.
        assert!(mem.purge_expired_grants().unwrap() <= 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn grant_is_bound_to_the_exact_manifest() {
        let dir = temp_workdir("grant-bound");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let root = mem.put_node(&memory_node("secret")).unwrap();
        mem.lock_subgraph(&root, "private").unwrap();
        let plan = mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap();
        let grant = mem.create_publish_grant(&plan, "pw").unwrap();
        // A grant for a public publish must not authorize a plaintext export of
        // the same root (different operation/backend).
        let export_plan = mem
            .build_egress_plan(&root, EgressOperation::PlaintextCarExport)
            .unwrap();
        assert!(!grant.authorizes(&export_plan).unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lock_registry_change_invalidates_an_existing_grant() {
        let dir = temp_workdir("grant-lock-registry");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        mem.create_publish_grant(&plan, "pw").unwrap();

        let unrelated = mem.put_node(&memory_node("unrelated")).unwrap();
        mem.lock_subgraph(&unrelated, "new unrelated lock").unwrap();
        let current = mem
            .build_egress_plan(&plan.root, EgressOperation::PublicPublish)
            .unwrap();
        assert_ne!(plan.lock_registry_digest, current.lock_registry_digest);
        assert!(matches!(
            mem.execute_approved_egress(&current, |_| Ok(())),
            Err(Error::PublicationBlocked { .. })
        ));
        assert_eq!(mem.load_grants().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn modified_grant_fails_integrity_verification() {
        let dir = temp_workdir("grant-tamper");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        let grant = mem.create_publish_grant(&plan, "pw").unwrap();
        let path = mem.grants_dir().unwrap().join(format!("{}.json", grant.id));
        let mut stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        stored["grant"]["scope"]["root"] = serde_json::Value::String("bafy-tampered".to_string());
        std::fs::write(&path, serde_json::to_vec_pretty(&stored).unwrap()).unwrap();

        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::GrantIntegrity(message)) if message.contains("MAC verification failed")
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn copied_grant_file_fails_closed_as_a_replay() {
        let dir = temp_workdir("grant-replay");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        let grant = mem.create_publish_grant(&plan, "pw").unwrap();
        let grants_dir = mem.grants_dir().unwrap();
        std::fs::copy(
            grants_dir.join(format!("{}.json", grant.id)),
            grants_dir.join("replayed.json"),
        )
        .unwrap();

        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::GrantIntegrity(message)) if message.contains("filename does not match")
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_one_racing_publish_can_consume_a_grant() {
        let dir = temp_workdir("grant-race");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        mem.create_publish_grant(&plan, "pw").unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let handles = [mem.clone(), mem.clone()].map(|mem| {
            let plan = plan.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                mem.execute_approved_egress(&plan, |_| Ok(()))
            })
        });
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(Error::PublicationBlocked { .. })))
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fresh_session_cannot_use_or_retain_an_old_grant() {
        let dir = temp_workdir("grant-restart");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        mem.create_publish_grant(&plan, "pw").unwrap();

        let restarted = MemCli::new(&dir);
        let current = restarted
            .build_egress_plan(&plan.root, EgressOperation::PublicPublish)
            .unwrap();
        assert!(matches!(
            restarted.execute_approved_egress(&current, |_| Ok(())),
            Err(Error::PublicationBlocked { .. })
        ));
        assert!(restarted.load_grants().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn high_confidence_sensitive_findings_block_public_authorization() {
        let dir = temp_workdir("sensitive-block");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let content = mem.put_blob(b"secret", "text/plain").unwrap();
        let root = mem
            .put_node(&Node {
                kind: "file_ref".to_string(),
                fields_json: serde_json::json!({
                    "path": ".env",
                    "size": 6,
                    "content": crate::binding::cid_link(&content).unwrap(),
                })
                .to_string(),
            })
            .unwrap();
        mem.lock_subgraph(&root, "private").unwrap();
        let plan = mem
            .build_egress_plan(&root, EgressOperation::PublicPublish)
            .unwrap();
        assert!(!plan.blocking_sensitive_findings().is_empty());
        assert!(matches!(
            mem.create_publish_grant(&plan, "pw"),
            Err(Error::SensitiveContentBlocked { .. })
        ));
        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::SensitiveContentBlocked { .. })
        ));
        let _ = std::fs::remove_dir_all(dir);
    }


    #[test]
    fn clear_all_grants_makes_them_ephemeral() {
        let dir = temp_workdir("clear-grants");
        let mem = MemCli::new(&dir);
        mem.set_password("pw").unwrap();
        let plan = locked_plan(&mem);
        mem.create_publish_grant(&plan, "pw").unwrap();
        assert_eq!(mem.load_grants().unwrap().len(), 1);
        mem.clear_all_grants().unwrap();
        assert!(mem.load_grants().unwrap().is_empty());
        assert!(matches!(
            mem.execute_approved_egress(&plan, |_| Ok(())),
            Err(Error::PublicationBlocked { .. })
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
