use super::{
    LingerAuthorization, LingerRequest, LingerState, ServiceFailure, ServiceFuture, SudoMode,
    commands::{ServiceCommand, ServiceCommandRunner, runner},
};
use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use zeroize::Zeroizing;

pub(super) async fn observe(
    runner: &dyn ServiceCommandRunner,
    uid: u32,
) -> Result<LingerState, ServiceFailure> {
    if uid == 0 || uid == u32::MAX {
        return Err(ServiceFailure::RootAccount);
    }
    let output = runner
        .run(ServiceCommand::new(
            "/usr/bin/loginctl",
            [
                "show-user".into(),
                uid.to_string().into(),
                "--property=Linger".into(),
                "--value".into(),
            ],
        ))
        .await?;
    if output.code != 0 {
        return Ok(LingerState::Unavailable);
    }
    match output.bytes.as_slice() {
        b"yes\n" | b"yes" => Ok(LingerState::Enabled),
        b"no\n" | b"no" => Ok(LingerState::Disabled),
        _ => Ok(LingerState::Unavailable),
    }
}

pub(super) fn sudo_command(uid: u32, mode: SudoMode) -> ServiceCommand {
    let mut args = vec![OsString::from("-k")];
    if mode == SudoMode::NonInteractive {
        args.push("-n".into());
    }
    args.extend([
        "--".into(),
        "/usr/bin/loginctl".into(),
        "enable-linger".into(),
        uid.to_string().into(),
    ]);
    ServiceCommand::new("/usr/bin/sudo", args).authorization(mode == SudoMode::Interactive)
}
fn pkexec_command(uid: u32) -> ServiceCommand {
    ServiceCommand::new(
        "/usr/bin/pkexec",
        [
            "/usr/bin/loginctl".into(),
            "enable-linger".into(),
            uid.to_string().into(),
        ],
    )
    .authorization(false)
}

/// CLI authorization for the one fixed loginctl operation. sudo -k prevents
/// this exchange from relying on or creating a reusable sudo timestamp.
pub struct SudoLingerAuthorization {
    pub(super) runner: Arc<dyn ServiceCommandRunner>,
    pub(super) mode: SudoMode,
    pub(super) uid: Option<u32>,
}
impl SudoLingerAuthorization {
    /// Selects controlling-terminal prompting or noninteractive refusal.
    pub fn new(mode: SudoMode) -> Self {
        Self {
            runner: runner(),
            mode,
            uid: None,
        }
    }
}
/// Desktop authorization for loginctl only. No SpaceTerm executable runs as root.
pub struct PkexecLingerAuthorization {
    pub(super) runner: Arc<dyn ServiceCommandRunner>,
    pub(super) uid: Option<u32>,
}
impl PkexecLingerAuthorization {
    /// Selects the host's native polkit authentication agent.
    pub fn new() -> Self {
        Self {
            runner: runner(),
            uid: None,
        }
    }
}
impl Default for PkexecLingerAuthorization {
    fn default() -> Self {
        Self::new()
    }
}

fn check_request(
    request: &LingerRequest,
    injected_uid: Option<u32>,
) -> Result<u32, ServiceFailure> {
    let uid = match injected_uid {
        #[cfg(feature = "test-support")]
        Some(uid) => uid,
        _ => {
            if !cfg!(target_os = "linux") {
                return Err(ServiceFailure::UnsupportedHost);
            }
            super::supervisor::current_account()?.0
        }
    };
    if uid == 0 || uid == u32::MAX {
        return Err(ServiceFailure::RootAccount);
    }
    if request.uid != uid {
        return Err(ServiceFailure::IdentityMismatch);
    }
    Ok(uid)
}
async fn needs_authorization(
    runner: &dyn ServiceCommandRunner,
    uid: u32,
) -> Result<bool, ServiceFailure> {
    match observe(runner, uid).await? {
        LingerState::Enabled => Ok(false),
        LingerState::Disabled => Ok(true),
        LingerState::Unavailable => Err(ServiceFailure::AuthorizationUnavailable),
    }
}
async fn verify_enabled(runner: &dyn ServiceCommandRunner, uid: u32) -> Result<(), ServiceFailure> {
    if observe(runner, uid).await? == LingerState::Enabled {
        Ok(())
    } else {
        Err(ServiceFailure::AuthorizationUnavailable)
    }
}
impl LingerAuthorization for SudoLingerAuthorization {
    fn enable_linger<'a>(&'a self, request: &'a LingerRequest) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let uid = check_request(request, self.uid)?;
            if !needs_authorization(self.runner.as_ref(), uid).await? {
                return Ok(());
            }
            let output = self
                .runner
                .run(sudo_command(uid, self.mode))
                .await
                .map_err(authorization_failure)?;
            match output.code {
                0 => {}
                130 => return Err(ServiceFailure::AuthorizationCancelled),
                _ => return Err(ServiceFailure::AuthorizationDenied),
            }
            verify_enabled(self.runner.as_ref(), uid).await
        })
    }
}
impl LingerAuthorization for PkexecLingerAuthorization {
    fn enable_linger<'a>(&'a self, request: &'a LingerRequest) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            let uid = check_request(request, self.uid)?;
            if !needs_authorization(self.runner.as_ref(), uid).await? {
                return Ok(());
            }
            let output = self
                .runner
                .run(pkexec_command(uid))
                .await
                .map_err(authorization_failure)?;
            match output.code {
                0 => verify_enabled(self.runner.as_ref(), uid).await,
                126 => Err(ServiceFailure::AuthorizationCancelled),
                127 => Err(ServiceFailure::AuthorizationDenied),
                _ => Err(ServiceFailure::AuthorizationUnavailable),
            }
        })
    }
}
fn authorization_failure(failure: ServiceFailure) -> ServiceFailure {
    match failure {
        ServiceFailure::SupervisorUnavailable => ServiceFailure::AuthorizationUnavailable,
        other => other,
    }
}

