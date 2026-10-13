//! Adapter to the client-core worker. No competing operation identity or journal.

use std::{sync::Arc, time::Duration};

use spaceterm_core::{
    authority::{ConnectionBinding, ServerEpoch},
    identity::{DeviceId, OperationStreamId, ServerId},
    management::{ManagementProgress, ServerStatus},
    observations::RuntimeClock,
    operations::OperationId,
};
use spaceterm_protocol::connection::AuthenticatedConnection;

use super::{
    confirmation::{
        AssessmentEvidence, AssessmentToken, ConfirmedManagementAction, ManagementAction,
        ManagementAssessment,
    },
    ports::{
        ManagementBackend, ManagementClientKind, ManagementFailure, ManagementFuture,
        ProtocolManagement, StatusBackend, StatusManagement, check_operation, check_server,
    },
    status::StatusSubscription,
};

pub const STATUS_READ_DEADLINE: Duration = Duration::from_secs(5);

/// Readiness derived only from an ordinary authenticated protocol handshake and
/// an existing journal binding. The backend invalidates it on loss/revocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedManagementContext {
    stream: OperationStreamId,
    device: DeviceId,
    epoch: ServerEpoch,
    major: u32,
    transcript_binding: [u8; 32],
    connection_binding: Option<ConnectionBinding>,
    kind: ManagementClientKind,
}
impl AuthenticatedManagementContext {
    /// Capture authenticated handshake facts. The transcript does not supply the
    /// canonical connection generation, so this alone cannot admit a subscription.
    pub fn from_connection(
        connection: &AuthenticatedConnection,
        stream: OperationStreamId,
        kind: ManagementClientKind,
    ) -> Result<Self, ManagementFailure> {
        let principal = connection.principal();
        if principal.server != stream.server().bytes()
            || principal.pairing != local_bytes(stream.pairing().local())
            || principal.client != local_bytes(stream.client().local())
            || principal.device == [0; 16]
            || connection.epoch() == 0
            || stream.server().bytes() == [0; 16]
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        Ok(Self {
            stream,
            device: DeviceId::new(principal.device),
            epoch: ServerEpoch::new(connection.epoch()),
            major: connection.major(),
            transcript_binding: connection.binding(),
            connection_binding: None,
            kind,
        })
    }
    /// Attach the current canonical binding supplied by the authenticated
    /// client-core/server-core handoff. Never derive a generation from the
    /// transcript or from the subscription returned by the backend.
    pub fn with_connection_binding(
        mut self,
        binding: ConnectionBinding,
    ) -> Result<Self, ManagementFailure> {
        if binding.client != self.stream.client() || binding.generation.get() == 0 {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        self.connection_binding = Some(binding);
        Ok(self)
    }
    pub fn connection_binding(self) -> Result<ConnectionBinding, ManagementFailure> {
        self.connection_binding
            .ok_or(ManagementFailure::UnsafeNativeState)
    }
    pub fn stream(self) -> OperationStreamId {
        self.stream
    }
    pub fn server(self) -> ServerId {
        self.stream.server()
    }
    pub fn device(self) -> DeviceId {
        self.device
    }
    pub fn epoch(self) -> ServerEpoch {
        self.epoch
    }
    pub fn major(self) -> u32 {
        self.major
    }
    pub fn kind(self) -> ManagementClientKind {
        self.kind
    }
}
fn local_bytes(local: u64) -> [u8; 16] {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&local.to_be_bytes());
    bytes
}

