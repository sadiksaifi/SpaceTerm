//! One-shot dispatch. Waiting ends independently of service-owned admitted work.

use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use spaceterm_client::management_cli::{
    ConfirmationPrompt, ConfirmationReply, ManagementAction, ManagementFuture, ProtocolManagement,
    bindings::STATUS_READ_DEADLINE, confirm_exact,
};
use spaceterm_host::{native_management::SudoMode, startup::AccountBuildKey};

use super::{
    CliFailure,
    arguments::{CliCommand, CliInvocation, ConfirmationMode, RequestSelector},
    bindings::{
        NativeCliCommand, NativeCommandBindings, PairingAuthorizationAdmission,
        PairingAuthorizationFailure, PairingCommandBindings, RegistrationAvailability,
    },
    output,
};

pub trait CliIo: Send {
    fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure>;
    fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure>;
    /// Verify BOTH stdin and stdout are private anonymous pipes before consuming
    /// a bounded public claim JSON document. Never read a terminal or a file.
    fn read_authorization_claim(&mut self) -> Result<zeroize::Zeroizing<Vec<u8>>, CliFailure> {
        Err(CliFailure::AuthorizationUnavailable)
    }
    /// Immediate delivery adapter for nonblocking byte stores. Implementations
    /// must not wait for I/O. OS pipe adapters override the asynchronous method.
    /// The default cannot fall back to ordinary stdout or a JSON envelope.
    fn write_authorization_delivery(&mut self, _bytes: &[u8]) -> Result<(), CliFailure> {
        Err(CliFailure::AuthorizationUnavailable)
    }
    /// Cancellable raw delivery. Readiness waits occur outside the invocation's
    /// output gate; only an immediate write can compete with interruption.
    fn write_authorization_delivery_async<'a>(
        &'a mut self,
        bytes: &'a [u8],
        signal: &'a CliSignalContext,
    ) -> ManagementFuture<'a, ()> {
        Box::pin(
            async move { signal.deliver_immediate(|| self.write_authorization_delivery(bytes)) },
        )
    }
    /// Raw-mode diagnostics carry only typed public metadata on stderr. Pipe
    /// adapters bound this write independently of the raw stdout channel.
    fn write_authorization_diagnostic(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        self.write_stderr(bytes)
    }
    /// Shares the invocation's nonblocking endpoint leases with its raw writer.
    fn write_authorization_diagnostic_scoped(
        &mut self,
        bytes: &[u8],
        _signal: &CliSignalContext,
    ) -> Result<(), CliFailure> {
        self.write_authorization_diagnostic(bytes)
    }
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
#[derive(Default)]
enum AuthorizationOutputState {
    #[default]
    Pending,
    Delivered,
    Interrupted,
    Failed,
}

/// One dispatch and its signal handler share this context, never dependencies.
/// Creating a context does not admit a request or grant output authority.
#[derive(Clone)]
pub struct CliSignalContext {
    invocation: CliInvocation,
    state: Arc<InvocationOutput>,
}
#[derive(Default)]
struct InvocationOutput {
    admission: PairingAuthorizationAdmission,
    authorization: Mutex<AuthorizationOutputState>,
    interrupted: tokio::sync::Notify,
    diagnostic_complete: AtomicBool,
    diagnostic_finished: tokio::sync::Notify,
    #[cfg(unix)]
    pipe_flags: super::pairing_pipe::InvocationPipeFlags,
}
struct InterruptionDiagnostic<'a>(&'a InvocationOutput);
impl Drop for InterruptionDiagnostic<'_> {
    fn drop(&mut self) {
        // The Interrupted gate already forbids any further raw write. Restore
        // inherited flags before process::exit, which will skip context Drop.
        #[cfg(unix)]
        self.0.pipe_flags.restore();
        self.0.diagnostic_complete.store(true, Ordering::Release);
        self.0.diagnostic_finished.notify_waiters();
    }
}
impl CliSignalContext {
    pub fn new(invocation: CliInvocation) -> Self {
        Self {
            invocation,
            state: Arc::default(),
        }
    }

