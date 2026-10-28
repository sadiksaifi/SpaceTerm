//! Shared policy for local control surfaces. Preparing a confirmation never changes service state.

pub mod cli;

use super::{LingerAuthorization, RetainedInstallation, Service, ServiceStartup, ServiceStatus};
use crate::ServerError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Stop,
    Restart,
    Uninstall,
}

/// An unconfirmed destructive action bound to the selected account service.
pub struct Confirmation {
    service: Service,
    action: Action,
    running: bool,
    wording: Wording,
}

impl Confirmation {
    pub fn action(&self) -> Action {
        self.action
    }

    pub fn wording(&self) -> &Wording {
        &self.wording
    }

    pub fn title(&self) -> &'static str {
        self.wording.title()
    }

    pub fn confirm_label(&self) -> &'static str {
        self.wording.confirm_label()
    }

    pub fn message(&self) -> String {
        self.wording.message()
    }

    pub fn detail(&self) -> String {
        self.wording.detail()
    }
}

/// Shared destructive-action copy, without service access or an authority grant.
pub struct Wording {
    server: String,
    action: Action,
    running: bool,
    startup: ServiceStartup,
}

impl Wording {
    /// Names the server from the compiled application identity.
    pub fn new(identity: &str, action: Action, running: bool, startup: ServiceStartup) -> Self {
        Self::for_server(display_name(identity), action, running, startup)
    }

    /// Names the server from a supplied name, so a remote confirmation names the machine the
    /// person selected rather than the application identity this process was built as.
    pub fn for_server(
        server: &str,
        action: Action,
        running: bool,
        startup: ServiceStartup,
    ) -> Self {
        Self {
            server: server.into(),
            action,
            running,
            startup,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.action {
            Action::Stop => "Stop Server",
            Action::Restart => "Restart Server",
            Action::Uninstall => "Uninstall Server",
        }
    }

    pub fn confirm_label(&self) -> &'static str {
        match self.action {
            Action::Uninstall => "Uninstall",
            _ => self.title(),
        }
    }

    pub fn message(&self) -> String {
        let server = &self.server;
        match self.action {
            Action::Stop => format!("Stop the {server} server?"),
            Action::Restart => format!("Restart the {server} server?"),
            Action::Uninstall => format!("Uninstall the {server} server?"),
        }
    }

    /// What ends and what is kept.
    pub fn detail(&self) -> String {
        const ENDS: &str = "All Terminal Sessions on it end for every client.";
        match self.action {
            Action::Stop => format!(
                "{ENDS} Workspaces are kept, and the server stays stopped until you start it again."
            ),
            Action::Restart => format!(
                "{ENDS} Workspaces are kept, and each Pane starts a new shell when you use it."
            ),
            Action::Uninstall if self.running => {
                format!("{ENDS} Workspaces are kept, so you can install it again later.")
            }
            Action::Uninstall => {
                let startup = match self.startup {
                    ServiceStartup::AtLogin => "It no longer starts when you log in.",
                    ServiceStartup::AtBoot => "It no longer starts when the computer starts.",
                };
                format!("{startup} Workspaces are kept, so you can install it again later.")
            }
        }
    }
}

/// Whether an update confirmation installs the candidate or returns to the previous version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateChange {
    /// Replaces the approved runtime with the verified candidate.
    Update,
    /// Returns to the retained previous runtime after the candidate never became ready.
    Return,
}

/// Shared copy for confirming one exact server update or return, without service access or an
/// authority grant. Each build is named by version and abbreviated commit, so two builds of one
/// version read apart.
pub struct UpdateWording {
    server: String,
    change: UpdateChange,
    previous: String,
    candidate: String,
    running: bool,
    stopped: bool,
}

impl UpdateWording {
    /// `previous` is the approved build and `candidate` the verified build it changes to.
    /// `running` and `stopped` are the observation the confirmation binds.
    pub fn new(
        identity: &str,
        change: UpdateChange,
        previous: &RetainedInstallation,
        candidate: &RetainedInstallation,
        running: bool,
        stopped: bool,
    ) -> Self {
        Self::for_server(
            display_name(identity),
            change,
            BuildName::of(previous),
            BuildName::of(candidate),
            running,
            stopped,
        )
    }

