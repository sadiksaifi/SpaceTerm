//! Dedicated headless consumers of the shared pairing journal and connection worker.
//! Restoration never creates storage, enrolls or selects another consumer's Client.

mod assessments;
mod enrollment;

use super::{NativeServiceManagement, classify_service};
use crate::{
    credentials::adapter::DeviceCredentials,
    journal::NativeClientJournal,
    pairing_runtime::NativePairingRuntime,
    restricted_routes::NativePairingConnector,
    route::{LocalRouteAuthority, NativeRouteConnector},
};
use spaceterm_client::{
    api::{ConnectedInfo, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass},
    journal::{JournalController, JournalLimits, ReceiptVerifier},
    limits::ClientLimits,
    management_cli::{
        AssessmentToken, ConfirmedManagementAction, ManagementAction, ManagementAssessment,
        ManagementClientKind, ManagementFailure, ManagementFuture, ProtocolManagement,
        ServerStatus, StatusBackend, StatusManagement, StatusOnlyManagement, StatusSubscription,
    },
    pairing::credentials::NativeDeviceCredentials,
    pending::LocalUseInvalidator,
    ports::{
        ClientFuture, ClientServices, ConnectionDelegate, ConnectionInvalidation,
        ConnectionPurpose, CredentialSource, OrganizationDecoder, OrganizationPublication,
        OutboundClass, ProtocolChannel, SigningAccess,
    },
    registry::{ClientRegistry, ClientRuntime, ServerHandle},
    replica::{OrganizationCursor, PublicationStamp, ReplicaSnapshot, SnapshotPart},
};
use spaceterm_core::{
    authority::GrantedControl,
    identity::WorkspaceId,
    operation_ledger::canonical::CanonicalCodec,
    operations::OperationId,
    pairing::{LocalPairingKey, PairingFailure, PairingReceiptBody, SignedPairingReceipt},
};
use spaceterm_host::account_service::RegisteredServer;
use spaceterm_protocol::{
    authentication::{HandshakeConfig, ServerPin},
    codec::Payload,
    convert::organization,
    pairing_adapter::ReceiptCodec,
    wire::{v1, v2},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::OnceCell;

/// Each executable has one dedicated journal. It cannot borrow the desktop's
/// primary Client or the native lifecycle bridge's management Client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeConsumerKind {
    Cli,
    StatusCompanion,
}
impl NativeConsumerKind {
    fn directory_name(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::StatusCompanion => "companion",
        }
    }
    fn management_kind(self) -> ManagementClientKind {
        match self {
            Self::Cli => ManagementClientKind::Cli,
            Self::StatusCompanion => ManagementClientKind::StatusCompanion,
        }
    }
    pub fn directory(self, registration: &RegisteredServer) -> Result<PathBuf, ManagementFailure> {
        let management = super::runtime::NativeRuntimeIdentity::directory(registration)
            .map_err(classify_service)?;
        Ok(management.with_file_name(self.directory_name()))
    }
}

/// Lazy production restoration preserves read-only native construction. An absent
/// dedicated journal is PairingRequired, never a request to provision a new key.
pub struct NativeConsumer {
    native: Arc<NativeServiceManagement>,
    credentials: Arc<dyn NativeDeviceCredentials>,
    kind: NativeConsumerKind,
    mutation: tokio::sync::Mutex<()>,
    owner: OnceCell<PairingOwner>,
    opened: OnceCell<OpenedConsumer>,
}
struct PairingOwner {
    pairing: Arc<NativePairingRuntime>,
    routes: Arc<NativeRouteConnector>,
    invalidator: Arc<ConsumerInvalidator>,
}
struct OpenedConsumer {
    pairing: Arc<NativePairingRuntime>,
    server: ServerHandle,
    operations: Option<Arc<super::tracked::ManagementOperations>>,
    kind: ManagementClientKind,
    _registry: Arc<ClientRegistry>,
    _runtime: ClientRuntime,
}
impl NativeConsumer {
    /// Selects one dedicated consumer with native credentials. Construction is
    /// read-only; enrollment remains an explicit foreground operation.
    pub fn new(native: Arc<NativeServiceManagement>, kind: NativeConsumerKind) -> Self {
        Self {
            native,
            credentials: Arc::new(DeviceCredentials::native()),
            kind,
            mutation: tokio::sync::Mutex::new(()),
            owner: OnceCell::new(),
            opened: OnceCell::new(),
        }
    }

