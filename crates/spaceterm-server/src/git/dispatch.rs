//! Git dispatch composes the ordinary authenticated loop and the real actor.
use super::{
    authority::AuthenticatedGitPrincipal,
    reads::GitStatusSubscription,
    writes::{GitService, GitSubmission},
};
use crate::{
    api::{ServerHandle, VerifiedPrincipal},
    contracts::{ServerFailure, ServerFailureClass, ServerFuture},
    failure::failure,
    protocol_dispatch::{ActorOrdinaryDispatch, OrdinaryDispatchPort},
};
use spaceterm_core::{
    operation_ledger_contracts::LedgerCodec,
    operations::{Command, OperationLookup},
    worktrees::{
        GitFailure,
        queries::{GitQueryRequest, GitQueryResponse},
    },
};
use spaceterm_protocol::{
    codec::Payload,
    convert::{git, operations},
    wire::{v1, v2},
};
use std::sync::Arc;

/// Install in ProtocolDispatcher's existing ordinary lane. It neither opens a
/// transport nor constructs a principal from client-supplied identity fields.
pub struct GitOrdinaryDispatch {
    service: GitService,
    codec: Arc<dyn LedgerCodec>,
    ordinary: ActorOrdinaryDispatch,
}
impl GitOrdinaryDispatch {
    pub fn new(service: GitService, codec: Arc<dyn LedgerCodec>) -> Self {
        Self {
            service,
            ordinary: ActorOrdinaryDispatch::new(codec.clone()),
            codec,
        }
    }
    /// The connection publication owner retains this receiver and compares each
    /// update's source against the actor snapshot at final unsigned dequeue.
    pub async fn subscribe_status(
        &self,
        principal: VerifiedPrincipal,
        request: &GitQueryRequest,
    ) -> Result<GitStatusSubscription, GitFailure> {
        let spaceterm_core::worktrees::GitQuery::Status { target } = request.query else {
            return Err(GitFailure::InvalidResponse);
        };
        self.service
            .subscribe_status(git_principal(principal), target)
            .await
    }
}
impl OrdinaryDispatchPort for GitOrdinaryDispatch {
    fn dispatch(
        &self,
        principal: VerifiedPrincipal,
        payload: Payload,
        handle: ServerHandle,
    ) -> ServerFuture<'_, Result<Option<Payload>, ServerFailure>> {
        Box::pin(async move {
            let major = match &payload {
                Payload::Major1(_) => 1,
                Payload::Major2(_) => 2,
            };
            if matches!(
                &payload,
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::GitQuery(_))
                }) | Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::GitQuery(_))
                })
            ) {
                let request = git::decode_query(&payload, principal.server())
                    .map_err(crate::adapters::transport::protocol_failure)?;
                let response = if request.subscribe {
                    // A one-reply dispatcher cannot claim a live subscription.
                    // Its connection owner must use subscribe_status above.
                    GitQueryResponse {
                        correlation: request.correlation,
                        scope: request.scope,
                        result: Err(GitFailure::Busy),
                    }
                } else {
                    self.service
                        .query_request(git_principal(principal), request)
                        .await
                };
                return git::encode_response(major, &response, principal.server())
                    .map(Some)
                    .map_err(crate::adapters::transport::protocol_failure);
            }
            let request = match &payload {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::Operation(value)),
                }) => Some(
                    operations::v1::decode_request(
                        value,
                        principal.pairing(),
                        principal.client(),
                        self.codec.as_ref(),
                    )
                    .map_err(crate::adapters::transport::protocol_failure)?,
                ),
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::Operation(value)),
                }) => Some(
                    operations::v2::decode_request(
                        value,
                        principal.pairing(),
                        principal.client(),
                        self.codec.as_ref(),
                    )
                    .map_err(crate::adapters::transport::protocol_failure)?,
                ),
                _ => None,
            };
            if let Some(request) = request
                && let Command::Worktree(command) = request.command()
            {
                let binding = request.binding();
                let submitted = self
                    .service
                    .submit(git_principal(principal), binding.id, command.clone())
                    .await
                    .map_err(crate::service::classify_git_failure)?;
                let lookup = match submitted {
                    GitSubmission::Pending(accepted) if accepted == binding => {
                        OperationLookup::Pending
                    }
                    GitSubmission::Retained(_) => handle.lookup_state(principal, binding).await?,
                    _ => return Err(failure(ServerFailureClass::RecoveryRequired)),
                };
                return match major {
                    1 => operations::v1::encode_lookup_result(binding, &lookup).map(|value| {
                        Some(Payload::Major1(v1::Message {
                            kind: Some(v1::message::Kind::OperationLookupResult(value)),
                        }))
                    }),
                    _ => operations::v2::encode_lookup_result(binding, &lookup).map(|value| {
                        Some(Payload::Major2(v2::Message {
                            kind: Some(v2::message::Kind::OperationLookupResult(value)),
                        }))
                    }),
                }
                .map_err(crate::adapters::transport::protocol_failure);
            }
            self.ordinary.dispatch(principal, payload, handle).await
        })
    }
}
fn git_principal(principal: VerifiedPrincipal) -> AuthenticatedGitPrincipal {
    AuthenticatedGitPrincipal::from_verified(principal)
}
