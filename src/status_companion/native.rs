//! Platform-neutral pieces of the native status areas and the production composition.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use spaceterm_client::management_cli::{ManagementFailure, ManagementFuture};
use spaceterm_client_native::cli::bindings::NativeObservation;

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

/// The production native observation until the native discovery provider is composed
/// into this binary. It reports the capability as unavailable instead of guessing.
pub(crate) struct UnavailableNativeStatus;

impl NativeStatus for UnavailableNativeStatus {
    fn inspect(&self) -> ManagementFuture<'_, NativeObservation> {
        Box::pin(async { Err(ManagementFailure::NativeUnavailable) })
    }
}

pub(crate) struct TokioTimer;

impl CompanionTimer for TokioTimer {
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }
}
