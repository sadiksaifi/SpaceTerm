//! Enrollment and self-revocation reachability share the native journal's exact
//! captured route. This connector has no registry, service manager, bootstrap,
//! enrollment authorization or credential-loading capability.

use crate::route::NativeRouteConnector;
use spaceterm_client::{
    api::{RouteDescription, RouteParts},
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournal, ClientJournalChange, ClientJournalTransaction, JournalCommitDisposition,
        RecoveredClientState,
    },
    limits::HANDSHAKE_DEADLINE,
    pairing::{
        credentials::{
            CredentialPurpose, CredentialUse, CredentialUsePurpose, NativeCredentialAccess,
            PairingFuture,
        },
        enrollment::{EnrollmentConnector, PairingTransport},
        revocation::RestrictedConnector,
    },
    ports::ConnectedRoute,
};
use spaceterm_core::pairing::{
    CredentialReference, PairingFailure, PendingRevocationTask, RevocationRouteReference,
};
use spaceterm_protocol::authentication::ServerPin;
use std::sync::Arc;
use zeroize::Zeroizing;

pub struct NativePairingConnector {
    journal: Arc<dyn ClientJournal>,
    routes: Arc<NativeRouteConnector>,
}
impl NativePairingConnector {
    /// Supply the same journal controller used by enrollment and pending work.
    pub fn new(journal: Arc<dyn ClientJournal>, routes: NativeRouteConnector) -> Self {
        Self::with_shared_routes(journal, Arc::new(routes))
    }
    /// Ordinary and restricted consumers share the same trusted local routes.
    pub fn with_shared_routes(
        journal: Arc<dyn ClientJournal>,
        routes: Arc<NativeRouteConnector>,
    ) -> Self {
        Self { journal, routes }
    }
    /// Called only in foreground route preparation, before enrollment. Captures effective SSH host, account, trust configuration
    /// and every jump endpoint in the one durable journal transaction owner.
    pub async fn capture_route(
        &self,
        reference: CredentialReference,
    ) -> Result<(), PairingFailure> {
        let state = self.journal.recover().await.map_err(failure)?;
        let slot = state
            .pairing()
            .slot(reference)
            .ok_or(PairingFailure::CredentialIntegrity)?;
        let route = slot.route.as_ref().ok_or(PairingFailure::RouteChanged)?;
        let RouteParts::Ssh { destination, .. } = route.parts() else {
            return Ok(());
        };
        let ssh = self
            .routes
            .ssh
            .as_ref()
            .ok_or(PairingFailure::RouteChanged)?;
        let capture = ssh.capture(destination).await.map_err(failure)?;
        let transaction = ClientJournalTransaction {
            expected_revision: state.revision(),
            changes: vec![ClientJournalChange::CaptureRestrictedSsh { reference, capture }],
        };
        match self.journal.commit(&transaction).await.map_err(failure)? {
            JournalCommitDisposition::Durable(_) => Ok(()),
            JournalCommitDisposition::NotApplied(reason) => Err(failure(reason.into())),
            JournalCommitDisposition::Indeterminate => Err(PairingFailure::RecoveryRequired),
        }
    }
    async fn connect(
        &self,
        state: &RecoveredClientState,
        pin: ServerPin,
        reference: CredentialReference,
        route: &RouteDescription,
        foreground: Option<&CredentialUse>,
    ) -> Result<Box<dyn PairingTransport>, PairingFailure> {
        let slot = state
            .pairing()
            .slot(reference)
            .ok_or(PairingFailure::CredentialIntegrity)?;
        if pin.public_key == [0; 32]
            || slot.server.map(|server| server.bytes()) != Some(pin.identity)
        {
            return Err(PairingFailure::IdentityMismatch);
        }
        if matches!(route.parts(), RouteParts::Ssh { .. }) && slot.ssh_capture.is_none() {
            return Err(PairingFailure::RouteChanged);
        }
        // Every native dial already owns its deadline and exact cleanup. Do not
        // drop that cleanup future merely because an outer deadline expired.
        let route = match foreground {
            Some(authorization) => {
                self.routes
                    .open_foreground_pairing_route(
                        pin,
                        route,
                        slot.ssh_capture.as_ref(),
                        authorization,
                    )
                    .await
            }
            None => {
                self.routes
                    .open_pairing_route(pin, route, slot.ssh_capture.as_ref())
                    .await
            }
        }
        .map_err(failure)?;
        Ok(Box::new(NativePairingTransport { route }))
    }
}
impl NativePairingConnector {
    async fn open_enrollment(
        &self,
        pin: ServerPin,
        route: RevocationRouteReference,
        foreground: Option<&CredentialUse>,
    ) -> Result<Box<dyn PairingTransport>, PairingFailure> {
        if let Some(authorization) = foreground {
            authorization.validate(authorization.scope())?;
            if authorization.purpose() != CredentialUsePurpose::Enrollment
                || authorization.native_access() != NativeCredentialAccess::Foreground
            {
                return Err(PairingFailure::AuthorizationDenied);
            }
        }
        let state = self.journal.recover().await.map_err(failure)?;
        let mut slots = state
            .pairing()
            .slots()
            .iter()
            .filter(|slot| slot.route_reference == Some(route));
        let slot = slots.next().ok_or(PairingFailure::RouteChanged)?;
        if slots.next().is_some() {
            return Err(PairingFailure::IdentityMismatch);
        }
        // Restricted acknowledgement can use its original saved/prepared
        // route, but never a later ordinary connection to the same Server.
        let targets = state
            .pairing()
            .prepared()
            .iter()
            .chain(
                state
                    .pairing()
                    .acknowledgements()
                    .iter()
                    .map(|ack| &ack.prepared),
            )
            .map(|prepared| prepared.target.target())
            .chain(
                state
                    .bindings()
                    .iter()
                    .map(|binding| binding.binding().map(|binding| binding.target().clone())),
            )
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        let concrete = if let Some(target) = targets
            .iter()
            .find(|target| target.credential() == slot.reference)
        {
            if target.pin() != pin {
                return Err(PairingFailure::IdentityMismatch);
            }
            target.route().clone()
        } else {
            slot.route.clone().ok_or(PairingFailure::RouteChanged)?
        };
        if let Some(authorization) = foreground {
            authorization.validate(authorization.scope())?;
            if authorization.purpose() != CredentialUsePurpose::Enrollment
                || authorization.native_access() != NativeCredentialAccess::Foreground
                || authorization.scope().reference() != slot.reference
                || authorization.scope().parts().namespace.as_str() != slot.namespace
                || authorization.scope().parts().purpose
                    != (CredentialPurpose::PairingAuthentication {
                        server: spaceterm_core::identity::ServerId::new(pin.identity),
                    })
            {
                return Err(PairingFailure::AuthorizationDenied);
            }
        }
        self.connect(&state, pin, slot.reference, &concrete, foreground)
            .await
    }
}
impl EnrollmentConnector for NativePairingConnector {
    fn open_foreground<'a>(
        &'a self,
        pin: ServerPin,
        route: RevocationRouteReference,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(self.open_enrollment(pin, route, Some(authorization)))
    }
    fn open<'a>(
        &'a self,
        pin: ServerPin,
        route: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(self.open_enrollment(pin, route, None))
    }
}
impl RestrictedConnector for NativePairingConnector {
    fn open<'a>(
        &'a self,
        task: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async move {
            let state = self.journal.recover().await.map_err(failure)?;
            let record = state
                .revocations()
                .iter()
                .find(|record| record.task == *task)
                .ok_or(PairingFailure::IdentityMismatch)?;
            let target = record.target.target().map_err(failure)?;
            let original = &task.parts().original;
            let slot = state
                .pairing()
                .slot(original.credential())
                .ok_or(PairingFailure::CredentialIntegrity)?;
            if target.pin().public_key != task.parts().pinned_server_public_key
                || target.local_pairing().map_err(failure)? != task.parts().local_pairing
                || target.device() != original.device()
                || target.client() != original.client()
                || target.credential() != original.credential()
                || slot.route_reference != Some(original.route())
            {
                return Err(PairingFailure::IdentityMismatch);
            }
            self.connect(
                &state,
                target.pin(),
                original.credential(),
                target.route(),
                None,
            )
            .await
        })
    }
}

