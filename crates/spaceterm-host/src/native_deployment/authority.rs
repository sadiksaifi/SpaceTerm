//! Shared native transaction locks and live credential-release leases.
//!
//! Native peers authenticate requests and post-exec processes. Decoded journal and startup
//! records are comparison data, not authorization. Production composition remains evidence-gated.

use std::{sync::Arc, time::Instant};

use super::contracts::{AuthorizedNativeRequest, NativeFailure};
use crate::activation_journal::{ActivationJournalRecord, ActivationJournalState};

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod data_results;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod head_storage;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod journal_document;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod journal_storage;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod record_file;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod records;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod registration;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod repair;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use head_storage::{NativeHeadStorage, NativeRecordSync, SystemRecordSync};
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use registration::RegistrationCustody;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod release;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use records::NativeRegistrationStore;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use release::{NativeReleaseLease, NativeReleasePeer, NativeReleaseValidation};

/// Retained OS authorization for one already-admitted journal request.
/// Implementations belong to trusted native composition, never a record decoder.
pub trait NativeJournalAuthorization: Send + Sync {
    /// Original immutable request parameters, independently authenticated by the native peer.
    fn journal(&self) -> &ActivationJournalRecord;
    /// Original native authorization deadline. Acquiring a lock cannot extend it.
    fn deadline(&self) -> Instant;
    /// Revalidates native caller, account incarnation, exact request, right, and expiry.
    fn revalidate(&self) -> Result<(), NativeFailure>;
}

/// Native request authentication boundary. Tests inject only the native peer, not authority facts.
pub trait NativeTransactionPeer: Send + Sync {
    /// Production peers use the common noncloneable AuthorizedNativeRequest.
    type Request;
    /// Retains the original authorization, rather than copying an external form or caller UID.
    fn authorize(
        &self,
        request: &Self::Request,
    ) -> Result<Box<dyn NativeJournalAuthorization>, NativeFailure>;
}

