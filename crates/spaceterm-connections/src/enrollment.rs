//! Foreground enrollment and restricted recovery for the CLI's own connections.
use crate::credentials::{CredentialProvider, StoreKind};
use crate::{
    ConnectionStore, CredentialError, Interaction, PendingEnrollment, PendingRevocation,
    SavedConnection, StoreError, UnpairState,
};
use spaceterm_server::Lock;
use spaceterm_server::direct::{
    self, ComparisonCode, DeviceCredential, DirectError, Enrollment, Invitation, PairedDevice,
    RevocationReceipt,
};
use spaceterm_server::pane::control::ClientName;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Direct(DirectError),
    Credential(CredentialError),
    Store(StoreError),
    Busy,
    Unconfirmed,
}
impl From<DirectError> for Error {
    fn from(error: DirectError) -> Self {
        Self::Direct(error)
    }
}
impl From<CredentialError> for Error {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}
impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Network effects use only the exact persisted claim or revocation.
pub trait PairingNetwork: Send + Sync {
    fn submit(&self, enrollment: &mut Enrollment) -> Result<ComparisonCode, DirectError>;
    fn finish(&self, enrollment: &mut Enrollment) -> Result<Option<PairedDevice>, DirectError>;
    fn cancel(&self, enrollment: &mut Enrollment)
    -> Result<Option<RevocationReceipt>, DirectError>;
    fn revoke(
        &self,
        connection: &SavedConnection,
        credential: &DeviceCredential,
        deadline: Instant,
    ) -> Result<RevocationReceipt, DirectError>;
}
pub struct NativeNetwork;
impl PairingNetwork for NativeNetwork {
    fn submit(&self, enrollment: &mut Enrollment) -> Result<ComparisonCode, DirectError> {
        enrollment.submit()
    }
    fn finish(&self, enrollment: &mut Enrollment) -> Result<Option<PairedDevice>, DirectError> {
        enrollment.finish()
    }
    fn cancel(
        &self,
        enrollment: &mut Enrollment,
    ) -> Result<Option<RevocationReceipt>, DirectError> {
        enrollment.cancel()
    }
    fn revoke(
        &self,
        connection: &SavedConnection,
        credential: &DeviceCredential,
        deadline: Instant,
    ) -> Result<RevocationReceipt, DirectError> {
        direct::revoke_until(
            &connection.endpoint,
            &connection.paired,
            credential,
            connection.route,
            connection.paired.pairing,
            deadline,
        )
    }
}

pub struct PairingClient<'a> {
    pub store: &'a ConnectionStore,
    pub credentials: &'a dyn CredentialProvider,
    pub network: &'a dyn PairingNetwork,
}

pub struct Joining<'a> {
    client: &'a PairingClient<'a>,
    record: PendingEnrollment,
    enrollment: Enrollment,
    _guard: Lock,
    deadline: Instant,
}

impl<'a> PairingClient<'a> {
    /// Retains exact cancellation authority before any network effect.
    pub fn begin(
        &'a self,
        invitation: &Invitation,
        name: String,
        client_name: ClientName,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Joining<'a>, Error> {
        if !crate::valid_name(&name) {
            return Err(StoreError::NameTaken.into());
        }
        let credentials = self.credentials.select()?;
        let route = crate::random_id()?;
        let credential_key = crate::random_id()?;
        let credential = DeviceCredential::generate()?;
        let mut enrollment = Enrollment::prepare(invitation, client_name, &credential)?;
        enrollment.set_deadline(deadline);
        let record = PendingEnrollment {
            route,
            name,
            endpoint: invitation.endpoint().to_owned(),
            pin: invitation.server_pin().clone(),
            claim: enrollment.claim_id(),
            credential: credential_key,
            credential_store: credentials.kind(),
            public_key: credential.public_key()?,
            offered: false,
            confirmed: false,
        };
        let guard = self.store.recovery_guard(&route)?.ok_or(Error::Busy)?;
        self.store
            .transaction(deadline, cancelled)
            .reserve(record.clone())?;
        let result = credentials.insert(&credential_key, credential.expose_bytes());
        if let Err(error) = result {
            // A failed insert grants no authority over an existing native item. Recovery
            // checks the generated public key before retiring any item.
            let _ = self.recover_enrollment(&record, deadline, &|| false);
            return Err(error.into());
        }
        Ok(Joining {
            client: self,
            record,
            enrollment,
            _guard: guard,
            deadline,
        })
    }

