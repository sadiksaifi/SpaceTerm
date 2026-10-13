//! Platform-neutral pieces of the native status areas and the production composition.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use spaceterm_client::management_cli::{
    ManagementFailure, ManagementFuture, ServerStatus, StatusManagement, StatusSubscription,
};
use spaceterm_client_native::NativeClientProvider;
use spaceterm_client_native::cli::bindings::{NativeObservation, RegistrationAvailability};
use spaceterm_host::startup::DesiredServerState;

use super::controller::{CompanionTimer, NativeStatus};
use super::model::CompanionModel;

/// The one model a native status area has not rendered yet. Replacing it while a render
/// is pending schedules nothing new, so updates coalesce instead of queueing.
pub(crate) struct ModelSlot {
    model: Mutex<Option<CompanionModel>>,
    scheduled: AtomicBool,
}

impl ModelSlot {
    pub(crate) fn new() -> Self {
        Self {
            model: Mutex::new(None),
            scheduled: AtomicBool::new(false),
        }
    }

    /// Stores the newest model. Returns true when the caller must schedule one render.
    pub(crate) fn replace(&self, model: CompanionModel) -> bool {
        *self.model.lock().unwrap_or_else(PoisonError::into_inner) = Some(model);
        !self.scheduled.swap(true, Ordering::AcqRel)
    }

    /// Takes the newest model for rendering. A later replacement schedules a new render.
    pub(crate) fn take(&self) -> Option<CompanionModel> {
        self.scheduled.store(false, Ordering::Release);
        self.model
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// The desktop bundle that ships this companion as
/// `<Desktop>.app/Contents/Helpers/<Companion>.app/Contents/MacOS/<executable>`.
#[cfg_attr(
    not(any(test, target_os = "macos")),
    allow(dead_code, reason = "only the macOS bundle layout nests the companion")
)]
pub(crate) fn enclosing_desktop_bundle(executable: &Path) -> Option<PathBuf> {
    let named = |path: &Path, name: &str| path.file_name().is_some_and(|file| file == name);
    let bundle = |path: &Path| path.extension().is_some_and(|extension| extension == "app");
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let companion = contents.parent()?;
    let helpers = companion.parent()?;
    let desktop_contents = helpers.parent()?;
    let desktop = desktop_contents.parent()?;
    (executable.is_absolute()
        && named(macos, "MacOS")
        && named(contents, "Contents")
        && bundle(companion)
        && named(helpers, "Helpers")
        && named(desktop_contents, "Contents")
        && bundle(desktop))
    .then(|| desktop.to_path_buf())
}

/// The desktop executable installed beside this companion as `<prefix>/bin/spaceterm`.
#[cfg_attr(
    not(any(test, target_os = "linux")),
    allow(
        dead_code,
        reason = "only the Linux layout installs the companion beside the desktop"
    )
)]
pub(crate) fn sibling_desktop_executable(executable: &Path) -> Option<PathBuf> {
    if !executable.is_absolute()
        || executable
            .file_name()
            .is_none_or(|name| name != "spaceterm-status")
    {
        return None;
    }
    Some(executable.parent()?.join("spaceterm"))
}

/// An XDG activation token a status host provided for the next desktop launch. It is
/// bounded and printable, and it is used once.
#[cfg(target_os = "linux")]
#[derive(Clone, Default)]
pub(crate) struct ActivationToken(std::sync::Arc<Mutex<Option<String>>>);

#[cfg(target_os = "linux")]
impl ActivationToken {
    const MAX: usize = 4096;

    pub(crate) fn provide(&self, token: String) {
        let valid = !token.is_empty()
            && token.len() <= Self::MAX
            && token.bytes().all(|byte| byte.is_ascii_graphic());
        if valid {
            *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(token);
        }
    }

    pub(crate) fn take(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

type ProviderFactory = dyn Fn() -> Result<NativeClientProvider, ManagementFailure> + Send + Sync;

/// Read-only composition for both status areas. Each attempt obtains the shared native
/// provider again, so setup or a saved companion binding can change while the item runs.
/// The provider alone selects the account/build and validates the companion's own binding.
pub(crate) struct NativeCompanion {
    provider: Arc<ProviderFactory>,
    // Liveness reads use the subscription's worker, not a second connection that
    // could replace the same Client's active connection generation.
    connected: Mutex<Option<NativeClientProvider>>,
}

impl NativeCompanion {
    pub(crate) fn current() -> Self {
        Self::new(NativeClientProvider::current)
    }

    pub(crate) fn new(
        provider: impl Fn() -> Result<NativeClientProvider, ManagementFailure> + Send + Sync + 'static,
    ) -> Self {
        Self {
            provider: Arc::new(provider),
            connected: Mutex::new(None),
        }
    }

    async fn provider(&self) -> Result<NativeClientProvider, ManagementFailure> {
        let provider = Arc::clone(&self.provider);
        tokio::task::spawn_blocking(move || provider())
            .await
            .map_err(|_| ManagementFailure::NativeUnavailable)?
    }

    async fn status_client(
        provider: &NativeClientProvider,
    ) -> Result<Arc<dyn StatusManagement>, ManagementFailure> {
        let observation = provider.inspect(provider.key()).await?;
        match observation.availability {
            RegistrationAvailability::Registered => {}
            RegistrationAvailability::Missing | RegistrationAvailability::Unavailable => {
                return Err(ManagementFailure::NativeUnavailable);
            }
            RegistrationAvailability::Locked => return Err(ManagementFailure::StorageLocked),
            RegistrationAvailability::Unsafe => return Err(ManagementFailure::UnsafeNativeState),
            RegistrationAvailability::Unsupported => {
                return Err(ManagementFailure::UnsupportedHost);
            }
        }
        // Missing companion authorization stays a pairing prerequisite even when
        // the service is stopped. Never substitute another client's binding.
        let status = provider.companion_status()?;
        // A durable Stop is not missing setup. An offline result lets the controller
        // show the advisory native Stop without opening or starting a connection.
        if observation.desired == Some(DesiredServerState::Stopped) {
            return Err(ManagementFailure::Offline);
        }
        Ok(status)
    }
}

impl NativeStatus for NativeCompanion {
    fn inspect(&self) -> ManagementFuture<'_, NativeObservation> {
        Box::pin(async {
            let provider = self.provider().await?;
            provider.inspect(provider.key()).await
        })
    }
}

impl StatusManagement for NativeCompanion {
    fn status(&self) -> ManagementFuture<'_, ServerStatus> {
        Box::pin(async {
            let connected = self
                .connected
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let provider = match connected {
                Some(provider) => provider,
                None => self.provider().await?,
            };
            Self::status_client(&provider).await?.status().await
        })
    }

    fn subscribe_status(&self) -> ManagementFuture<'_, StatusSubscription> {
        Box::pin(async {
            self.connected
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let provider = self.provider().await?;
            let subscription = Self::status_client(&provider)
                .await?
                .subscribe_status()
                .await?;
            *self
                .connected
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(provider);
            Ok(subscription)
        })
    }
}

pub(crate) struct TokioTimer;

impl CompanionTimer for TokioTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }
}