    /// Native credential boundary for deliberate enrollment and restoration.
    /// Production construction selects Keychain or Secret Service.
    pub fn with_credentials(mut self, credentials: Arc<dyn NativeDeviceCredentials>) -> Self {
        self.credentials = credentials;
        self
    }
    /// Retains one already recovered consumer journal without ordinary dialing.
    /// Restricted retry remains available when ordinary use has been suppressed.
    pub fn with_recovered_journal(
        native: Arc<NativeServiceManagement>,
        kind: NativeConsumerKind,
        journal: Arc<JournalController>,
        credentials: Arc<dyn NativeDeviceCredentials>,
        routes: NativeRouteConnector,
    ) -> Self {
        Self {
            native,
            credentials: credentials.clone(),
            kind,
            mutation: tokio::sync::Mutex::new(()),
            owner: OnceCell::new_with(Some(pairing_owner(journal, credentials, routes))),
            opened: OnceCell::new(),
        }
    }
    /// Composition seam for real journals with injected credential and route
    /// boundaries. Receipt verification and worker authentication stay real.
    pub async fn restore(
        native: Arc<NativeServiceManagement>,
        kind: NativeConsumerKind,
        journal: Arc<JournalController>,
        credentials: Arc<dyn NativeDeviceCredentials>,
        routes: NativeRouteConnector,
    ) -> Result<Self, ManagementFailure> {
        let consumer = Self::with_recovered_journal(native, kind, journal, credentials, routes);
        consumer.opened().await?;
        Ok(consumer)
    }
    async fn owner(&self) -> Result<&PairingOwner, ManagementFailure> {
        self.owner
            .get_or_try_init(|| async {
                let directory = self
                    .native
                    .supervisor
                    .client_connections_directory()
                    .join(self.kind.directory_name());
                match std::fs::symlink_metadata(&directory) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Err(ManagementFailure::PairingRequired);
                    }
                    Err(_) => return Err(ManagementFailure::StorageUnavailable),
                    Ok(_) => {}
                }
                let codec = Arc::new(CanonicalCodec);
                let verifier = Arc::new(PinnedReceipts);
                let limits = JournalLimits::default();
                let storage = NativeClientJournal::open_existing(
                    directory,
                    codec.clone(),
                    verifier.clone(),
                    limits,
                )
                .await
                .map_err(classify_client)?;
                let journal = Arc::new(
                    JournalController::recover(Arc::new(storage), codec, verifier, limits)
                        .await
                        .map_err(classify_client)?,
                );
                let endpoint = match self.native.supervisor.discover() {
                    Ok(Some(registration)) => registration.endpoint().ok(),
                    Ok(None) | Err(_) => self
                        .native
                        .supervisor
                        .retained_local_endpoint()
                        .ok()
                        .flatten(),
                };
                // Unavailable or unsafe runtime storage grants no IPC authority.
                // It must not hide the verified journal or cause storage repair.
                let local = endpoint.and_then(|endpoint| LocalRouteAuthority::new(endpoint).ok());
                let routes = NativeRouteConnector::new(local.into_iter().collect(), None)
                    .map_err(classify_client)?;
                Ok(pairing_owner(journal, self.credentials.clone(), routes))
            })
            .await
    }
    async fn opened(&self) -> Result<&OpenedConsumer, ManagementFailure> {
        let registration = self
            .native
            .supervisor
            .discover()
            .map_err(classify_service)?
            .ok_or(ManagementFailure::NativeUnavailable)?;
        if registration.desired_state() != spaceterm_host::startup::DesiredServerState::Running
            || !registration.is_installed()
        {
            return Err(ManagementFailure::NativeUnavailable);
        }
        // Ordinary use retains typed native route failures. Journal-only removal
        // and restricted retry recovery do not require this runtime to be present.
        registration.endpoint().map_err(classify_service)?;
        let opened = self
            .opened
            .get_or_try_init(|| async {
                let owner = self.owner().await?;
                let inventory = owner.pairing.inventory().await.map_err(classify_pairing)?;
                if inventory
                    .saved
                    .iter()
                    .any(|saved| saved.pin() != registration.server_pin())
                {
                    return Err(ManagementFailure::UnsafeNativeState);
                }
                if self.kind == NativeConsumerKind::Cli {
                    self.recover_protocol_receipts(owner.pairing.journal())
                        .await?;
                }
                open_consumer(self.kind, owner).await
            })
            .await?;
        let target = opened
            .pairing
            .journal()
            .snapshot()
            .map_err(classify_client)?
            .bindings()
            .iter()
            .find(|saved| {
                saved
                    .binding()
                    .is_ok_and(|binding| binding.target().server() == opened.server.identity())
            })
            .ok_or(ManagementFailure::PairingRequired)?
            .binding()
            .map_err(classify_client)?;
        if target.target().pin() != registration.server_pin() {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        Ok(opened)
    }
    async fn settle_protocol_receipt(
        &self,
        receipt: &super::receipts::Receipt,
        journal: &JournalController,
    ) -> Result<(), ManagementFailure> {
        let super::receipts::Receipt::Protocol { binding, outcome } = receipt else {
            return Ok(());
        };
        let state = journal.snapshot().map_err(classify_client)?;
        let Some(header) = state.stream(binding.id.stream()) else {
            return Ok(());
        };
        if let Some(record) = state.operation(binding.id) {
            if record.binding != *binding
                || !matches!(&record.state,
                spaceterm_client::journal::RetainedOperationState::Completed { outcome: original, .. } if original == outcome)
            {
                return Err(ManagementFailure::UnsafeNativeState);
            }
            journal
                .transact(vec![
                    spaceterm_client::journal::ClientJournalChange::ConsumeResult(binding.id),
                ])
                .await
                .map_err(classify_client)?;
        } else if header.confirmed_prefix < binding.id.sequence().get() {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        super::receipts::settle(&self.native.supervisor, receipt)
    }
    async fn recover_protocol_receipts(
        &self,
        journal: &JournalController,
    ) -> Result<(), ManagementFailure> {
        for receipt in super::receipts::unsettled(&self.native.supervisor)? {
            self.settle_protocol_receipt(&receipt, journal).await?;
        }
        Ok(())
    }
    async fn wait_retained(
        &self,
        operations: &super::tracked::ManagementOperations,
        ticket: &spaceterm_client::operations::OperationTicket,
    ) -> Result<Result<spaceterm_core::operations::Outcome, ClientFailure>, ManagementFailure> {
        let result = operations.wait(ticket).await;
        if let Ok(outcome) = &result {
            let compact = spaceterm_core::operation_ledger_contracts::CompactOutcome::new(
                outcome,
                operations.tracker.journal().codec(),
            )
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
            let receipt = super::receipts::Receipt::Protocol {
                binding: ticket.binding(),
                outcome: compact,
            };
            super::receipts::retain(&self.native.supervisor, &receipt)?;
            self.settle_protocol_receipt(&receipt, operations.tracker.journal())
                .await?;
        }
        Ok(result)
    }
    fn mutable(&self) -> Result<(), ManagementFailure> {
        if self.kind == NativeConsumerKind::Cli {
            Ok(())
        } else {
            Err(ManagementFailure::AuthorizationDenied)
        }
    }
    fn now(&self) -> Result<spaceterm_core::observations::MonotonicTick, ManagementFailure> {
        Ok(spaceterm_core::observations::MonotonicTick::new(
            spaceterm_host::account_service::ServiceTime::current()
                .map_err(classify_service)?
                .millis,
        ))
    }
    async fn assessment_owner(
        &self,
        context: Option<spaceterm_client::management_cli::AuthenticatedManagementContext>,
    ) -> Result<(PathBuf, assessments::Scope), ManagementFailure> {
        let runtime = self.pairing_runtime().await?;
        let inventory = runtime.inventory().await.map_err(classify_pairing)?;
        if inventory.saved.len() != 1 {
            return Err(ManagementFailure::PairingRequired);
        }
        let saved = &inventory.saved[0];
        let scope = if let Some(context) = context {
            if context.stream() != saved.client().parts().ordinary
                || context.device() != saved.device()
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            assessments::Scope::new(context, saved.pin())?
        } else {
            assessments::Scope::local(
                saved.client().parts().ordinary,
                saved.device(),
                saved.pin(),
                spaceterm_client::journal::ClientJournalRevision::new(inventory.revision),
            )?
        };
        // Account/build authority selects storage even if server registration is
        // absent or unreadable. The verified journal supplies pairing scope only.
        let directory = self
            .native
            .supervisor
            .client_connections_directory()
            .with_file_name("cli-assessments");
        Ok((directory, scope))
    }
    async fn retained_assessment(
        &self,
        token: &AssessmentToken,
    ) -> Result<ManagementAssessment, ManagementFailure> {
        let (directory, scope) =
            self.assessment_owner(None)
                .await
                .map_err(|failure| match failure {
                    // A token names original consent. Missing pairing authority
                    // invalidates that consent, rather than requesting new pairing.
                    ManagementFailure::PairingRequired => ManagementFailure::StaleAssessment,
                    failure => failure,
                })?;
        assessments::restore_owned(directory, scope, token.clone(), self.now()?).await
    }
    async fn consume_assessment(
        &self,
        assessment: &ManagementAssessment,
    ) -> Result<(), ManagementFailure> {
        let context = if matches!(
            assessment.evidence(),
            spaceterm_client::management_cli::AssessmentEvidence::LocalSelfUnpair { .. }
        ) {
            None
        } else {
            Some(self.opened().await?.backend()?.context()?)
        };
        let (directory, scope) = self.assessment_owner(context).await?;
        assessments::consume(directory, scope, assessment.clone(), self.now()?).await
    }
    /// Closes only this consumer's connection worker, never the server.
    pub async fn shutdown(mut self) -> Result<(), ManagementFailure> {
        if let Some(opened) = self.opened.take() {
            opened._runtime.shutdown().await.map_err(classify_client)?;
        }
        Ok(())
    }
    /// The sole pairing runtime for this consumer, including restricted pending
    /// work. Ordinary restoration has no Start or enrollment effect.
    pub async fn pairing_runtime(&self) -> Result<Arc<NativePairingRuntime>, ManagementFailure> {
        self.mutable()?;
        Ok(self.owner().await?.pairing.clone())
    }
}
impl OpenedConsumer {
    fn backend(&self) -> Result<impl StatusBackend + use<>, ManagementFailure> {
        Ok(self
            .server
            .queries(self.pairing.journal().clone())
            .map_err(classify_client)?
            .status_backend(self.kind))
    }
}
impl StatusManagement for NativeConsumer {
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(async {
            StatusOnlyManagement::new(self.opened().await?.backend()?)
                .status()
                .await
        })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async {
            StatusOnlyManagement::new(self.opened().await?.backend()?)
                .subscribe_status()
                .await
        })
    }
}
impl ProtocolManagement for NativeConsumer {
    fn negotiated_major(&self) -> Result<u32, ManagementFailure> {
        self.opened
            .get()
            .ok_or(ManagementFailure::Offline)?
            .backend()?
            .context()
            .map(|context| context.major())
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(async { self.opened().await?.backend()?.status().await })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async { self.opened().await?.backend()?.subscribe_status().await })
    }
    fn assess(&self, action: ManagementAction) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async move {
            self.mutable()?;
            let opened = self.opened().await?;
            let context = opened.backend()?.context()?;
            if action.server() != context.server() {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            let evidence = match &action {
                ManagementAction::RemoteAccess { .. } => {
                    let status = opened.backend()?.status().await?;
                    spaceterm_client::management_cli::AssessmentEvidence::RemoteAccess {
                        current_version: status.parts().remote_access.version,
                    }
                }
                ManagementAction::EndAllSessions { server } => {
                    let operations = opened
                        .operations
                        .as_ref()
                        .ok_or(ManagementFailure::AuthorizationDenied)?;
                    let ticket = operations
                        .submit_command(spaceterm_core::operations::Command::AssessEndAllSessions {
                            server: *server,
                        })
                        .await
                        .map_err(classify_client)?;
                    match self
                        .wait_retained(operations, &ticket)
                        .await?
                        .map_err(|_| ManagementFailure::OutcomePending)?
                    {
                        spaceterm_core::operations::Outcome::Assessment(value) => {
                            spaceterm_client::management_cli::AssessmentEvidence::EndAllSessions(
                                *value,
                            )
                        }
                        _ => return Err(ManagementFailure::StaleAssessment),
                    }
                }
                ManagementAction::Unpair { target } => {
                    if target.parts().device != context.device() {
                        return Err(ManagementFailure::NativeUnavailable);
                    }
                    let operations = opened
                        .operations
                        .as_ref()
                        .ok_or(ManagementFailure::AuthorizationDenied)?;
                    let ticket = operations
                        .submit_command(spaceterm_core::operations::Command::Pairing(
                            spaceterm_core::pairing::PairingCommand::AssessUnpair {
                                target: target.clone(),
                            },
                        ))
                        .await
                        .map_err(classify_client)?;
                    match self
                        .wait_retained(operations, &ticket)
                        .await?
                        .map_err(|_| ManagementFailure::OutcomePending)?
                    {
                        spaceterm_core::operations::Outcome::Pairing(value) => match *value {
                            spaceterm_core::pairing::PairingOutcome::UnpairAssessed(value) => {
                                spaceterm_client::management_cli::AssessmentEvidence::Unpair(value)
                            }
                            _ => return Err(ManagementFailure::StaleAssessment),
                        },
                        _ => return Err(ManagementFailure::StaleAssessment),
                    }
                }
                _ => return Err(ManagementFailure::NativeUnavailable),
            };
            if opened.backend()?.context()? != context {
                return Err(ManagementFailure::Offline);
            }
            let issued = self.now()?;
            let expires = spaceterm_core::observations::MonotonicTick::new(
                issued
                    .get()
                    .checked_add(30_000)
                    .ok_or(ManagementFailure::LimitExceeded)?,
            );
            let mut nonce = [0; 32];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
                .map_err(|_| ManagementFailure::NativeUnavailable)?;
            let token = AssessmentToken::new(crate::cli::output::hex(&nonce))?;
            let assessment = ManagementAssessment::new(token, action, evidence, issued, expires)?;
            let (directory, scope) = self.assessment_owner(Some(context)).await?;
            if opened.backend()?.context()? != context {
                return Err(ManagementFailure::Offline);
            }
            assessments::retain(directory, scope, assessment.clone(), issued, expires).await?;
            Ok(assessment)
        })
    }
    fn restore_assessment(
        &self,
        action: ManagementAction,
        token: AssessmentToken,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async move {
            self.mutable()?;
            let assessment = self.retained_assessment(&token).await?;
            if assessment.action() != &action {
                return Err(ManagementFailure::StaleAssessment);
            }
            let context = self.opened().await?.backend()?.context()?;
            if assessment.action().server() != context.server()
                || assessment
                    .epoch()
                    .is_some_and(|epoch| epoch != context.epoch())
            {
                return Err(ManagementFailure::StaleAssessment);
            }
            let (directory, scope) = self.assessment_owner(Some(context)).await?;
            assessments::restore(directory, scope, token, self.now()?).await
        })
    }
    fn execute(
        &self,
        confirmed: ConfirmedManagementAction,
    ) -> ManagementFuture<'_, spaceterm_core::management::ManagementProgress> {
        Box::pin(async move {
            self.mutable()?;
            let _serial = self
                .mutation
                .try_lock()
                .map_err(|_| ManagementFailure::OutcomePending)?;
            let assessment = confirmed.assessment();
            if matches!(assessment.action(), ManagementAction::Unpair { .. }) {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            if self.retained_assessment(assessment.token()).await? != *assessment {
                return Err(ManagementFailure::StaleAssessment);
            }
            let opened = self.opened().await?;
            let context = opened.backend()?.context()?;
            if assessment.action().server() != context.server()
                || assessment
                    .epoch()
                    .is_some_and(|epoch| epoch != context.epoch())
            {
                return Err(ManagementFailure::StaleAssessment);
            }
            let operations = opened
                .operations
                .as_ref()
                .ok_or(ManagementFailure::AuthorizationDenied)?;
            // Consume before the first await that can admit a durable operation.
            // Cancellation cannot reuse this confirmation for a new identity.
            self.consume_assessment(assessment).await?;
            assessment.validate_at(self.now()?)?;
            if opened.backend()?.context()? != context {
                return Err(ManagementFailure::StaleAssessment);
            }
            let ticket = operations
                .submit_command(assessment.command())
                .await
                .map_err(classify_client)?;
            Ok(progress(
                &ticket,
                self.wait_retained(operations, &ticket).await?,
            ))
        })
    }
    fn resolve(
        &self,
        operation: OperationId,
    ) -> ManagementFuture<'_, spaceterm_core::management::ManagementProgress> {
        Box::pin(async move {
            self.mutable()?;
            let owner = self.owner().await?;
            self.recover_protocol_receipts(owner.pairing.journal())
                .await?;
            if let Some(receipt @ super::receipts::Receipt::Protocol { .. }) =
                super::receipts::load(
                    &self.native.supervisor,
                    &super::receipts::protocol_key(operation)?,
                )?
            {
                let super::receipts::Receipt::Protocol { binding, outcome } = &receipt else {
                    unreachable!()
                };
                let state = owner
                    .pairing
                    .journal()
                    .snapshot()
                    .map_err(classify_client)?;
                let saved = state
                    .bindings()
                    .iter()
                    .find(|saved| saved.streams().parts().ordinary == operation.stream())
                    .ok_or(ManagementFailure::AuthorizationDenied)?;
                let paired = saved.binding().map_err(classify_client)?;
                if !owner
                    .pairing
                    .journal()
                    .ordinary_use_allowed(&paired)
                    .map_err(classify_client)?
                {
                    return Err(ManagementFailure::AuthorizationDenied);
                }
                let outcome = outcome
                    .outcome(owner.pairing.journal().codec())
                    .map_err(|_| ManagementFailure::UnsafeNativeState)?;
                return Ok(progress_binding(*binding, Ok(outcome)));
            }
            let opened = self.opened().await?;
            let operations = opened
                .operations
                .as_ref()
                .ok_or(ManagementFailure::AuthorizationDenied)?;
            let ticket = operations
                .resolve_original(operation)
                .await
                .map_err(|_| ManagementFailure::OutcomePending)?;
            Ok(progress(
                &ticket,
                self.wait_retained(operations, &ticket).await?,
            ))
        })
    }
}