    /// Called only by later mutating connection commands. A live join keeps its lock.
    pub fn recover(&self, deadline: Instant) -> Result<usize, Error> {
        self.recover_while(deadline, &|| false)
    }
    pub fn recover_while(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<usize, Error> {
        let mut pending = 0;
        for record in self.store.pending_enrollments()? {
            if Instant::now() >= deadline || cancelled() {
                pending += 1;
                continue;
            }
            let Some(_guard) = self.store.recovery_guard(&record.route)? else {
                continue;
            };
            // The live owner may have published and released its lock after our snapshot.
            // Re-read under the route lock before acquiring any cancellation authority.
            let Some(current) = self
                .store
                .pending_enrollments()?
                .into_iter()
                .find(|row| row.route == record.route)
            else {
                continue;
            };
            if self
                .recover_enrollment(&current, deadline, cancelled)
                .is_err()
            {
                pending += 1;
            }
        }
        for record in self.store.pending_revocations()? {
            if Instant::now() >= deadline || cancelled() {
                pending += 1;
                continue;
            }
            let Some(_guard) = self.store.recovery_guard(&record.connection.route)? else {
                continue;
            };
            let Some(current) = self
                .store
                .pending_revocations()?
                .into_iter()
                .find(|row| row.connection.route == record.connection.route)
            else {
                continue;
            };
            if self
                .recover_revocation(&current, deadline, cancelled)
                .is_err()
            {
                pending += 1;
            }
        }
        Ok(pending)
    }

    fn read_credential(&self, key: &[u8; 32], kind: StoreKind) -> Result<DeviceCredential, Error> {
        let bytes = self
            .credentials
            .open(kind)?
            .read(key, Interaction::Quiet)?
            .ok_or(CredentialError::Unavailable)?;
        Ok(DeviceCredential::from_bytes(&bytes)?)
    }

    fn recover_enrollment(
        &self,
        record: &PendingEnrollment,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), Error> {
        let credentials = self.credentials.open(record.credential_store)?;
        let bytes = credentials.read(&record.credential, Interaction::Quiet)?;
        let credential = bytes
            .as_ref()
            .map(|bytes| DeviceCredential::from_bytes(bytes))
            .transpose()?;
        let owns_item = credential
            .as_ref()
            .map(|credential| credential.public_key())
            .transpose()?
            .is_some_and(|key| key == record.public_key);
        if !record.confirmed {
            if record.offered {
                if !owns_item {
                    return Err(CredentialError::Unavailable.into());
                }
                let mut enrollment = Enrollment::resume(
                    &record.endpoint,
                    record.pin.clone(),
                    record.claim,
                    credential.as_ref().ok_or(CredentialError::Unavailable)?,
                )?;
                enrollment.set_deadline(deadline);
                if let Some(receipt) = self.network.cancel(&mut enrollment)? {
                    enrollment.validate_cancellation(&receipt)?;
                }
            }
            self.store
                .transaction(deadline, cancelled)
                .confirm_enrollment_cleanup(record.route)?;
        }
        if owns_item {
            credentials.remove(&record.credential, Interaction::Quiet)?;
        }
        self.store
            .transaction(deadline, cancelled)
            .retire_enrollment(record.route)?;
        Ok(())
    }

    pub fn recover_revocation(
        &self,
        record: &PendingRevocation,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), Error> {
        let saved = &record.connection;
        if !record.confirmed {
            let credential = self.read_credential(&saved.credential, saved.credential_store)?;
            let receipt = self.network.revoke(saved, &credential, deadline)?;
            if receipt.server != saved.paired.pin.server
                || receipt.operation != saved.route
                || receipt.target != saved.paired.pairing
            {
                return Err(DirectError::Substituted.into());
            }
            self.store
                .transaction(deadline, cancelled)
                .confirm_revocation(saved.route)?;
        }
        self.credentials
            .open(saved.credential_store)?
            .remove(&saved.credential, Interaction::Quiet)?;
        self.store
            .transaction(deadline, cancelled)
            .retire_pending(saved.route)?;
        Ok(())
    }
}

impl Joining<'_> {
    pub fn submit(&mut self, cancelled: &dyn Fn() -> bool) -> Result<ComparisonCode, Error> {
        self.client
            .store
            .transaction(self.deadline, cancelled)
            .mark_offered(self.record.route)?;
        self.record.offered = true;
        Ok(self.client.network.submit(&mut self.enrollment)?)
    }
    pub fn finish(
        &mut self,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<SavedConnection>, Error> {
        let Some(paired) = self.client.network.finish(&mut self.enrollment)? else {
            return Ok(None);
        };
        if paired.pin != self.record.pin || paired.client == [0; 32] || paired.pairing == [0; 32] {
            return Err(DirectError::Substituted.into());
        }
        let connection = SavedConnection {
            name: self.record.name.clone(),
            route: self.record.route,
            endpoint: self.record.endpoint.clone(),
            paired,
            credential: self.record.credential,
            credential_store: self.record.credential_store,
            unpair: UnpairState::Paired,
        };
        if let Err(error) = self
            .client
            .store
            .transaction(self.deadline, cancelled)
            .publish(connection.clone())
        {
            if matches!(error, StoreError::Timeout | StoreError::Interrupted) {
                return Err(error.into());
            }
            // A rename may have succeeded before its synchronization failed. Only a
            // synchronized reread can settle publication; cancellation must never guess.
            let rows = self.client.store.load().map_err(|_| Error::Unconfirmed)?;
            if !rows.iter().any(|row| row == &connection) {
                return Err(StoreError::Unavailable.into());
            }
        }
        Ok(Some(connection))
    }
    pub fn cancel(&mut self, deadline: Instant) -> Result<(), Error> {
        // The published route owns the credential now. Never cancel through stale metadata.
        if self
            .client
            .store
            .load()?
            .iter()
            .any(|row| row.route == self.record.route)
        {
            return Err(Error::Unconfirmed);
        }
        let record = self
            .client
            .store
            .pending_enrollments()?
            .into_iter()
            .find(|row| row.route == self.record.route)
            .ok_or(StoreError::NotFound)?;
        self.client.recover_enrollment(&record, deadline, &|| false)
    }
}
