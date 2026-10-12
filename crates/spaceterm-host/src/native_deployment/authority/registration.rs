//! Descriptor-bound registration issuance. Decoding alone never creates this authority.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::{
    NativeRegistrationStore,
    record_file::BoundRecord,
    records::{RecordProtection, classify, prefix},
};
use crate::{
    installation::RetainedRegistration,
    native_deployment::{bindings, contracts::*},
    protected_directory::OwnedProtectedDirectory,
};

pub(crate) struct RegistrationCustody {
    store: Arc<NativeRegistrationStore>,
    profiles: OwnedProtectedDirectory,
    head: BoundRecord,
    profile: BoundRecord,
    journal: Option<BoundRecord>,
}
impl RegistrationCustody {
    pub(crate) fn revalidate(&self, key: AccountBuildKey) -> Result<(), NativeFailure> {
        self.store.verify()?;
        self.store.require_clear_progress(key)?;
        self.profiles.verify().map_err(classify)?;
        self.head.verify(&self.store, self.store.root.file())?;
        self.profile.verify(&self.store, self.profiles.file())?;
        if let Some(journal) = &self.journal {
            journal.verify(&self.store, self.store.root.file())?;
        }
        Ok(())
    }
}

impl NativeRegistrationStore {
    /// Issues retained registration only from native protected descriptors under shared serialization.
    /// Account and storage observation still belong to the startup native peer, not this decoder.
    pub fn retain_registration(
        self: &Arc<Self>,
        key: AccountBuildKey,
        generation: std::num::NonZeroU64,
    ) -> Result<RetainedRegistration, NativeFailure> {
        let locks = self.lock(key, Instant::now() + Duration::from_secs(2))?;
        self.require_clear_progress(key)?;
        let snapshot = self.read_snapshot(key)?;
        if snapshot.head.generation != generation {
            return Err(NativeFailure::GenerationMismatch);
        }
        let name = prefix(key);
        let (head, bytes) =
            BoundRecord::open(self, self.root.file(), &format!("{name}.registration"), 256)?;
        if bytes != crate::native_registration::encode_registration_head(&snapshot.head)? {
            return Err(NativeFailure::GenerationMismatch);
        }
        let profiles = OwnedProtectedDirectory::open(
            &self.path.join(format!("{name}.profiles")),
            Arc::new(RecordProtection(Arc::clone(&self.protection))),
        )
        .map_err(classify)?;
        let (profile, bytes) = BoundRecord::open(
            self,
            profiles.file(),
            &format!("{}.json", bindings::hex(&snapshot.head.profile_sha256)),
            64 * 1024,
        )?;
        if profile.sha256 != snapshot.head.profile_sha256 || bytes != snapshot.profile.encode()? {
            return Err(NativeFailure::InvalidRecord);
        }
        let journal = snapshot
            .journal
            .as_ref()
            .map(|journal| {
                let (record, bytes) = BoundRecord::open(
                    self,
                    self.root.file(),
                    &format!("{name}.journal"),
                    64 * 1024,
                )?;
                if super::journal_document::JournalDocument::decode(&bytes)?.current != *journal {
                    return Err(NativeFailure::GenerationMismatch);
                }
                super::journal_storage::require_allocation_coverage(journal, &snapshot.head)?;
                if journal
                    .recovery_selection(&snapshot.head, &snapshot.profile.account().sha256())?
                    == crate::activation_journal::RecoverySelection::RepairRequired
                {
                    return Err(NativeFailure::Indeterminate);
                }
                Ok(record)
            })
            .transpose()?;
        let custody = RegistrationCustody {
            store: Arc::clone(self),
            profiles,
            head,
            profile,
            journal,
        };
        custody.revalidate(key)?;
        self.verify_locks(&locks)?;
        RetainedRegistration::from_native(snapshot.head, snapshot.profile, custody)
    }
}
