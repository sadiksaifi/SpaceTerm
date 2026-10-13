//! One-shot dispatch. Waiting ends independently of service-owned admitted work.

use std::{collections::BTreeSet, sync::Arc};

use spaceterm_client::management_cli::{
    ConfirmationPrompt, ConfirmationReply, ManagementAction, ManagementFuture, ProtocolManagement,
    bindings::STATUS_READ_DEADLINE, confirm_exact,
};
use spaceterm_host::{native_management::SudoMode, startup::AccountBuildKey};

use super::{
    CliFailure,
    arguments::{CliCommand, CliInvocation, ConfirmationMode, RequestSelector},
    bindings::{
        NativeCliCommand, NativeCommandBindings, PairingCommandBindings, RegistrationAvailability,
    },
    output,
};

pub trait CliIo: Send {
    fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure>;
    fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure>;
    /// Deliberate direct enrollment asks for the exact public Server fingerprint.
    /// The pairing driver verifies the pin; this reply alone grants no authority.
    fn confirm_server_fingerprint(
        &mut self,
        _fingerprint: super::arguments::PublicFingerprint,
    ) -> Result<(), CliFailure> {
        Err(CliFailure::ConfirmationRequired)
    }
    /// Confirms a native all-client lifecycle assessment through the controlling
    /// terminal. Implementations must not silently confirm or consume stdin.
    fn confirm_native_action(
        &mut self,
        _prompt: &crate::native::NativeActionPrompt,
    ) -> Result<(), CliFailure> {
        Err(CliFailure::ConfirmationRequired)
    }
    /// Read a bounded controlling-terminal reply, never stdin.
    fn read_confirmation(
        &mut self,
        prompt: &ConfirmationPrompt,
    ) -> Result<ConfirmationReply, CliFailure>;
}
pub struct CliDependencies {
    key: Result<AccountBuildKey, CliFailure>,
    protocol: Arc<dyn ProtocolManagement>,
    native: Arc<dyn NativeCommandBindings>,
    pairing: Arc<dyn PairingCommandBindings>,
    mutation: tokio::sync::Mutex<()>,
}
impl CliDependencies {
    /// Trusted composition supplies the invoking account/build, never a flag or
    /// environment-selected registration, helper path, credential or test switch.
    pub fn new(
        key: Result<AccountBuildKey, CliFailure>,
        protocol: Arc<dyn ProtocolManagement>,
        native: Arc<dyn NativeCommandBindings>,
        pairing: Arc<dyn PairingCommandBindings>,
    ) -> Self {
        Self {
            key,
            protocol,
            native,
            pairing,
            mutation: tokio::sync::Mutex::new(()),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CliExit(u8);
impl CliExit {
    pub fn code(self) -> u8 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dispatch {
    Serve,
    Exit(CliExit),
}

pub async fn dispatch(
    invocation: CliInvocation,
    dependencies: &CliDependencies,
    io: &mut dyn CliIo,
) -> Dispatch {
    let result = run(invocation.clone(), dependencies, io).await;
    match result {
        Ok(dispatch) => dispatch,
        Err(failure) => {
            let failure = output::emit_failure(failure, invocation.json, io)
                .err()
                .unwrap_or(failure);
            Dispatch::Exit(CliExit(failure.exit_code()))
        }
    }
}
async fn run(
    invocation: CliInvocation,
    deps: &CliDependencies,
    io: &mut dyn CliIo,
) -> Result<Dispatch, CliFailure> {
    let CliInvocation {
        json,
        non_interactive,
        command,
    } = invocation;
    match command {
        CliCommand::Help => {
            output::emit_text("usage", output::USAGE, json, io)?;
            return Ok(Dispatch::Exit(CliExit(0)));
        }
        CliCommand::Version => {
            output::emit_text("version", super::bindings::CLI_VERSION, json, io)?;
            return Ok(Dispatch::Exit(CliExit(0)));
        }
        _ => {}
    }
    let key = deps.key?;
    if key.account_uid == 0 {
        return Err(CliFailure::AuthorizationDenied);
    }
    if key.account_uid == u32::MAX {
        return Err(CliFailure::UnsafeNativeState);
    }
    if command == CliCommand::Serve {
        return Ok(Dispatch::Serve);
    }
    let mode = if non_interactive {
        SudoMode::NonInteractive
    } else {
        SudoMode::Interactive
    };
    let mut exit = 0;
    let read_only = matches!(
        command,
        CliCommand::Status { .. } | CliCommand::PairList | CliCommand::UpdateCheck
    );
    let _serial = if read_only {
        None
    } else {
        Some(
            deps.mutation
                .try_lock()
                .map_err(|_| CliFailure::OutcomePending)?,
        )
    };
    let result = match command {
        CliCommand::Status {
            operation: Some(RequestSelector::Protocol(operation)),
        } => {
            let progress =
                tokio::time::timeout(STATUS_READ_DEADLINE, deps.protocol.resolve(operation))
                    .await
                    .map_err(|_| CliFailure::OutcomePending)??;
            if progress.request.operation_id() != Some(operation) {
                return Err(CliFailure::UnsafeNativeState);
            }
            output::CliOutput::Progress(progress)
        }
        CliCommand::Status {
            operation: Some(RequestSelector::Native(request)),
        } => {
            let result = tokio::time::timeout(
                STATUS_READ_DEADLINE,
                deps.native.run(
                    key,
                    NativeCliCommand::Resolve { request },
                    ConfirmationMode::Assess,
                    mode,
                    io,
                ),
            )
            .await
            .map_err(|_| CliFailure::OutcomePending)??;
            let original_request = match &result {
                output::CliOutput::NativeProgress(progress) => Some(progress.request),
                output::CliOutput::ServiceProgress(progress) => Some(*progress.request.as_bytes()),
                _ => None,
            };
            if original_request != Some(*request.as_bytes()) {
                return Err(CliFailure::UnsafeNativeState);
            }
            result
        }
        CliCommand::Status { operation: None } => {
            let native = tokio::time::timeout(STATUS_READ_DEADLINE, deps.native.inspect(key))
                .await
                .unwrap_or(Err(CliFailure::NativeUnavailable));
            let mut status = output::LocalStatus {
                native: native.as_ref().ok().cloned(),
                native_failure: native.err(),
                authenticated: None,
                negotiated_protocol_major: None,
                authentication_failure: None,
            };
            if status.native.as_ref().is_some_and(|n| {
                n.availability == RegistrationAvailability::Registered
                    && n.desired == Some(spaceterm_host::startup::DesiredServerState::Running)
            }) {
                match read_query(deps.protocol.status()).await {
                    Ok(value) => match deps.protocol.negotiated_major() {
                        Ok(major) => {
                            status.authenticated = Some(value);
                            status.negotiated_protocol_major = Some(major);
                        }
                        Err(failure) => status.authentication_failure = Some(failure),
                    },
                    Err(failure) => status.authentication_failure = Some(failure),
                }
            }
            output::CliOutput::Status(status)
        }
        CliCommand::Install { version } => {
            deps.native
                .run(
                    key,
                    NativeCliCommand::Install { version },
                    ConfirmationMode::Interactive,
                    mode,
                    io,
                )
                .await?
        }
        CliCommand::Restart { confirmation } => {
            native_confirmed(deps, key, NativeCliCommand::Restart, confirmation, mode, io).await?
        }
        CliCommand::Uninstall { confirmation } => {
            native_confirmed(
                deps,
                key,
                NativeCliCommand::Uninstall,
                confirmation,
                mode,
                io,
            )
            .await?
        }
        CliCommand::Start => {
            deps.native
                .run(
                    key,
                    NativeCliCommand::Start,
                    ConfirmationMode::Interactive,
                    mode,
                    io,
                )
                .await?
        }
        CliCommand::Stop { confirmation } => {
            native_confirmed(deps, key, NativeCliCommand::Stop, confirmation, mode, io).await?
        }
        CliCommand::RemoteAccess {
            endpoints,
            confirmation,
        } => {
            native_confirmed(
                deps,
                key,
                NativeCliCommand::RemoteAccess { endpoints },
                confirmation,
                mode,
                io,
            )
            .await?
        }
        CliCommand::UpdateCheck => {
            deps.native
                .run(
                    key,
                    NativeCliCommand::UpdateCheck,
                    ConfirmationMode::Assess,
                    mode,
                    io,
                )
                .await?
        }
        CliCommand::UpdateStage { version } => {
            deps.native
                .run(
                    key,
                    NativeCliCommand::UpdateStage { version },
                    ConfirmationMode::Interactive,
                    mode,
                    io,
                )
                .await?
        }
        CliCommand::UpdateActivate {
            installation,
            confirmation,
        } => {
            native_confirmed(
                deps,
                key,
                NativeCliCommand::UpdateActivate { installation },
                confirmation,
                mode,
                io,
            )
            .await?
        }
        CliCommand::PairEnroll {
            fingerprint,
            recover_account,
        } => {
            if non_interactive && fingerprint.is_none() {
                return Err(CliFailure::ConfirmationRequired);
            }
            deps.pairing
                .enroll(fingerprint, recover_account, mode, io)
                .await?
        }
        CliCommand::PairAuthorize {
            request,
            fingerprint,
        } => output::CliOutput::Progress(deps.pairing.authorize(request, fingerprint).await?),
        CliCommand::EndAllSessions { confirmation } => {
            require_confirmation(&confirmation, mode)?;
            let server = read_query(deps.protocol.status()).await?.parts().server;
            let action = ManagementAction::EndAllSessions { server };
            let assessment = match &confirmation {
                ConfirmationMode::Token(token) => {
                    read_query(
                        deps.protocol
                            .restore_assessment(action.clone(), token.clone()),
                    )
                    .await?
                }
                _ => read_query(deps.protocol.assess(action.clone())).await?,
            };
            if assessment.action() != &action {
                return Err(CliFailure::StaleAssessment);
            }
            if confirmation == ConfirmationMode::Assess {
                output::CliOutput::Assessment(assessment)
            } else {
                let confirmed = confirm_assessment(assessment, confirmation, io)?;
                output::CliOutput::Progress(deps.protocol.execute(confirmed).await?)
            }
        }
        CliCommand::Unpair {
            device,
            confirmation,
        } => {
            require_confirmation(&confirmation, mode)?;
            let token = match &confirmation {
                ConfirmationMode::Token(value) => Some(value.clone()),
                _ => None,
            };
            let assessment = read_query(deps.pairing.assess_unpair(device, token)).await?;
            if !matches!(assessment.action(), ManagementAction::Unpair { target } if target.parts().device == device)
            {
                return Err(CliFailure::StaleAssessment);
            }
            if confirmation == ConfirmationMode::Assess {
                output::CliOutput::Assessment(assessment)
            } else {
                let ManagementAction::Unpair { target } = assessment.action() else {
                    return Err(CliFailure::StaleAssessment);
                };
                let target = target.clone();
                let confirmed = confirm_assessment(assessment, confirmation, io)?;
                let report = deps.pairing.unpair(confirmed).await?;
                if (report.server, report.device, report.pairing)
                    != (
                        target.parts().server,
                        target.parts().device,
                        target.parts().pairing,
                    )
                {
                    return Err(CliFailure::UnsafeNativeState);
                }
                output::CliOutput::Unpair(report)
            }
        }
        CliCommand::RetryPendingUnpair => {
            let reports = deps.pairing.retry_pending().await?;
            if reports.len() > spaceterm_core::pairing::MAX_PENDING_REVOCATIONS {
                return Err(CliFailure::LimitExceeded);
            }
            for report in reports {
                let output = output::CliOutput::Unpair(report);
                exit = exit.max(output.exit_code());
                output::emit(&output, json, io)?;
            }
            return Ok(Dispatch::Exit(CliExit(exit)));
        }
        CliCommand::PairList => {
            let mut cursor = None;
            let mut total = 0;
            let mut seen = BTreeSet::new();
            let mut server = None;
            loop {
                let page = read_query(deps.pairing.list_page(cursor.clone())).await?;
                if page.devices.len() > 128
                    || total + page.devices.len() > spaceterm_core::pairing::MAX_PAIRING_LIFETIMES
                {
                    return Err(CliFailure::LimitExceeded);
                }
                if page.next.is_some() && (page.devices.is_empty() || page.next == cursor) {
                    return Err(CliFailure::UnsafeNativeState);
                }
                for device in &page.devices {
                    if device.server.bytes() == [0; 16]
                        || device.device.bytes() == [0; 16]
                        || device.pairing.server() != device.server
                        || server.is_some_and(|s| s != device.server)
                        || !seen.insert(device.pairing)
                    {
                        return Err(CliFailure::UnsafeNativeState);
                    }
                    server = Some(device.server);
                }
                total += page.devices.len();
                output::emit(&output::CliOutput::Devices(page.devices), json, io)?;
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
            }
            return Ok(Dispatch::Exit(CliExit(0)));
        }
        CliCommand::Help | CliCommand::Version | CliCommand::Serve => {
            return Err(CliFailure::Usage);
        }
    };
    exit = result.exit_code();
    output::emit(&result, json, io)?;
    Ok(Dispatch::Exit(CliExit(exit)))
}
fn require_confirmation(confirmation: &ConfirmationMode, mode: SudoMode) -> Result<(), CliFailure> {
    if mode == SudoMode::NonInteractive && *confirmation == ConfirmationMode::Interactive {
        Err(CliFailure::ConfirmationRequired)
    } else {
        Ok(())
    }
}
async fn native_confirmed(
    deps: &CliDependencies,
    key: AccountBuildKey,
    command: NativeCliCommand,
    confirmation: ConfirmationMode,
    mode: SudoMode,
    io: &mut dyn CliIo,
) -> Result<output::CliOutput, CliFailure> {
    require_confirmation(&confirmation, mode)?;
    deps.native.run(key, command, confirmation, mode, io).await
}
fn confirm_assessment(
    assessment: spaceterm_client::management_cli::ManagementAssessment,
    confirmation: ConfirmationMode,
    io: &mut dyn CliIo,
) -> Result<spaceterm_client::management_cli::ConfirmedManagementAction, CliFailure> {
    let reply = match confirmation {
        ConfirmationMode::Token(token) => ConfirmationReply::new(token),
        ConfirmationMode::Interactive => {
            io.read_confirmation(&ConfirmationPrompt::new(assessment.clone()))?
        }
        ConfirmationMode::Assess => return Err(CliFailure::ConfirmationRequired),
    };
    confirm_exact(assessment, reply)
}

async fn read_query<T>(future: ManagementFuture<'_, T>) -> Result<T, CliFailure> {
    tokio::time::timeout(STATUS_READ_DEADLINE, future)
        .await
        .map_err(|_| CliFailure::Offline)?
}