    /// Names the server from a supplied name and each build from supplied nonsecret values, so a
    /// remote confirmation names the machine the person selected and the builds a remote service
    /// reported, without a local installation directory.
    pub fn for_server(
        server: &str,
        change: UpdateChange,
        previous: BuildName<'_>,
        candidate: BuildName<'_>,
        running: bool,
        stopped: bool,
    ) -> Self {
        let (previous, candidate) = build_labels(previous, candidate);
        Self {
            server: server.into(),
            change,
            previous,
            candidate,
            running,
            stopped,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.change {
            UpdateChange::Update => "Update Server",
            UpdateChange::Return => "Return to Previous Version",
        }
    }

    pub fn confirm_label(&self) -> String {
        match self.change {
            UpdateChange::Update => self.title().into(),
            UpdateChange::Return => format!("Return to {}", self.previous),
        }
    }

    pub fn message(&self) -> String {
        let Self {
            server,
            previous,
            candidate,
            ..
        } = self;
        match self.change {
            UpdateChange::Update if self.stopped && !self.running => {
                format!("Update the stopped {server} server to {candidate}?")
            }
            UpdateChange::Update => format!("Update the {server} server to {candidate}?"),
            UpdateChange::Return => format!("Return the {server} server to {previous}?"),
        }
    }

    /// What changes, what ends, and what is kept.
    pub fn detail(&self) -> String {
        let Self {
            previous,
            candidate,
            ..
        } = self;
        match self.change {
            UpdateChange::Update if self.running => format!(
                "It changes from {previous} to {candidate}. All Terminal Sessions on it end for \
                 every client, and Workspaces are kept."
            ),
            UpdateChange::Update if self.stopped => format!(
                "It changes from {previous} to {candidate}. It isn\u{2019}t running, so no \
                 Terminal Sessions end, and it stays stopped until you start it."
            ),
            UpdateChange::Update => format!(
                "It changes from {previous} to {candidate}. It isn\u{2019}t running, so no \
                 Terminal Sessions end."
            ),
            UpdateChange::Return => format!(
                "Version {candidate} never started. Version {previous} and your Workspaces are \
                 unchanged, and the server stays stopped until you start it."
            ),
        }
    }
}

/// What names a server build to a person: its version, source commit, and package digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildName<'a> {
    pub version: &'a str,
    pub commit: &'a str,
    /// The whole signed package's digest. Two packages of one version and commit differ here.
    pub package: &'a str,
}

impl<'a> BuildName<'a> {
    pub fn of(installation: &'a RetainedInstallation) -> Self {
        Self {
            version: &installation.version,
            commit: &installation.commit,
            package: installation
                .directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        }
    }
}

/// Names a server build by version and abbreviated source commit.
pub fn build_label(version: &str, commit: &str) -> String {
    let commit = commit.get(..7).unwrap_or(commit);
    format!("{version} (commit {commit})")
}

/// Names two builds so they read apart. Version and commit name each build, and the package
/// digest's prefix is added only when those match, since the packages then differ elsewhere.
pub fn build_labels(first: BuildName<'_>, second: BuildName<'_>) -> (String, String) {
    let name = |build: BuildName<'_>| {
        let commit = build.commit.get(..7).unwrap_or(build.commit);
        format!("{} (commit {commit}", build.version)
    };
    let ambiguous = name(first) == name(second) && first.package != second.package;
    let label = |build: BuildName<'_>| match build.package.get(..8) {
        Some(package) if ambiguous => format!("{}, package {package})", name(build)),
        _ => format!("{})", name(build)),
    };
    (label(first), label(second))
}

/// The application display name for a compiled service identity.
pub fn display_name(identity: &str) -> &str {
    match identity {
        "spaceterm" => "SpaceTerm",
        "spaceterm-preflight" => "SpaceTerm Preflight",
        "spaceterm-development" => "SpaceTerm Development",
        _ => identity,
    }
}

/// Blocking owning-account controls shared by command-line and desktop clients.
#[derive(Clone)]
pub struct Controller {
    service: Service,
}

impl Controller {
    pub fn new(service: Service) -> Self {
        Self { service }
    }

    pub fn display_name(&self) -> &str {
        display_name(&self.service.identity)
    }

    pub fn startup(&self) -> ServiceStartup {
        self.service.startup()
    }

    /// Read existing service policy without setup, startup, or credential access.
    pub fn status(&self) -> Result<ServiceStatus, ServerError> {
        self.service.status()
    }

    /// Explicit Start is the only operation that reverses deliberate Stop.
    pub fn start(&self) -> Result<ServiceStatus, ServerError> {
        self.service.start()
    }

    /// Change automatic startup without changing the running server or deliberate Stop.
    pub fn set_automatic_start(&self, enabled: bool) -> Result<ServiceStatus, ServerError> {
        self.service.set_automatic_start(enabled)
    }

    /// Request the platform authorization selected by the person's explicit action.
    pub fn enable_linger(&self, authorization: LingerAuthorization) -> Result<(), ServerError> {
        self.service.enable_linger(authorization)
    }