/// Existing obscured Authentication Prompt, injected by the SSH Bootstrap owner.
/// The response is zeroized and is never a command, environment value or cached grant.
pub trait LingerAuthenticationPrompt: Send + Sync {
    /// Obtains one response for this independently resolved account.
    fn respond<'a>(&'a self, request: &'a LingerRequest) -> ServiceFuture<'a, Zeroizing<Vec<u8>>>;
}
/// One invocation's borrowed sudo prompt scope. It has neither Clone nor Debug.
pub struct ScopedLingerAuthentication<'a> {
    request: &'a LingerRequest,
    prompt: &'a dyn LingerAuthenticationPrompt,
    attempts: AtomicU8,
}
impl ScopedLingerAuthentication<'_> {
    /// Delivers at most three bounded zeroizing responses to this original exchange.
    /// The SSH owner writes them only to its retained sudo terminal, not stdin or argv.
    pub fn respond(&self) -> ServiceFuture<'_, Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            self.attempts
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < 3).then_some(n + 1)
                })
                .map_err(|_| ServiceFailure::LimitExceeded)?;
            let response = self.prompt.respond(self.request).await?;
            if response.len() > 16 * 1024 || response.contains(&0) {
                return Err(ServiceFailure::LimitExceeded);
            }
            Ok(response)
        })
    }
}
/// Authenticated SSH account/host and retained sudo terminal owner.
/// It must consume the exact child on denial, cancellation and observer loss.
pub trait SshLingerExchange: Send + Sync {
    /// Observes linger over the authenticated SSH transport without elevation.
    fn linger(&self, request: &LingerRequest) -> ServiceFuture<'_, LingerState>;
    /// Runs only the supplied closed sudo/loginctl command. No shell, AskPass,
    /// sudo -S, caller program or password environment is an accepted route.
    fn invoke<'a>(
        &'a self,
        request: &'a LingerRequest,
        command: ServiceCommand,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> ServiceFuture<'a, ()>;
}
/// SSH Bootstrap authorization scoped to one fixed loginctl exchange.
pub struct SshLingerAuthorization<B, P> {
    boundary: B,
    prompt: P,
}
impl<B: SshLingerExchange, P: LingerAuthenticationPrompt> SshLingerAuthorization<B, P> {
    /// Injects the authenticated SSH exchange and existing Authentication Prompt.
    pub fn new(boundary: B, prompt: P) -> Self {
        Self { boundary, prompt }
    }
}
impl<B: SshLingerExchange, P: LingerAuthenticationPrompt> LingerAuthorization
    for SshLingerAuthorization<B, P>
{
    fn enable_linger<'a>(&'a self, request: &'a LingerRequest) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            if request.uid == 0 || request.uid == u32::MAX {
                return Err(ServiceFailure::RootAccount);
            }
            match self.boundary.linger(request).await? {
                LingerState::Enabled => return Ok(()),
                LingerState::Unavailable => return Err(ServiceFailure::AuthorizationUnavailable),
                LingerState::Disabled => {}
            }
            let authentication = ScopedLingerAuthentication {
                request,
                prompt: &self.prompt,
                attempts: AtomicU8::new(0),
            };
            self.boundary
                .invoke(
                    request,
                    sudo_command(request.uid, SudoMode::Interactive),
                    &authentication,
                )
                .await?;
            if self.boundary.linger(request).await? == LingerState::Enabled {
                Ok(())
            } else {
                Err(ServiceFailure::AuthorizationUnavailable)
            }
        })
    }
}
