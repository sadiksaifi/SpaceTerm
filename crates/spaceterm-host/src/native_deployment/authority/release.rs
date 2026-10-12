use std::{sync::Arc, time::Instant};

use super::{NativeTransactionLease, NativeTransactionPeer, NativeTransactions};
use crate::{
    approved_installation::ApprovedInstallationDescriptor,
    native_deployment::{
        account::AccountBinding,
        contracts::{AccountBuildKey, NativeFailure, nonzero},
        profile::BootCredentialBinding,
    },
    startup::{
        PendingStartupContext, StartupInspection, StartupPreflight, StartupSnapshot,
        inspect_startup_snapshot,
    },
};

/// Independently retained post-exec, installation and process authority.
/// The production peer retains native process handles and the exact admitted execution closure.
/// Readiness bits, context fields, hashes and PID names cannot implement this observation.
pub trait NativeReleaseValidation: Send + Sync {
    /// Exact pending context observed by the native issuer, not copied from a wire frame.
    fn context(&self) -> &PendingStartupContext;
    /// Descriptor of the independently retained approved execution, authenticated by the head.
    fn descriptor(&self) -> &ApprovedInstallationDescriptor;
    /// Original absolute exchange deadline, not a fresh deadline on each recheck.
    fn deadline(&self) -> Instant;
    /// Revalidates retained process incarnation, final privileges, custody, and execution closure.
    fn revalidate_execution(&self) -> Result<(), NativeFailure>;
    /// Independently observes current native account incarnation and primary GID.
    fn observe_account(&self) -> Result<AccountBinding, NativeFailure>;
    /// Revalidates support policy and registered storage without reading or creating its underlay.
    fn check_storage_and_policy(&self, snapshot: &StartupSnapshot) -> Result<(), NativeFailure>;
}

/// Native post-exec issuer. Tests inject the native peer, never a fabricated production proof.
pub trait NativeReleasePeer: NativeTransactionPeer {
    /// Production uses PendingStartup, retaining registration, profile and approved execution.
    type Pending;
    /// Production uses independently observed PostExecProof.
    type Proof;
    /// Binds the proof to the pending authority and retains native bindings through delivery.
    fn retain_post_exec(
        &self,
        pending: &Self::Pending,
        proof: &Self::Proof,
    ) -> Result<Box<dyn NativeReleaseValidation>, NativeFailure>;
}

/// Noncloneable live release fence. Stop and activation use these same native lock inodes.
/// Credential adapters must retain this lease and recheck before retrieval, after retrieval,
/// before delivery and before acknowledgement. Dropping it releases the fence, never earlier.
pub struct NativeReleaseLease {
    transaction: NativeTransactionLease,
    native: Box<dyn NativeReleaseValidation>,
    credential: BootCredentialBinding,
}
impl std::fmt::Debug for NativeReleaseLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeReleaseLease(..)")
    }
}
impl NativeReleaseLease {
    /// Returns exact bounded comparison data for the startup transcript.
    pub fn context(&self) -> &PendingStartupContext {
        self.native.context()
    }
    /// Returns only the fixed credential authenticated by the protected profile.
    pub fn credential_binding(&self) -> &BootCredentialBinding {
        &self.credential
    }
    /// Revalidates the live fence and every current registration binding before a secret effect.
    pub fn recheck(&self) -> Result<(), NativeFailure> {
        self.transaction.revalidate()?;
        self.native.revalidate_execution()?;
        let context = self.context();
        let inspection = ReleaseInspection { lease: self };
        let snapshot = self.transaction.records.read_snapshot(context.key)?;
        if let Some(journal) = &snapshot.journal {
            super::journal_storage::require_allocation_coverage(journal, &snapshot.head)?;
        }
        if inspect_startup_snapshot(
            context.key,
            context.registration_generation,
            &snapshot,
            &inspection,
        )? != StartupPreflight::RetainApproved(context.installation)
        {
            return Err(NativeFailure::OutcomePending);
        }
        let head = &snapshot.head;
        let descriptor = self.native.descriptor();
        use sha2::{Digest, Sha256};
        let descriptor_digest: [u8; 32] = Sha256::digest(descriptor.encode()?).into();
        if descriptor_digest != head.approved_descriptor_sha256
            || descriptor.key != context.key
            || descriptor.server_id != context.server_id
            || descriptor.installation != context.installation
            || descriptor.account_binding_sha256 != context.account_binding_sha256
            || descriptor.registration_profile_sha256 != head.profile_sha256
            || descriptor.manifest_sha256 != context.manifest_sha256
            || descriptor.server_artifact_sha256 != context.server_artifact_sha256
            || descriptor.installation_binding()? != context.installation_binding
        {
            return Err(NativeFailure::GenerationMismatch);
        }
        if head.lifecycle_generation != context.lifecycle_generation
            || head.approved_data_generation != context.data_generation
            || head.approved_installation != context.installation
            || head.server_id != context.server_id
            || snapshot.profile.account().primary_gid() != context.primary_gid
            || snapshot.profile.account().sha256() != context.account_binding_sha256
            || head.approved_manifest_sha256 != context.manifest_sha256
            || head.active_request != context.journal_request
            || snapshot.profile.boot_credential().sha256()? != context.credential_binding_sha256
            || snapshot.profile.boot_credential().sha256()? != self.credential.sha256()?
        {
            return Err(NativeFailure::GenerationMismatch);
        }
        self.native.revalidate_execution()?;
        self.transaction.revalidate()
    }
}
struct ReleaseInspection<'a> {
    lease: &'a NativeReleaseLease,
}
impl StartupInspection for ReleaseInspection<'_> {
    fn read_snapshot(&self, key: AccountBuildKey) -> Result<StartupSnapshot, NativeFailure> {
        self.lease.transaction.records.read_snapshot(key)
    }
    fn observe_account(&self, _: AccountBuildKey) -> Result<AccountBinding, NativeFailure> {
        self.lease.native.observe_account()
    }
    fn check_storage(&self, snapshot: &StartupSnapshot) -> Result<(), NativeFailure> {
        self.lease.native.check_storage_and_policy(snapshot)
    }
}
impl<P: NativeReleasePeer> NativeTransactions<P> {
    /// Acquires the shared fence only after the native peer retains actual post-exec authority.
    pub fn authorize_release(
        &self,
        pending: &P::Pending,
        proof: &P::Proof,
    ) -> Result<NativeReleaseLease, NativeFailure> {
        let native = self.peer.retain_post_exec(pending, proof)?;
        native.revalidate_execution()?;
        let context = native.context();
        for digest in [
            &context.account_binding_sha256,
            &context.installation_binding,
            &context.server_artifact_sha256,
            &context.manifest_sha256,
            &context.credential_binding_sha256,
            &context.startup_nonce,
        ] {
            nonzero(digest)?;
        }
        for process in [context.original_process, context.retriever_process] {
            if process.pid == 0
                || process.pid > i32::MAX as u32
                || process.start_microseconds >= 1_000_000
            {
                return Err(NativeFailure::ProcessIdentityMismatch);
            }
        }
        if context.original_process.pid == context.retriever_process.pid {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        let deadline = native.deadline();
        let locks = self.records.lock(context.key, deadline)?;
        let snapshot = self.records.read_snapshot(context.key)?;
        let credential = snapshot.profile.boot_credential().clone();
        let lease = NativeReleaseLease {
            transaction: NativeTransactionLease {
                locks,
                records: Arc::clone(&self.records),
                authorization: None,
                admitting: false,
                deadline,
            },
            native,
            credential,
        };
        lease.recheck()?;
        Ok(lease)
    }
}