fn pairing_owner(
    journal: Arc<JournalController>,
    credentials: Arc<dyn NativeDeviceCredentials>,
    routes: NativeRouteConnector,
) -> PairingOwner {
    let invalidator = Arc::new(ConsumerInvalidator::default());
    let routes = Arc::new(routes);
    let connector = Arc::new(NativePairingConnector::with_shared_routes(
        journal.clone(),
        routes.clone(),
    ));
    let pairing = Arc::new(NativePairingRuntime::with_adapters(
        journal.clone(),
        credentials,
        connector.clone(),
        connector,
        invalidator.clone(),
        HandshakeConfig::default(),
    ));
    PairingOwner {
        pairing,
        routes,
        invalidator,
    }
}
async fn open_consumer(
    kind: NativeConsumerKind,
    owner: &PairingOwner,
) -> Result<OpenedConsumer, ManagementFailure> {
    open_pairing(kind, owner, true, None).await
}
async fn open_pairing(
    kind: NativeConsumerKind,
    owner: &PairingOwner,
    additional_only: bool,
    selected_pin: Option<ServerPin>,
) -> Result<OpenedConsumer, ManagementFailure> {
    let pairing = &owner.pairing;
    let journal = pairing.journal().clone();
    let inventory = pairing.inventory().await.map_err(classify_pairing)?;
    // Dedicated stores contain exactly one explicitly selected server-issued
    // additional Client. Ambiguous stores never fall back to the first record.
    let selected = inventory
        .saved
        .iter()
        .filter(|saved| selected_pin.is_none_or(|pin| saved.pin() == pin))
        .collect::<Vec<_>>();
    let saved = match selected.as_slice() {
        [saved] => *saved,
        [] => return Err(ManagementFailure::PairingRequired),
        _ => return Err(ManagementFailure::UnsafeNativeState),
    };
    if additional_only
        && !matches!(
            saved.receipt().parts().claims,
            PairingReceiptBody::ClientBinding { .. }
        )
    {
        return Err(ManagementFailure::AuthorizationDenied);
    }
    let ordinary = pairing
        .restore(saved.key(), saved.client().parts().client.id)
        .await
        .map_err(classify_pairing)?;
    let binding = ordinary.binding().binding().map_err(classify_client)?;
    let source = Arc::new(ConsumerCredentials {
        pairing: pairing.clone(),
        binding: binding.clone(),
    });
    let operations = (kind == NativeConsumerKind::Cli).then(|| {
        Arc::new(super::tracked::ManagementOperations::new(
            journal.clone(),
            binding.clone(),
        ))
    });
    let delegate: Arc<dyn ConnectionDelegate> = match &operations {
        Some(operations) => operations.clone(),
        None => Arc::new(ReadOnlyDelegate),
    };
    let services = ClientServices {
        routes: owner.routes.clone(),
        credentials: source,
        journal,
        organization: Arc::new(CanonicalOrganization),
        delegate,
        discovery: None,
        limits: ClientLimits::default(),
        additional_capabilities: vec![
            spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
        ],
    };
    let (registry, runtime) = ClientRegistry::recover(services)
        .await
        .map_err(classify_client)?;
    let registry = Arc::new(registry);
    *owner
        .invalidator
        .registry
        .lock()
        .map_err(|_| ManagementFailure::UnsafeNativeState)? = Some(Arc::downgrade(&registry));
    if let Some(operations) = &operations {
        operations.attach(&registry).map_err(classify_client)?;
    }
    let server = registry
        .server(binding.target().server())
        .ok_or(ManagementFailure::PairingRequired)?;
    server.connect().await.map_err(classify_client)?;
    let mut state = server.state();
    let mut replica = server.replica();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if matches!(
                *state.borrow_and_update(),
                spaceterm_client::api::ConnectionState::Connected { .. }
            ) && replica.borrow_and_update().is_ready()
            {
                return Ok(());
            }
            tokio::select! {
                result = state.changed() => result.map_err(|_| ManagementFailure::Offline)?,
                result = replica.changed() => result.map_err(|_| ManagementFailure::Offline)?,
            }
        }
    })
    .await
    .map_err(|_| ManagementFailure::Offline)??;
    if let Some(operations) = &operations {
        let stream = ordinary.binding().streams().parts().ordinary;
        operations
            .tracker
            .acknowledge(stream, operations.as_ref())
            .await
            .map_err(classify_client)?;
    }
    Ok(OpenedConsumer {
        pairing: pairing.clone(),
        server,
        operations,
        kind: kind.management_kind(),
        _registry: registry,
        _runtime: runtime,
    })
}
struct ConsumerCredentials {
    pairing: Arc<NativePairingRuntime>,
    binding: PairedServerBinding,
}
impl CredentialSource for ConsumerCredentials {
    fn signing_access<'a>(
        &'a self,
        target: &'a spaceterm_client::api::ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> ClientFuture<'a, SigningAccess> {
        Box::pin(async move {
            if target != self.binding.target() || purpose != ConnectionPurpose::Ordinary {
                return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
            }
            let key = target.local_pairing()?;
            let ordinary = self
                .pairing
                .restore(&key, target.client())
                .await
                .map_err(pairing_client)?;
            Ok(SigningAccess(
                ordinary.signing_key().await.map_err(pairing_client)?,
            ))
        })
    }
}
struct PinnedReceipts;
impl ReceiptVerifier for PinnedReceipts {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidStorage))
    }
}
struct CanonicalOrganization;
impl OrganizationDecoder for CanonicalOrganization {
    fn decode(&self, payload: Payload) -> Result<OrganizationPublication, ClientFailure> {
        macro_rules! snapshot {
            ($m:expr,$p:ident) => {
                match $m.kind {
                    Some($p::message::Kind::OrganizationSnapshot(value)) => {
                        organization::$p::decode_snapshot_part(&value)
                    }
                    _ => return Err(ClientFailure::new(ClientFailureClass::InvalidPeer)),
                }
            };
        }
        let value = match payload {
            Payload::Major1(m) => snapshot!(m, v1),
            Payload::Major2(m) => snapshot!(m, v2),
        }
        .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))?;
        let part = value.part;
        Ok(OrganizationPublication::Snapshot(Box::new(SnapshotPart {
            stamp: PublicationStamp {
                request: part.request,
                request_generation: part.generation,
                transfer: part.transfer,
                index: part.index,
                total: part.total,
                cursor: OrganizationCursor {
                    server: value.state.server,
                    epoch: part.epoch,
                    revision: part.revision,
                },
            },
            state: value.state,
            runtime: value.runtime,
        })))
    }
}
struct ReadOnlyDelegate;
impl ConnectionDelegate for ReadOnlyDelegate {
    fn invalidate(&self, _: &PairedServerBinding, _: ConnectionInvalidation) {}
    fn reconcile<'a>(
        &'a self,
        _: ConnectedInfo,
        _: &'a ReplicaSnapshot,
        _: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn publication(&self, _: ConnectedInfo, _: &ReplicaSnapshot) -> Result<(), ClientFailure> {
        Ok(())
    }
    fn admit(&self, _: ConnectedInfo, _: &Payload) -> Result<OutboundClass, ClientFailure> {
        Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied))
    }
    fn receive<'a>(
        &'a self,
        _: ConnectedInfo,
        _: Payload,
        _: &'a mut dyn ProtocolChannel,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async { Err(ClientFailure::new(ClientFailureClass::InvalidPeer)) })
    }
}
#[derive(Default)]
struct ConsumerInvalidator {
    registry: Mutex<Option<std::sync::Weak<ClientRegistry>>>,
}
impl LocalUseInvalidator for ConsumerInvalidator {
    fn invalidate_workspace(
        &self,
        _: &LocalPairingKey,
        _: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Ok(vec![])
    }
    fn invalidate_attachment(
        &self,
        _: &LocalPairingKey,
        _: spaceterm_client::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure> {
        Ok(vec![])
    }
    fn invalidate_pairing(&self, _: &LocalPairingKey) {}
    fn remove_pairing<'a>(&'a self, key: &'a LocalPairingKey) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            let registry = self
                .registry
                .lock()
                .map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))?
                .as_ref()
                .and_then(std::sync::Weak::upgrade);
            if let Some(registry) = registry {
                registry.remove_suppressed(key).await?;
            }
            Ok(())
        })
    }
}
fn pairing_client(_: PairingFailure) -> ClientFailure {
    ClientFailure::new(ClientFailureClass::AuthorizationDenied)
}
fn classify_pairing(failure: PairingFailure) -> ManagementFailure {
    match failure {
        PairingFailure::PairingRevoked => ManagementFailure::Revoked,
        PairingFailure::ClientNotBound => ManagementFailure::PairingRequired,
        PairingFailure::CredentialUnavailable => ManagementFailure::CredentialUnavailable,
        PairingFailure::StorageUnavailable => ManagementFailure::StorageUnavailable,
        PairingFailure::StaleAssessment => ManagementFailure::StaleAssessment,
        _ => ManagementFailure::UnsafeNativeState,
    }
}
fn classify_client(failure: ClientFailure) -> ManagementFailure {
    match failure.class() {
        ClientFailureClass::Unavailable | ClientFailureClass::Cancelled => {
            ManagementFailure::Offline
        }
        ClientFailureClass::AuthorizationDenied => ManagementFailure::AuthorizationDenied,
        ClientFailureClass::TargetNotFound => ManagementFailure::PairingRequired,
        ClientFailureClass::StorageUnavailable => ManagementFailure::StorageUnavailable,
        ClientFailureClass::ResourceLimit => ManagementFailure::LimitExceeded,
        ClientFailureClass::Incompatible | ClientFailureClass::MissingCapability => {
            ManagementFailure::Incompatible
        }
        _ => ManagementFailure::UnsafeNativeState,
    }
}

