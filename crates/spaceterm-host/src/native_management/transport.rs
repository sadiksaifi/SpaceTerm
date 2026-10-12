//! Fixed client transport composition. No caller-selected commands or credential inputs.

use super::{
    ConfirmedNativeAction, NativeAction, NativeAssessment, NativeManagementPort, NativeStatus,
};
use crate::native_deployment::contracts::{
    AccountBuildKey, NativeFailure, NativeManagementFuture, NativeRequestId,
};
use crate::native_management::{
    SudoMode,
    cli_sudo::{SudoAdapter, SudoBoundary},
    controller::{AuthorizationRoute, ManagementProgress},
};

/// Read/assessment/resolution access to the existing authenticated native owner.
/// Assessment retains its original protected scope; resolve cannot admit a mutation.
pub trait NativeManagementObservations: Send + Sync {
    /// Reads bounded native observations without loading credentials or changing intent.
    fn inspect(&self, key: AccountBuildKey) -> NativeManagementFuture<'_, NativeStatus>;
    /// Stores the exact original assessment with the installation owner's native request scope.
    fn assess(
        &self,
        key: AccountBuildKey,
        action: NativeAction,
    ) -> NativeManagementFuture<'_, NativeAssessment>;
    /// Authenticates the caller and resolves only the original admitted request.
    fn resolve(&self, request: NativeRequestId) -> NativeManagementFuture<'_, ManagementProgress>;
}

/// Fixed OS GUI transport, selected for the platform by native composition.
pub trait NativeGuiManagementTransport: Send + Sync {
    /// Sends the exact confirmed request through the audit-token or authenticated polkit subject route.
    fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
    ) -> NativeManagementFuture<'_, ManagementProgress>;
}

/// Concrete facade port over the native owner, retained system sudo, and fixed GUI route.
pub struct SystemManagementPort<O, S, G> {
    observations: O,
    sudo: std::sync::Arc<SudoAdapter<S>>,
    gui: G,
}
impl<O: NativeManagementObservations, S: SudoBoundary, G: NativeGuiManagementTransport>
    SystemManagementPort<O, S, G>
{
    /// Injects native peers, not a command, executable path, or password provider.
    pub fn new(observations: O, sudo: S, gui: G) -> Self {
        Self {
            observations,
            sudo: std::sync::Arc::new(SudoAdapter::new(sudo)),
            gui,
        }
    }
}
impl<O: NativeManagementObservations, S: SudoBoundary + 'static, G: NativeGuiManagementTransport>
    NativeManagementPort for SystemManagementPort<O, S, G>
{
    fn inspect(&self, key: AccountBuildKey) -> NativeManagementFuture<'_, NativeStatus> {
        self.observations.inspect(key)
    }
    fn assess(
        &self,
        key: AccountBuildKey,
        action: NativeAction,
    ) -> NativeManagementFuture<'_, NativeAssessment> {
        self.observations.assess(key, action)
    }
    fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
        route: AuthorizationRoute,
    ) -> NativeManagementFuture<'_, ManagementProgress> {
        match route {
            AuthorizationRoute::Gui => self.gui.execute(confirmed),
            AuthorizationRoute::CliInteractive | AuthorizationRoute::CliNonInteractive => {
                Box::pin(async move {
                    let mode = match route {
                        AuthorizationRoute::CliNonInteractive => SudoMode::NonInteractive,
                        _ => SudoMode::Interactive,
                    };
                    let request = confirmed.assessment().request().clone();
                    let sudo = std::sync::Arc::clone(&self.sudo);
                    let (reply, waiting) = tokio::sync::oneshot::channel();
                    // Cancellation drops only the client wait. The exchange retains its
                    // original request and native custody until the actual broker is reaped.
                    std::thread::Builder::new()
                        .name("native-management-exchange".into())
                        .spawn(move || {
                            let _ = reply.send(sudo.execute(&request, mode));
                        })
                        .map_err(|_| NativeFailure::NativeUnavailable)?;
                    waiting
                        .await
                        .map_err(|_| NativeFailure::NativeUnavailable)?
                })
            }
        }
    }
    fn resolve(&self, request: NativeRequestId) -> NativeManagementFuture<'_, ManagementProgress> {
        self.observations.resolve(request)
    }
}

/// Validates native response correlation independently of broker exit or authorization outcome.
pub fn bind_reply(
    request: NativeRequestId,
    progress: ManagementProgress,
) -> Result<ManagementProgress, NativeFailure> {
    if progress.request != *request.as_bytes() {
        return Err(NativeFailure::IdentityMismatch);
    }
    Ok(progress)
}
