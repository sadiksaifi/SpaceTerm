//! SpaceTerm Core updates in two steps. Install verifies and unpacks a new version beside the
//! running one and ends nothing. Restart moves the server to that Pending Version through a
//! helper from the previous runtime, and a failed start returns to the previous version.
use super::super::Service;
use crate::CoreError;
use crate::update::facts::{InstallFailure, PendingVersion, UpdateCheck, UpdateFacts, Version};
use crate::update::source::FactsSource;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Where an archive came from. Every source follows the same version order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// This Core's own signed feed. Release builds only.
    Feed,
    /// A desktop pushed its embedded build over the service lane.
    Push,
    /// A local archive, from `install.sh` or `spaceterm update` with a file.
    Local,
}

/// A signed SpaceTerm Core archive on disk, with its raw 64-byte Ed25519 signature.
#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> {
    pub archive: &'a Path,
    pub signature: &'a Path,
    /// 64 lowercase hex digits: the SHA-256 the source declared for `archive`.
    pub sha256: &'a str,
    pub source: Source,
}

/// Why an install did not produce a Pending Version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// The version is not newer than the newest version this Core has.
    #[error("This SpaceTerm Core version is not newer than the installed one.")]
    NotNewer,
    /// The version is the one this Core skipped after it failed to start.
    #[error("This SpaceTerm Core version failed to start before.")]
    Skipped,
    /// Another install or a restart is in progress.
    #[error("Another SpaceTerm Core update is in progress.")]
    Busy,
    /// The install failed and changed nothing. Feed installs and completed pushes record it as
    /// the Core's `last_failure`.
    #[error("The SpaceTerm Core update couldn\u{2019}t be installed.")]
    Failed(InstallFailure),
    #[error("{0}")]
    Core(CoreError),
}

/// What asked for a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    /// A user confirmed it. It ends every Terminal Session and may cross a breaking change.
    /// Without a Pending Version the server restarts into the running version.
    Asked,
    /// The Core decided on its own. It goes ahead only into a Pending Version that is not
    /// breaking, and only at zero Terminal Sessions or at boot.
    Silent,
}

/// Why a restart did not begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RestartRefusal {
    /// A silent restart found no Pending Version.
    #[error("No SpaceTerm Core update is waiting.")]
    NothingPending,
    /// A silent restart found open Terminal Sessions.
    #[error("Terminal Sessions are open.")]
    SessionsOpen,
    /// A silent restart found a breaking Pending Version, which always asks.
    #[error("This SpaceTerm Core update needs confirmation.")]
    Breaking,
    /// Another install or a restart is in progress.
    #[error("Another SpaceTerm Core update is in progress.")]
    Busy,
    #[error("{0}")]
    Core(CoreError),
}

/// A restart the helper now performs. The Core closes its connections next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restarting {
    pub to: Version,
}

/// The update engine of one running SpaceTerm Core, shared by its listeners, its service lane
/// and its update sources. It owns the persisted update record (`pending`, `skipped`,
/// `last_failure`, `check`) and produces the Core's [`UpdateFacts`].
#[derive(Clone)]
pub struct Updates {
    shared: Arc<Shared>,
}

struct Shared {
    service: Service,
}

/// An install in progress. It reports `Installing` progress until it ends. Dropping it without
/// [`Installing::install`] or [`Installing::fail`] records no failure, as a desktop push that
/// breaks off does.
pub struct Installing<'a> {
    updates: &'a Updates,
    version: Version,
    total: u64,
}

/// A feed check in progress. It reports `Checking` until it ends.
pub struct Checking<'a> {
    updates: &'a Updates,
}

impl Updates {
    /// The engine of the server running `service`'s approved runtime. It reads the persisted
    /// update record and settles any restart that did not finish.
    ///
    /// # Errors
    /// Returns the storage classifications for unreadable or unsafe account storage.
    pub fn open(service: Service) -> Result<Self, CoreError> {
        Ok(Self {
            shared: Arc::new(Shared { service }),
        })
    }

    /// The persisted facts of a stopped Core, with `sessions: 0` and no operation.
    ///
    /// # Errors
    /// Returns the storage classifications for unreadable or unsafe account storage.
    pub fn stored(service: &Service) -> Result<UpdateFacts, CoreError> {
        let _ = service;
        Err(CoreError::Unsupported)
    }

    /// Whether an install of `version` would be admitted now. Sources call this before they
    /// download, so a refused version costs nothing.
    ///
    /// # Errors
    /// Returns NotNewer, Skipped or Busy.
    pub fn admits(&self, version: &Version) -> Result<(), InstallError> {
        let _ = (&self.shared.service, version);
        Err(InstallError::Core(CoreError::Unsupported))
    }

    /// Begin installing `version` of `total` archive bytes. Facts report `Installing` from now.
    ///
    /// # Errors
    /// Returns what [`Self::admits`] returns.
    pub fn begin_install(
        &self,
        version: Version,
        total: u64,
    ) -> Result<Installing<'_>, InstallError> {
        self.admits(&version)?;
        Ok(Installing {
            updates: self,
            version,
            total,
        })
    }

    /// Install a local archive whose bytes are all present. Equivalent to
    /// [`Self::begin_install`] then [`Installing::install`].
    ///
    /// # Errors
    /// As [`Installing::install`].
    pub fn install(&self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        let _ = archive;
        Err(InstallError::Core(CoreError::Unsupported))
    }

    /// Begin a restart. A restart into a Pending Version goes through a helper from the running
    /// runtime with a readiness deadline; a failed start restores the retained-state snapshot,
    /// returns to this version and skips the Pending Version unless the failure is transient.
    ///
    /// # Errors
    /// Returns a [`RestartRefusal`] when the restart did not begin.
    pub fn restart(&self, request: Restart) -> Result<Restarting, RestartRefusal> {
        let _ = request;
        Err(RestartRefusal::Core(CoreError::Unsupported))
    }

    /// Begin a feed check. Facts report `Checking` until [`Checking::finish`] or drop.
    pub fn begin_check(&self) -> Checking<'_> {
        Checking { updates: self }
    }

    /// The server's live Terminal Session count changed. At zero, a waiting Pending Version that
    /// is not breaking restarts silently.
    pub fn set_sessions(&self, sessions: u32) {
        let _ = sessions;
    }
}

impl Installing<'_> {
    /// `received` of the archive's bytes arrived.
    pub fn progress(&self, received: u64) {
        let _ = (self.updates, &self.version, self.total, received);
    }

    /// Verify the received archive against the declared version and unpack it beside the
    /// running version. At most two versions stay on disk: the running one and this one.
    ///
    /// # Errors
    /// Returns Failed for disk full or a verification failure, NotNewer or Skipped when the
    /// verified build names another version than the declared one would allow.
    pub fn install(self, archive: Archive<'_>) -> Result<PendingVersion, InstallError> {
        self.updates.install(archive)
    }

    /// The source failed, for example its download broke off. Feed installs record it.
    pub fn fail(self, failure: InstallFailure) {
        let _ = failure;
    }
}

impl Checking<'_> {
    /// Record the check's outcome, which survives restarts.
    pub fn finish(self, check: UpdateCheck) {
        let _ = (self.updates, check);
    }
}

impl FactsSource for Updates {
    fn facts(&self) -> UpdateFacts {
        unimplemented!("interface only")
    }

    fn wait(&self, revision: u64, timeout: Duration) -> UpdateFacts {
        let _ = (revision, timeout);
        self.facts()
    }
}