fn progress(
    ticket: &spaceterm_client::operations::OperationTicket,
    outcome: Result<spaceterm_core::operations::Outcome, ClientFailure>,
) -> spaceterm_core::management::ManagementProgress {
    progress_binding(ticket.binding(), outcome)
}
fn progress_binding(
    binding: spaceterm_core::operations::OperationBinding,
    outcome: Result<spaceterm_core::operations::Outcome, ClientFailure>,
) -> spaceterm_core::management::ManagementProgress {
    use spaceterm_core::{
        management::{ManagementProgress, ManagementProgressState, ManagementRequestReference},
        operations::Outcome,
    };
    let state = match outcome {
        Ok(Outcome::Applied | Outcome::Assessment(_)) => ManagementProgressState::Completed,
        Ok(Outcome::Pairing(value))
            if matches!(
                *value,
                spaceterm_core::pairing::PairingOutcome::UnpairAssessed(_)
            ) =>
        {
            ManagementProgressState::Completed
        }
        Ok(_) => ManagementProgressState::Unavailable,
        Err(_) => ManagementProgressState::Pending,
    };
    ManagementProgress {
        request: ManagementRequestReference::Operation(binding),
        state,
        completed_units: u32::from(state == ManagementProgressState::Completed),
        total_units: 1,
    }
}

