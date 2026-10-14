use super::ServerQueries;
use crate::{
    api::{ConnectedInfo, ConnectionCapabilities, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass, invalid_peer, resource_limit},
    journal::JournalController,
    registry::same_connection,
};
use spaceterm_protocol::{
    codec::Payload,
    limits::MAX_ACCOUNT_RUNTIME_BYTES,
    negotiation::{CAPABILITY_ACCOUNT_RUNTIME, CAPABILITY_REMOTE_NATIVE_LIFECYCLE},
    wire::{common, v1, v2},
};
use std::sync::Arc;

impl ServerQueries {
    /// Checks the exact saved binding and canonical journal without exposing authority tokens.
    pub fn validate_owner(
        &self,
        binding: &PairedServerBinding,
        journal: &Arc<JournalController>,
    ) -> Result<(), ClientFailure> {
        self.validate_recovery_owner(binding, journal)?;
        if !self.journal.ordinary_use_allowed(binding)? {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        Ok(())
    }

    /// Checks only the original binding and canonical journal identity, even after sealing.
    /// Success grants no call authority or access to another operation's recovery record.
    pub fn validate_recovery_owner(
        &self,
        binding: &PairedServerBinding,
        journal: &Arc<JournalController>,
    ) -> Result<(), ClientFailure> {
        if !Arc::ptr_eq(&self.journal, journal) {
            return Err(ClientFailure::new(ClientFailureClass::InvalidStorage));
        }
        if &self.server.binding != binding {
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        Ok(())
    }

    /// Rejects a known-incompatible peer. Success does not establish connection readiness.
    pub fn check_compatible(&self) -> Result<(), ClientFailure> {
        self.validate_owner(&self.server.binding, &self.journal)?;
        if matches!(
            *self.server.state().borrow(),
            crate::api::ConnectionState::Incompatible { .. }
        ) {
            return Err(ClientFailure::new(ClientFailureClass::Incompatible));
        }
        Ok(())
    }

    /// Returns capabilities only while this worker and canonical pairing remain ready.
    pub fn capabilities(&self) -> Result<ConnectionCapabilities, ClientFailure> {
        Ok(self.info()?.capabilities)
    }

    /// Returns the authenticated server epoch while this worker and pairing remain ready.
    pub fn server_epoch(&self) -> Result<spaceterm_core::authority::ServerEpoch, ClientFailure> {
        Ok(self.info()?.epoch)
    }

    /// Calls the account runtime through this worker's authenticated connection.
    /// The native consumer owns the closed body grammar and result validation.
    /// Cancellation removes queued admission, but cannot undo a transmitted call.
    pub async fn account_runtime(&self, body: &[u8]) -> Result<Vec<u8>, ClientFailure> {
        let info = self.account_runtime_info(body)?;
        self.account_runtime_request(info, body).await
    }

    /// Calls native lifecycle operations only on a connection advertising both runtime capabilities.
    /// Capability admission and final connection fencing use the same captured connection.
    pub async fn native_account_runtime(&self, body: &[u8]) -> Result<Vec<u8>, ClientFailure> {
        let info = self.account_runtime_info(body)?;
        if !info
            .capabilities
            .supports(CAPABILITY_REMOTE_NATIVE_LIFECYCLE)
        {
            return Err(ClientFailure::new(ClientFailureClass::MissingCapability));
        }
        self.account_runtime_request(info, body).await
    }

    fn account_runtime_info(&self, body: &[u8]) -> Result<ConnectedInfo, ClientFailure> {
        if body.is_empty() {
            return Err(invalid_peer());
        }
        if body.len() > MAX_ACCOUNT_RUNTIME_BYTES {
            return Err(resource_limit());
        }
        let info = self.info()?;
        if !info.capabilities.supports(CAPABILITY_ACCOUNT_RUNTIME) {
            return Err(ClientFailure::new(ClientFailureClass::MissingCapability));
        }
        Ok(info)
    }

    async fn account_runtime_request(
        &self,
        info: ConnectedInfo,
        body: &[u8],
    ) -> Result<Vec<u8>, ClientFailure> {
        let reply = self
            .request(info, |request| {
                let message = common::AccountRuntimeMessage {
                    request,
                    body: body.to_vec(),
                    request_generation: info.attempt,
                };
                match info.major {
                    1 => Ok(Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::AccountRuntimeRequest(message)),
                    })),
                    2 => Ok(Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::AccountRuntimeRequest(message)),
                    })),
                    _ => Err(invalid_peer()),
                }
            })
            .await?;
        let message = match reply {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeReply(message)),
            })
            | Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
            }) => message,
            _ => return Err(invalid_peer()),
        };
        if !same_connection(info, self.info()?) {
            return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
        }
        Ok(message.body)
    }
}