    /// Observe the selected service and describe an action without changing it.
    pub fn prepare(&self, action: Action) -> Result<Confirmation, ServerError> {
        let running = match self.status() {
            Ok(status) => {
                if !status.installed {
                    return Err(ServerError::NotInstalled);
                }
                if matches!(action, Action::Stop | Action::Restart) && status.stopped {
                    return Err(ServerError::Stopped);
                }
                status.running
            }
            Err(ServerError::CleanupUnresolved) if action == Action::Stop => {
                let (_, state) = self.service.read()?.ok_or(ServerError::NotInstalled)?;
                if !state.installed {
                    return Err(ServerError::NotInstalled);
                }
                // A refused native Stop can leave a process running after durable Stop was
                // published. Warn about every Session and let explicit Stop retry the same
                // native checks; Start and Restart remain unavailable.
                true
            }
            Err(error) => return Err(error),
        };
        Ok(Confirmation {
            service: self.service.clone(),
            action,
            running,
            wording: Wording::new(&self.service.identity, action, running, self.startup()),
        })
    }

    /// Execute one confirmed action. The service rechecks native authority and durable Stop.
    pub fn confirm(&self, confirmation: Confirmation) -> Result<ServiceStatus, ServerError> {
        if confirmation.service.identity != self.service.identity
            || confirmation.service.paths.data != self.service.paths.data
            || confirmation.service.paths.runtime != self.service.paths.runtime
        {
            return Err(ServerError::AuthenticationRefused);
        }
        self.service
            .apply_control(confirmation.action, Some(confirmation.running))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_confirmation_names_the_selected_server_with_the_same_copy() {
        let local = Wording::new(
            "spaceterm-development",
            Action::Stop,
            true,
            ServiceStartup::AtLogin,
        );
        let remote = Wording::for_server("penguin", Action::Stop, true, ServiceStartup::AtLogin);
        assert_eq!(local.message(), "Stop the SpaceTerm Development server?");
        assert_eq!(remote.message(), "Stop the penguin server?");
        assert_eq!(local.title(), remote.title());
        assert_eq!(local.detail(), remote.detail());
        assert_eq!(
            local.detail(),
            "All Terminal Sessions on it end for every client. Workspaces are kept, and the \
             server stays stopped until you start it again."
        );
    }

    #[test]
    fn an_uninstall_confirmation_says_what_is_kept_whether_or_not_the_server_runs() {
        let uninstall = |running, startup| {
            Wording::for_server("penguin", Action::Uninstall, running, startup).detail()
        };
        assert_eq!(
            uninstall(true, ServiceStartup::AtLogin),
            "All Terminal Sessions on it end for every client. Workspaces are kept, so you can \
             install it again later."
        );
        assert_eq!(
            uninstall(false, ServiceStartup::AtLogin),
            "It no longer starts when you log in. Workspaces are kept, so you can install it \
             again later."
        );
        assert_eq!(
            uninstall(false, ServiceStartup::AtBoot),
            "It no longer starts when the computer starts. Workspaces are kept, so you can \
             install it again later."
        );
    }

    #[test]
    fn a_remote_update_confirmation_names_the_selected_server_and_candidate() {
        let build = |version: &'static str| BuildName {
            version,
            commit: "0123456789abcdef0123456789abcdef01234567",
            package: "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
        };
        let wording = UpdateWording::for_server(
            "penguin",
            UpdateChange::Update,
            build("0.6.8"),
            build("0.7.0"),
            true,
            false,
        );
        assert_eq!(
            wording.message(),
            "Update the penguin server to 0.7.0 (commit 0123456)?"
        );
        assert_eq!(
            wording.detail(),
            "It changes from 0.6.8 (commit 0123456) to 0.7.0 (commit 0123456). All Terminal \
             Sessions on it end for every client, and Workspaces are kept."
        );
        let observed = |running, stopped| {
            UpdateWording::for_server(
                "penguin",
                UpdateChange::Update,
                build("0.6.8"),
                build("0.7.0"),
                running,
                stopped,
            )
            .detail()
        };
        assert_eq!(
            observed(false, true),
            "It changes from 0.6.8 (commit 0123456) to 0.7.0 (commit 0123456). It isn\u{2019}t \
             running, so no Terminal Sessions end, and it stays stopped until you start it."
        );
        assert_eq!(
            observed(false, false),
            "It changes from 0.6.8 (commit 0123456) to 0.7.0 (commit 0123456). It isn\u{2019}t \
             running, so no Terminal Sessions end."
        );
        let returning = UpdateWording::for_server(
            "penguin",
            UpdateChange::Return,
            build("0.6.8"),
            build("0.7.0"),
            false,
            true,
        );
        assert_eq!(
            returning.detail(),
            "Version 0.7.0 (commit 0123456) never started. Version 0.6.8 (commit 0123456) and \
             your Workspaces are unchanged, and the server stays stopped until you start it."
        );
    }
}