impl crate::cli::bindings::PairingCommandBindings for NativeConsumer {
    fn enroll<'a>(
        &'a self,
        expected: Option<crate::cli::arguments::PublicFingerprint>,
        recover_account: bool,
        _: spaceterm_host::native_management::SudoMode,
        io: &'a mut dyn crate::cli::runner::CliIo,
    ) -> ManagementFuture<'a, crate::cli::output::CliOutput> {
        Box::pin(async move { self.enroll_registered(expected, recover_account, io).await })
    }
    fn authorize(
        &self,
        _: crate::cli::arguments::PublicRequestId,
        _: crate::cli::arguments::PublicFingerprint,
    ) -> ManagementFuture<'_, spaceterm_core::management::ManagementProgress> {
        Box::pin(async {
            self.mutable()?;
            self.opened().await?;
            Err(ManagementFailure::AuthorizationUnavailable)
        })
    }
    fn list_page(
        &self,
        _: Option<crate::cli::bindings::InventoryCursor>,
    ) -> ManagementFuture<'_, crate::cli::bindings::PairingInventoryPage> {
        Box::pin(async {
            self.mutable()?;
            self.opened().await?;
            Err(ManagementFailure::NativeUnavailable)
        })
    }
    fn assess_unpair(
        &self,
        device: spaceterm_core::identity::DeviceId,
        token: Option<AssessmentToken>,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async move {
            self.mutable()?;
            // Restore supplied consent before checking fresh-assessment inventory.
            // Its original pairing may already have been removed by another token.
            let retained = if let Some(token) = token {
                Some(self.retained_assessment(&token).await?)
            } else {
                None
            };
            let runtime = self.pairing_runtime().await?;
            let inventory = runtime.inventory().await.map_err(classify_pairing)?;
            if inventory.saved.len() != 1 {
                return Err(if retained.is_some() {
                    ManagementFailure::StaleAssessment
                } else if inventory.saved.is_empty() {
                    ManagementFailure::PairingRequired
                } else {
                    ManagementFailure::UnsafeNativeState
                });
            }
            let saved = &inventory.saved[0];
            if saved.device() != device {
                return Err(if retained.is_some() {
                    ManagementFailure::StaleAssessment
                } else {
                    ManagementFailure::NativeUnavailable
                });
            }
            if !matches!(
                saved.receipt().parts().claims,
                PairingReceiptBody::ClientBinding { .. }
            ) {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            let local = runtime
                .assess_unpair(saved.key(), saved.client().parts().client.id)
                .await
                .map_err(classify_pairing)?;
            let target = spaceterm_core::pairing::PairingTarget::new(
                spaceterm_core::pairing::PairingTargetParts {
                    server: saved.key().parts().server,
                    device,
                    pairing: saved.key().parts().pairing,
                },
            )
            .map_err(|_| ManagementFailure::UnsafeNativeState)?;
            let action = ManagementAction::Unpair { target };
            if let Some(assessment) = retained {
                if assessment.action() != &action
                    || !matches!(assessment.evidence(), spaceterm_client::management_cli::AssessmentEvidence::LocalSelfUnpair { pairing, client }
                        if pairing == local.pairing() && *client == local.client())
                {
                    return Err(ManagementFailure::StaleAssessment);
                }
                return Ok(assessment);
            }
            let issued = self.now()?;
            let expires = spaceterm_core::observations::MonotonicTick::new(
                issued
                    .get()
                    .checked_add(30_000)
                    .ok_or(ManagementFailure::LimitExceeded)?,
            );
            let mut nonce = [0; 32];
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce)
                .map_err(|_| ManagementFailure::NativeUnavailable)?;
            let token = AssessmentToken::new(crate::cli::output::hex(&nonce))?;
            let evidence = spaceterm_client::management_cli::AssessmentEvidence::LocalSelfUnpair {
                pairing: local.pairing().clone(),
                client: local.client(),
            };
            let assessment = ManagementAssessment::new(token, action, evidence, issued, expires)?;
            let (directory, scope) = self.assessment_owner(None).await?;
            assessments::retain(directory, scope, assessment.clone(), issued, expires).await?;
            Ok(assessment)
        })
    }
    fn unpair(
        &self,
        confirmed: ConfirmedManagementAction,
    ) -> ManagementFuture<'_, crate::cli::bindings::UnpairReport> {
        Box::pin(async move {
            use crate::cli::bindings::{RevocationState, UnpairReport};
            self.mutable()?;
            let _serial = self
                .mutation
                .try_lock()
                .map_err(|_| ManagementFailure::OutcomePending)?;
            let assessment = confirmed.assessment();
            if self.retained_assessment(assessment.token()).await? != *assessment {
                return Err(ManagementFailure::StaleAssessment);
            }
            let ManagementAction::Unpair { target } = assessment.action() else {
                return Err(ManagementFailure::StaleAssessment);
            };
            let runtime = self.pairing_runtime().await?;
            let spaceterm_client::management_cli::AssessmentEvidence::LocalSelfUnpair {
                pairing,
                client,
            } = assessment.evidence()
            else {
                return Err(ManagementFailure::StaleAssessment);
            };
            let inventory = runtime.inventory().await.map_err(classify_pairing)?;
            let saved = inventory
                .saved
                .iter()
                .find(|saved| {
                    saved.key() == pairing
                        && saved.client().parts().client.id == *client
                        && saved.device() == target.parts().device
                })
                .ok_or(ManagementFailure::StaleAssessment)?;
            let local = runtime
                .assess_unpair(saved.key(), *client)
                .await
                .map_err(classify_pairing)?;
            if pairing != local.pairing()
                || *client != local.client()
                || target.parts().server != pairing.parts().server
                || target.parts().pairing != pairing.parts().pairing
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            let key = local.pairing().clone();
            self.consume_assessment(assessment).await?;
            assessment.validate_at(self.now()?)?;
            runtime.self_unpair(local).await.map_err(classify_pairing)?;
            let pending = runtime
                .inventory()
                .await
                .map_err(classify_pairing)?
                .revocations
                .into_iter()
                .find(|pending| pending.task.parts().local_pairing == key)
                .ok_or(ManagementFailure::Indeterminate)?;
            let result = runtime
                .retry_revocation_once(pending.task.parts().original.operation())
                .await;
            let remaining = runtime.inventory().await.map_err(classify_pairing)?;
            let retained = remaining
                .revocations
                .iter()
                .any(|pending| pending.task.parts().local_pairing == key);
            let revocation = if !retained {
                RevocationState::Confirmed
            } else if matches!(result, Err(PairingFailure::RecoveryRequired)) {
                RevocationState::Indeterminate
            } else {
                RevocationState::Pending
            };
            Ok(UnpairReport {
                server: target.parts().server,
                device: target.parts().device,
                pairing: target.parts().pairing,
                local_connection_removed: remaining.is_suppressed(&key),
                revocation,
            })
        })
    }
    fn retry_pending(&self) -> ManagementFuture<'_, Vec<crate::cli::bindings::UnpairReport>> {
        Box::pin(async {
            use crate::cli::bindings::{RevocationState, UnpairReport};
            let runtime = self.pairing_runtime().await?;
            let inventory = runtime.inventory().await.map_err(classify_pairing)?;
            let mut reports = Vec::new();
            for pending in inventory.revocations {
                let original = pending.task.parts().original;
                let result = runtime.retry_revocation_once(original.operation()).await;
                let remaining = runtime.inventory().await.map_err(classify_pairing)?;
                let retained = remaining
                    .revocations
                    .iter()
                    .any(|work| work.task.parts().original.operation() == original.operation());
                let revocation = if !retained {
                    RevocationState::Confirmed
                } else if matches!(result, Err(PairingFailure::RecoveryRequired)) {
                    RevocationState::Indeterminate
                } else {
                    RevocationState::Pending
                };
                reports.push(UnpairReport {
                    server: original.server(),
                    device: original.device(),
                    pairing: original.pairing(),
                    local_connection_removed: remaining
                        .is_suppressed(&pending.task.parts().local_pairing),
                    revocation,
                });
            }
            Ok(reports)
        })
    }
}