struct NativePairingTransport {
    route: ConnectedRoute,
}
impl PairingTransport for NativePairingTransport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            tokio::time::timeout(HANDSHAKE_DEADLINE, self.route.sender.send(bytes))
                .await
                .map_err(|_| PairingFailure::TimedOut)?
                .map_err(failure)
        })
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(async move {
            tokio::time::timeout(HANDSHAKE_DEADLINE, self.route.receiver.receive())
                .await
                .map_err(|_| PairingFailure::TimedOut)?
                .map_err(failure)?
                .map(Zeroizing::new)
                .ok_or(PairingFailure::Offline)
        })
    }
}

fn failure(error: ClientFailure) -> PairingFailure {
    match error.class() {
        ClientFailureClass::Unavailable => PairingFailure::Offline,
        ClientFailureClass::AuthorizationDenied | ClientFailureClass::PinMismatch => {
            PairingFailure::AuthorizationDenied
        }
        ClientFailureClass::StaleAuthority | ClientFailureClass::UnsafeEndpoint => {
            PairingFailure::RouteChanged
        }
        ClientFailureClass::Incompatible => PairingFailure::Incompatible,
        ClientFailureClass::ResourceLimit | ClientFailureClass::CounterExhausted => {
            PairingFailure::LimitExceeded
        }
        ClientFailureClass::StorageUnavailable => PairingFailure::StorageUnavailable,
        ClientFailureClass::UnsafeStorage => PairingFailure::UnsafeStorage,
        ClientFailureClass::UnknownSchema => PairingFailure::UnknownSchema,
        ClientFailureClass::RecoveryRequired => PairingFailure::RecoveryRequired,
        ClientFailureClass::Cancelled => PairingFailure::Cancelled,
        _ => PairingFailure::InvalidDurableState,
    }
}