/// Noncloneable lease holding fresh open-file descriptions on stable protected lock inodes.
/// Its destructor closes the descriptors. No serialized record can reconstruct this lease.
pub struct NativeTransactionLease {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) locks: records::TransactionLocks,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) records: Arc<NativeRegistrationStore>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    authorization: Option<Box<dyn NativeJournalAuthorization>>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    admitting: bool,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(crate) deadline: Instant,
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    _unavailable: std::convert::Infallible,
}
impl std::fmt::Debug for NativeTransactionLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeTransactionLease(..)")
    }
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
impl NativeTransactionLease {
    /// Rechecks retained lock bindings, custody, durable journal state, and original authorization.
    /// A later synchronization does not clear an uncertain journal.
    pub fn revalidate(&self) -> Result<(), NativeFailure> {
        self.revalidate_recovery()?;
        // Ordinary mutation always binds the selected admitted journal, including after
        // a deferred admission has been recovered under this lease.
        if let Some(authorization) = &self.authorization {
            let journal = self.records.read_journal(self.locks.key)?;
            match journal.state {
                ActivationJournalState::RecoveryRequired => {
                    return Err(NativeFailure::Indeterminate);
                }
                ActivationJournalState::Completed | ActivationJournalState::Aborted => {
                    return Err(NativeFailure::OutcomePending);
                }
                _ => (),
            }
            journal.same_request(authorization.journal())?;
        }
        self.records.require_clear_progress(self.locks.key)
    }
    fn revalidate_recovery(&self) -> Result<(), NativeFailure> {
        if Instant::now() >= self.deadline {
            return Err(NativeFailure::TimedOut);
        }
        self.records.verify_locks(&self.locks)?;
        if let Some(authorization) = &self.authorization {
            authorization.revalidate()?;
            if !self.admitting {
                self.records
                    .read_journal(self.locks.key)?
                    .same_request(authorization.journal())?;
            }
        }
        if Instant::now() >= self.deadline {
            return Err(NativeFailure::TimedOut);
        }
        Ok(())
    }
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
impl super::publication::NativePublicationTransaction for NativeTransactionLease {
    fn revalidate(&self) -> Result<(), NativeFailure> {
        self.revalidate()
    }
}

/// Serialization shared by publication, activation, repair, collection, helpers and releases.
pub trait NativeTransactionAuthority: Send + Sync {
    /// Acquires bounded authority for the OS-authenticated original request.
    fn begin(
        &self,
        request: &AuthorizedNativeRequest,
    ) -> Result<NativeTransactionLease, NativeFailure>;
    /// Rechecks authority without reclassifying an earlier uncertain write as durable.
    fn revalidate(&self, lease: &NativeTransactionLease) -> Result<(), NativeFailure>;
}

/// Native engine over retained protected records and independently authenticated requests.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub struct NativeTransactions<Peer> {
    pub(crate) records: Arc<NativeRegistrationStore>,
    pub(crate) peer: Peer,
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
impl<Peer: NativeTransactionPeer> NativeTransactions<Peer> {
    /// Injects native authority-bearing peers. Neither paths nor decoded records come from a client.
    pub fn new(records: Arc<NativeRegistrationStore>, peer: Peer) -> Self {
        Self { records, peer }
    }
    /// Authenticates and serializes the original request. New admission is synchronized
    /// before this returns any mutation lease. Replays resolve the existing journal.
    pub fn begin(&self, request: &Peer::Request) -> Result<NativeTransactionLease, NativeFailure> {
        self.begin_with_sync(request, SystemRecordSync)
    }
    /// Uses the native synchronization edge for journal admission, with the original deadline.
    pub fn begin_with_sync<S: NativeRecordSync>(
        &self,
        request: &Peer::Request,
        sync: S,
    ) -> Result<NativeTransactionLease, NativeFailure> {
        let authorization = self.peer.authorize(request)?;
        authorization.revalidate()?;
        authorization.journal().validate()?;
        let deadline = authorization.deadline();
        let locks = self.records.lock(authorization.journal().key, deadline)?;
        self.records.require_clear_progress(locks.key)?;
        let existing = match self.records.read_journal_document(locks.key) {
            Ok(document) => {
                if let Some(retained) = document.find(authorization.journal().request)
                    && retained.request != document.current.request
                {
                    retained.same_request(authorization.journal())?;
                    return Err(NativeFailure::OutcomePending);
                }
                Some(document.current)
            }
            Err(NativeFailure::RegistrationMissing) => None,
            Err(error) => return Err(error),
        };
        let admitting = if let Some(record) = &existing {
            if record.request == authorization.journal().request {
                record.same_request(authorization.journal())?;
                false
            } else if matches!(
                record.state,
                ActivationJournalState::Completed | ActivationJournalState::Aborted
            ) {
                true
            } else {
                return Err(NativeFailure::IdentityMismatch);
            }
        } else {
            true
        };
        let lease = NativeTransactionLease {
            locks,
            records: Arc::clone(&self.records),
            authorization: Some(authorization),
            admitting,
            deadline,
        };
        let mut storage = NativeHeadStorage::new(lease, sync);
        if admitting {
            storage.admit_journal(existing.as_ref())?;
            storage.lease.admitting = false;
        }
        storage.lease.revalidate()?;
        storage
            .inhibit_admitted_request()
            .map_err(|_| NativeFailure::Indeterminate)?;
        let lease = storage.into_lease();
        lease.revalidate()?;
        Ok(lease)
    }
    /// Reacquires the original authorized journal for content-validated head recovery only.
    /// This lease cannot pass ordinary revalidation while subordinate publication intent exists.
    pub fn begin_recovery(
        &self,
        request: &Peer::Request,
    ) -> Result<NativeTransactionLease, NativeFailure> {
        let authorization = self.peer.authorize(request)?;
        authorization.revalidate()?;
        authorization.journal().validate()?;
        let deadline = authorization.deadline();
        let locks = self.records.lock(authorization.journal().key, deadline)?;
        match self.records.read_journal_document(locks.key) {
            Ok(document) if document.repair.is_some() => return Err(NativeFailure::Indeterminate),
            Ok(_) | Err(NativeFailure::RegistrationMissing) => (),
            Err(error) => return Err(error),
        }
        let admitting = journal_storage::has_journal_intent(
            &self.records,
            locks.key,
            authorization.journal().request,
        )?;
        let lease = NativeTransactionLease {
            locks,
            records: Arc::clone(&self.records),
            authorization: Some(authorization),
            admitting,
            deadline,
        };
        lease.revalidate_recovery()?;
        Ok(lease)
    }
    /// Resolves an original terminal request under serialization without issuing mutation authority.
    pub fn resolve(
        &self,
        request: &Peer::Request,
    ) -> Result<crate::activation_journal::RetainedNativeOutcome, NativeFailure> {
        let authorization = self.peer.authorize(request)?;
        authorization.revalidate()?;
        let locks = self
            .records
            .lock(authorization.journal().key, authorization.deadline())?;
        self.records.verify_locks(&locks)?;
        self.records.require_clear_progress(locks.key)?;
        let document = self.records.read_journal_document(locks.key)?;
        let journal = document
            .find(authorization.journal().request)
            .ok_or(NativeFailure::IdentityMismatch)?;
        let outcome = journal.resolve_request(authorization.journal())?;
        authorization.revalidate()?;
        if Instant::now() >= authorization.deadline() {
            return Err(NativeFailure::TimedOut);
        }
        Ok(outcome)
    }
    /// Completes only the original request and releases its matching fence under serialization.
    /// Recovery results do not alter an earlier Indeterminate disposition.
    pub fn finish_request<S: NativeRecordSync>(
        &self,
        request: &Peer::Request,
        sync: S,
    ) -> crate::native_deployment::contracts::PublicationDisposition<
        crate::activation_journal::RetainedNativeOutcome,
    > {
        use crate::native_deployment::contracts::PublicationDisposition;
        let lease = match self.begin_recovery(request) {
            Ok(lease) => lease,
            Err(error) => return PublicationDisposition::NotApplied(error),
        };
        let storage = NativeHeadStorage::new(lease, sync);
        storage.finish_request()
    }
    /// Rejects a lease from another authority root before using its retained handles.
    pub fn revalidate(&self, lease: &NativeTransactionLease) -> Result<(), NativeFailure> {
        if !Arc::ptr_eq(&self.records, &lease.records) {
            return Err(NativeFailure::IdentityMismatch);
        }
        lease.revalidate()
    }
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
impl<P: NativeTransactionPeer<Request = AuthorizedNativeRequest>> NativeTransactionAuthority
    for NativeTransactions<P>
{
    fn begin(
        &self,
        request: &AuthorizedNativeRequest,
    ) -> Result<NativeTransactionLease, NativeFailure> {
        self.begin(request)
    }
    fn revalidate(&self, lease: &NativeTransactionLease) -> Result<(), NativeFailure> {
        self.revalidate(lease)
    }
}

/// Unavailable native boundary until genuine providers and privileged evidence pass.
#[derive(Default)]
pub struct UnavailableNativeTransactionAuthority;
impl NativeTransactionAuthority for UnavailableNativeTransactionAuthority {
    fn begin(&self, _: &AuthorizedNativeRequest) -> Result<NativeTransactionLease, NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
    fn revalidate(&self, _: &NativeTransactionLease) -> Result<(), NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
}

pub(crate) fn pending_linux_custody(
    _: &super::contracts::PendingStartup,
) -> Result<&crate::linux_custody::NativeLinuxCustody, NativeFailure> {
    Err(NativeFailure::CustodyUnavailable)
}
pub(crate) fn pending_retained_server(
    pending: &super::contracts::PendingStartup,
) -> Result<&std::fs::File, NativeFailure> {
    pending.retained_installation().retained_server()
}
pub(crate) fn pending_retained_credential_tool(
    pending: &super::contracts::PendingStartup,
) -> Result<&std::fs::File, NativeFailure> {
    pending.retained_installation().retained_credential_tool()
}
pub(crate) fn release_linux_custody(
    _: &super::contracts::CredentialReleasePermit,
) -> Result<&crate::linux_custody::NativeLinuxCustody, NativeFailure> {
    Err(NativeFailure::CustodyUnavailable)
}
pub(crate) fn release_retained_credential_tool(
    _: &super::contracts::CredentialReleasePermit,
) -> Result<&std::fs::File, NativeFailure> {
    Err(NativeFailure::ExecutionBindingUnproven)
}