    pub fn pairing_authorization_admission(&self) -> PairingAuthorizationAdmission {
        self.state.admission.clone()
    }

    /// Reports composition failure before dispatch or a signal handler starts.
    /// Raw mode uses its bounded diagnostic channel; normal envelopes are unchanged.
    pub fn emit_initial_failure(
        &self,
        failure: CliFailure,
        io: &mut dyn CliIo,
    ) -> Result<(), CliFailure> {
        let result = if matches!(
            self.invocation.command,
            CliCommand::PairAuthorizeClaim { .. }
        ) {
            output::emit_authorization_failure_scoped(
                &PairingAuthorizationFailure::new(failure, self.state.admission.request()),
                self.invocation.json,
                io,
                self,
            )
        } else {
            output::emit_failure(failure, self.invocation.json, io)
        };
        self.finish_output();
        result
    }

    fn finish_output(&self) {
        // Task cancellation and runtime shutdown need not destroy a signal
        // clone before process exit. Terminal dispatch restores flags itself.
        #[cfg(unix)]
        self.state.pipe_flags.restore();
    }

    /// Returns false when this invocation already selected its final output.
    /// No diagnostic write, readiness wait or producer effect holds the gate.
    pub fn emit_interruption(
        &self,
        failure: CliFailure,
        io: &mut dyn CliIo,
    ) -> Result<bool, CliFailure> {
        if !matches!(
            self.invocation.command,
            CliCommand::PairAuthorizeClaim { .. }
        ) {
            output::emit_failure(failure, self.invocation.json, io)?;
            return Ok(true);
        }
        {
            let mut state = self
                .state
                .authorization
                .lock()
                .map_err(|_| CliFailure::UnsafeNativeState)?;
            match *state {
                AuthorizationOutputState::Delivered | AuthorizationOutputState::Failed => {
                    return Ok(false);
                }
                AuthorizationOutputState::Interrupted => return Ok(true),
                AuthorizationOutputState::Pending => {}
            }
            *state = AuthorizationOutputState::Interrupted;
        }
        let _diagnostic = InterruptionDiagnostic(&self.state);
        self.state.interrupted.notify_waiters();
        output::emit_authorization_failure_scoped(
            &PairingAuthorizationFailure::new(failure, self.state.admission.request()),
            self.invocation.json,
            io,
            self,
        )?;
        Ok(true)
    }

    #[cfg(unix)]
    pub(crate) fn keep_nonblocking(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
    ) -> Result<(), CliFailure> {
        self.state.pipe_flags.keep(fd)
    }