pub struct TrackedProtocolManagement<B> {
    backend: B,
    /// The assessment provider and clock share one comparison origin. Restoring
    /// an old token must retain its original ticks, not assign a fresh receipt tick.
    clock: Arc<dyn RuntimeClock>,
    mutation: tokio::sync::Mutex<()>,
}
impl<B: ManagementBackend> TrackedProtocolManagement<B> {
    pub fn new(backend: B, clock: Arc<dyn RuntimeClock>) -> Self {
        Self {
            backend,
            clock,
            mutation: tokio::sync::Mutex::new(()),
        }
    }
    fn mutable_context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        let context = self.backend.context()?;
        if context.kind != ManagementClientKind::Cli {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(context)
    }
    fn validate_assessment(
        &self,
        value: &ManagementAssessment,
        action: &ManagementAction,
        context: AuthenticatedManagementContext,
    ) -> Result<(), ManagementFailure> {
        check_server(value.action().server(), context.server())?;
        if value.action() != action {
            return Err(ManagementFailure::StaleAssessment);
        }
        if value.epoch().is_some_and(|epoch| epoch != context.epoch) {
            return Err(ManagementFailure::StaleAssessment);
        }
        match value.evidence() {
            AssessmentEvidence::EndAllSessions(v) => {
                check_operation(v.reference().operation, context.stream)?
            }
            AssessmentEvidence::Unpair(v) => {
                check_operation(v.parts().reference.operation, context.stream)?
            }
            _ => {}
        }
        value.validate_at(
            self.clock
                .now()
                .map_err(|_| ManagementFailure::StaleAssessment)?,
        )
    }
    fn same_context(
        &self,
        before: AuthenticatedManagementContext,
    ) -> Result<(), ManagementFailure> {
        if self.backend.context()? != before {
            return Err(ManagementFailure::Offline);
        }
        Ok(())
    }
    fn validate_progress(
        &self,
        progress: ManagementProgress,
        context: AuthenticatedManagementContext,
        expected: Option<OperationId>,
    ) -> Result<ManagementProgress, ManagementFailure> {
        let Some(operation) = progress.request.operation_id() else {
            return Err(ManagementFailure::UnsafeNativeState);
        };
        check_operation(operation, context.stream)?;
        if expected.is_some_and(|id| id != operation)
            || progress.completed_units > progress.total_units
        {
            return Err(ManagementFailure::UnsafeNativeState);
        }
        Ok(progress)
    }
}
impl<B: ManagementBackend> ProtocolManagement for TrackedProtocolManagement<B> {
    fn negotiated_major(&self) -> Result<u32, ManagementFailure> {
        Ok(self.backend.context()?.major())
    }
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(async {
            let context = self.backend.context()?;
            let status = tokio::time::timeout(STATUS_READ_DEADLINE, self.backend.status())
                .await
                .map_err(|_| ManagementFailure::Offline)??;
            self.same_context(context)?;
            check_server(status.parts().server, context.server())?;
            if status.parts().epoch != context.epoch {
                return Err(ManagementFailure::Offline);
            }
            Ok(status)
        })
    }
    fn assess(&self, action: ManagementAction) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async move {
            let context = self.mutable_context()?;
            check_server(action.server(), context.server())?;
            let value =
                tokio::time::timeout(STATUS_READ_DEADLINE, self.backend.assess(action.clone()))
                    .await
                    .map_err(|_| ManagementFailure::Offline)??;
            self.same_context(context)?;
            self.validate_assessment(&value, &action, context)?;
            Ok(value)
        })
    }
    fn restore_assessment(
        &self,
        action: ManagementAction,
        token: AssessmentToken,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        Box::pin(async move {
            let context = self.mutable_context()?;
            check_server(action.server(), context.server())?;
            let value = tokio::time::timeout(
                STATUS_READ_DEADLINE,
                self.backend
                    .restore_assessment(action.clone(), token.clone()),
            )
            .await
            .map_err(|_| ManagementFailure::Offline)??;
            self.same_context(context)?;
            self.validate_assessment(&value, &action, context)?;
            if value.token() != &token {
                return Err(ManagementFailure::StaleAssessment);
            }
            Ok(value)
        })
    }
    fn execute(
        &self,
        confirmed: ConfirmedManagementAction,
    ) -> ManagementFuture<'_, ManagementProgress> {
        Box::pin(async move {
            // The Tokio mutex guard is Send and remains held through submission.
            let _serial = self
                .mutation
                .try_lock()
                .map_err(|_| ManagementFailure::OutcomePending)?;
            let context = self.mutable_context()?;
            let assessment = confirmed.assessment();
            self.validate_assessment(assessment, assessment.action(), context)?;
            // Self-unpair must first remove ordinary local use and retain the
            // reserved restricted task through the pairing journal facade.
            if matches!(assessment.action(), ManagementAction::Unpair { target } if target.parts().device == context.device())
            {
                return Err(ManagementFailure::AuthorizationDenied);
            }
            let progress = self.backend.submit(assessment.command()).await?;
            // Do not turn a lost connection into a new operation or discard an
            // already-durable result. The tracker owns post-submission recovery.
            self.validate_progress(progress, context, None)
        })
    }
    fn resolve(&self, operation: OperationId) -> ManagementFuture<'_, ManagementProgress> {
        Box::pin(async move {
            let context = self.mutable_context()?;
            check_operation(operation, context.stream)?;
            let progress =
                tokio::time::timeout(STATUS_READ_DEADLINE, self.backend.resolve(operation))
                    .await
                    .map_err(|_| ManagementFailure::OutcomePending)??;
            self.validate_progress(progress, context, Some(operation))
        })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async {
            let context = self.backend.context()?;
            context.connection_binding()?;
            let subscription =
                tokio::time::timeout(STATUS_READ_DEADLINE, self.backend.subscribe_status())
                    .await
                    .map_err(|_| ManagementFailure::Offline)??;
            self.same_context(context)?;
            check_subscription(&subscription, context)?;
            Ok(subscription)
        })
    }
}

/// The status companion's facade. The narrowed backend type is the first
/// barrier; the per-call Client-kind check keeps a CLI or desktop binding from
/// being presented through the companion's identity.
pub struct StatusOnlyManagement<B> {
    backend: B,
}
impl<B: StatusBackend> StatusOnlyManagement<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }
    fn companion_context(&self) -> Result<AuthenticatedManagementContext, ManagementFailure> {
        let context = self.backend.context()?;
        if context.kind != ManagementClientKind::StatusCompanion {
            return Err(ManagementFailure::AuthorizationDenied);
        }
        Ok(context)
    }
    fn unchanged(&self, before: AuthenticatedManagementContext) -> Result<(), ManagementFailure> {
        if self.backend.context()? != before {
            return Err(ManagementFailure::Offline);
        }
        Ok(())
    }
}
impl<B: StatusBackend> StatusManagement for StatusOnlyManagement<B> {
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(async {
            let context = self.companion_context()?;
            let status = tokio::time::timeout(STATUS_READ_DEADLINE, self.backend.status())
                .await
                .map_err(|_| ManagementFailure::Offline)??;
            self.unchanged(context)?;
            check_server(status.parts().server, context.server())?;
            if status.parts().epoch != context.epoch {
                return Err(ManagementFailure::Offline);
            }
            Ok(status)
        })
    }
    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async {
            let context = self.companion_context()?;
            context.connection_binding()?;
            let subscription =
                tokio::time::timeout(STATUS_READ_DEADLINE, self.backend.subscribe_status())
                    .await
                    .map_err(|_| ManagementFailure::Offline)??;
            self.unchanged(context)?;
            check_subscription(&subscription, context)?;
            Ok(subscription)
        })
    }
}

fn check_subscription(
    subscription: &StatusSubscription,
    context: AuthenticatedManagementContext,
) -> Result<(), ManagementFailure> {
    check_server(subscription.server(), context.server())?;
    if subscription.connection() != context.connection_binding()? {
        return Err(ManagementFailure::UnsafeNativeState);
    }
    Ok(())
}