    async fn wait_interruption_diagnostic(&self) {
        loop {
            let notified = self.state.diagnostic_finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let interrupted = self
                .state
                .authorization
                .lock()
                .is_ok_and(|state| matches!(*state, AuthorizationOutputState::Interrupted));
            if !interrupted || self.state.diagnostic_complete.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    /// Compatibility for immediate, nonblocking byte stores. Production OS
    /// output uses try_delivery_write and never invokes a blocking Write here.
    fn deliver_immediate(
        &self,
        write: impl FnOnce() -> Result<(), CliFailure>,
    ) -> Result<(), CliFailure> {
        let mut state = self
            .state
            .authorization
            .lock()
            .map_err(|_| CliFailure::UnsafeNativeState)?;
        match *state {
            AuthorizationOutputState::Pending => {}
            AuthorizationOutputState::Interrupted => return Err(CliFailure::OutcomePending),
            _ => return Err(CliFailure::UnsafeNativeState),
        }
        write()?;
        *state = AuthorizationOutputState::Delivered;
        Ok(())
    }

    /// The callback must be one nonblocking syscall, never write_all or flush.
    /// The final accepted byte and interruption have one ordering point.
    pub(crate) fn try_delivery_write(
        &self,
        remaining: usize,
        write: impl FnOnce() -> std::io::Result<usize>,
    ) -> Result<std::io::Result<usize>, CliFailure> {
        let mut state = self
            .state
            .authorization
            .lock()
            .map_err(|_| CliFailure::UnsafeNativeState)?;
        match *state {
            AuthorizationOutputState::Pending => {}
            AuthorizationOutputState::Interrupted => return Err(CliFailure::OutcomePending),
            _ => return Err(CliFailure::UnsafeNativeState),
        }
        let result = write();
        if let Ok(count) = result {
            if count == 0 || count > remaining {
                return Err(CliFailure::IoUnavailable);
            }
            if count == remaining {
                *state = AuthorizationOutputState::Delivered;
            }
        }
        Ok(result)
    }

    pub(crate) async fn interrupted(&self) {
        loop {
            // Register before inspecting state so notify_waiters cannot be lost.
            let notified = self.state.interrupted.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .state
                .authorization
                .lock()
                .is_ok_and(|state| matches!(*state, AuthorizationOutputState::Interrupted))
            {
                return;
            }
            notified.await;
        }
    }

    fn emit_authorization_failure(
        &self,
        failure: PairingAuthorizationFailure,
        io: &mut dyn CliIo,
    ) -> Result<Dispatch, CliFailure> {
        {
            let mut state = self
                .state
                .authorization
                .lock()
                .map_err(|_| CliFailure::UnsafeNativeState)?;
            match *state {
                AuthorizationOutputState::Interrupted => {
                    return Ok(Dispatch::Exit(CliExit(
                        CliFailure::OutcomePending.exit_code(),
                    )));
                }
                AuthorizationOutputState::Delivered => return Ok(Dispatch::Exit(CliExit(0))),
                AuthorizationOutputState::Failed => {
                    return Ok(Dispatch::Exit(CliExit(failure.failure.exit_code())));
                }
                AuthorizationOutputState::Pending => *state = AuthorizationOutputState::Failed,
            }
        }
        let classification =
            output::emit_authorization_failure_scoped(&failure, self.invocation.json, io, self)
                .err()
                .unwrap_or(failure.failure);
        Ok(Dispatch::Exit(CliExit(classification.exit_code())))
    }
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
    dispatch_with_context(&CliSignalContext::new(invocation), dependencies, io).await
}

/// Dispatch with the context shared by this invocation's independent handler.
/// Use a new context for every command, including when dependencies are reused.
pub async fn dispatch_with_context(
    signal: &CliSignalContext,
    dependencies: &CliDependencies,
    io: &mut dyn CliIo,
) -> Dispatch {
    let invocation = &signal.invocation;
    let result = run(invocation.clone(), dependencies, io, signal).await;
    let dispatch = match result {
        Ok(dispatch) => dispatch,
        Err(failure) => {
            if matches!(invocation.command, CliCommand::PairAuthorizeClaim { .. }) {
                signal
                    .emit_authorization_failure(
                        PairingAuthorizationFailure::new(failure, signal.state.admission.request()),
                        io,
                    )
                    .unwrap_or_else(|failure| Dispatch::Exit(CliExit(failure.exit_code())))
            } else {
                let failure = output::emit_failure(failure, invocation.json, io)
                    .err()
                    .unwrap_or(failure);
                Dispatch::Exit(CliExit(failure.exit_code()))
            }
        }
    };
    // The executable may abort its signal task after this returns. Wait until
    // that task has completed the bounded diagnostic, without holding the gate.
    signal.wait_interruption_diagnostic().await;
    signal.finish_output();
    dispatch
}
/// Ends only this client's wait when interrupted. Admitted host work retains its
/// original durable request; interruption neither acknowledges nor reissues it.
pub async fn dispatch_until(
    invocation: CliInvocation,
    dependencies: &CliDependencies,
    io: &mut dyn CliIo,
    interrupted: impl std::future::Future<Output = ()>,
) -> Dispatch {
    dispatch_until_with_context(
        &CliSignalContext::new(invocation),
        dependencies,
        io,
        interrupted,
    )
    .await
}

pub async fn dispatch_until_with_context(
    signal: &CliSignalContext,
    dependencies: &CliDependencies,
    io: &mut dyn CliIo,
    interrupted: impl std::future::Future<Output = ()>,
) -> Dispatch {
    let completed = {
        let waiting = dispatch_with_context(signal, dependencies, io);
        tokio::pin!(waiting);
        tokio::pin!(interrupted);
        tokio::select! {
            biased;
            () = &mut interrupted => None,
            result = &mut waiting => Some(result),
        }
    };
    let dispatch = completed.unwrap_or_else(|| {
        match signal.emit_interruption(CliFailure::OutcomePending, io) {
            Ok(false) => Dispatch::Exit(CliExit(0)),
            Ok(true) => Dispatch::Exit(CliExit(CliFailure::OutcomePending.exit_code())),
            Err(failure) => Dispatch::Exit(CliExit(failure.exit_code())),
        }
    });
    signal.wait_interruption_diagnostic().await;
    signal.finish_output();
    dispatch
}

async fn run(
    invocation: CliInvocation,
    deps: &CliDependencies,
    io: &mut dyn CliIo,
    signal: &CliSignalContext,
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
        CliCommand::Install { version, source } => {
            deps.native
                .run(
                    key,
                    NativeCliCommand::Install { version, source },
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
            require_confirmation(&confirmation, mode)?;
            let server = read_query(deps.protocol.status()).await?.parts().server;
            let policy = if endpoints.is_empty() {
                spaceterm_core::remote_access::RemoteAccessPolicy::disabled(server)
            } else {
                spaceterm_core::remote_access::RemoteAccessPolicy::enabled(server, endpoints)
                    .map_err(|_| CliFailure::Usage)?
            };
            protocol_confirmed(
                deps,
                ManagementAction::RemoteAccess { policy },
                confirmation,
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
        CliCommand::UpdateStage { version, source } => {
            deps.native
                .run(
                    key,
                    NativeCliCommand::UpdateStage { version, source },
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
        CliCommand::PairAuthorizeClaim { fingerprint } => {
            let claim = io.read_authorization_claim()?;
            if claim.len() > super::pairing_pipe::MAX_PUBLIC_CLAIM_BYTES {
                return Err(CliFailure::LimitExceeded);
            }
            if claim.is_empty() {
                return Err(CliFailure::Usage);
            }
            let delivered = match deps
                .pairing
                .authorize_claim_admitted(
                    claim,
                    fingerprint,
                    signal.pairing_authorization_admission(),
                )
                .await
            {
                Ok(delivery) => {
                    signal.state.admission.record(delivery.request());
                    io.write_authorization_delivery_async(delivery.bytes(), signal)
                        .await
                        .map_err(|failure| {
                            PairingAuthorizationFailure::new(failure, Some(delivery.request()))
                        })
                }
                Err(failure) => Err(failure),
            };
            return match delivered {
                Ok(()) => Ok(Dispatch::Exit(CliExit(0))),
                Err(failure) => signal.emit_authorization_failure(failure, io),
            };
        }
        CliCommand::EndAllSessions { confirmation } => {
            require_confirmation(&confirmation, mode)?;
            let server = read_query(deps.protocol.status()).await?.parts().server;
            protocol_confirmed(
                deps,
                ManagementAction::EndAllSessions { server },
                confirmation,
                io,
            )
            .await?
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
async fn protocol_confirmed(
    deps: &CliDependencies,
    action: ManagementAction,
    confirmation: ConfirmationMode,
    io: &mut dyn CliIo,
) -> Result<output::CliOutput, CliFailure> {
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
        Ok(output::CliOutput::Assessment(assessment))
    } else {
        let confirmed = confirm_assessment(assessment, confirmation, io)?;
        Ok(output::CliOutput::Progress(
            deps.protocol.execute(confirmed).await?,
        ))
    }
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
