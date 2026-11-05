//! Shared policy for local control surfaces. Preparing a confirmation never changes service state.

use super::{LingerAuthorization, RetainedInstallation, Service, ServiceStartup, ServiceStatus};
use crate::CoreError;

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

    /// Whether the server ran when this confirmation was prepared.
    pub fn running(&self) -> bool {
        self.running
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
    /// The Machine the copy names: This Mac, This Computer, or the selected machine's name.
    machine: String,
    action: Action,
    running: bool,
    startup: ServiceStartup,
}

impl Wording {
    /// Names this computer: This Mac on macOS, This Computer elsewhere.
    pub fn local(action: Action, running: bool, startup: ServiceStartup) -> Self {
        Self::for_machine(this_computer(), action, running, startup)
    }

    /// Names the Machine the person selected.
    pub fn for_machine(
        machine: &str,
        action: Action,
        running: bool,
        startup: ServiceStartup,
    ) -> Self {
        Self {
            machine: machine.into(),
            action,
            running,
            startup,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.action {
            Action::Stop => "Stop SpaceTerm Core",
            Action::Restart => "Restart SpaceTerm Core",
            Action::Uninstall => "Uninstall SpaceTerm Core",
        }
    }

    pub fn confirm_label(&self) -> &'static str {
        match self.action {
            Action::Uninstall => "Uninstall",
            _ => self.title(),
        }
    }

    pub fn message(&self) -> String {
        let machine = &self.machine;
        match self.action {
            Action::Stop => format!("Stop SpaceTerm Core on {machine}?"),
            Action::Restart => format!("Restart SpaceTerm Core on {machine}?"),
            Action::Uninstall => format!("Uninstall SpaceTerm Core on {machine}?"),
        }
    }

    /// What ends and what is kept.
    pub fn detail(&self) -> String {
        const ENDS: &str = "All Terminal Sessions on it end for every client.";
        match self.action {
            Action::Stop => format!(
                "{ENDS} Workspaces are kept, and SpaceTerm Core stays stopped until you start it again."
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

/// Shared copy for confirming one exact SpaceTerm Core update, without service access or an
/// authority grant. Each build is named by version and abbreviated commit, so two builds of one
/// version read apart.
pub struct UpdateWording {
    /// The Machine the copy names: This Mac, This Computer, or the selected machine's name.
    machine: String,
    previous: String,
    candidate: String,
    running: bool,
    stopped: bool,
}

impl UpdateWording {
    /// `previous` is the approved build and `candidate` the verified build it changes to.
    /// `running` and `stopped` are the observation the confirmation binds.
    pub fn local(
        previous: &RetainedInstallation,
        candidate: &RetainedInstallation,
        running: bool,
        stopped: bool,
    ) -> Self {
        Self::for_machine(
            this_computer(),
            BuildName::of(previous),
            BuildName::of(candidate),
            running,
            stopped,
        )
    }

    /// Names the server from a supplied name and each build from supplied nonsecret values, so a
    /// remote confirmation names the machine the person selected and the builds a remote service
    /// reported, without a local installation directory.
    pub fn for_machine(
        machine: &str,
        previous: BuildName<'_>,
        candidate: BuildName<'_>,
        running: bool,
        stopped: bool,
    ) -> Self {
        let (previous, candidate) = build_labels(previous, candidate);
        Self {
            machine: machine.into(),
            previous,
            candidate,
            running,
            stopped,
        }
    }

    pub fn title(&self) -> &'static str {
        "Update SpaceTerm Core"
    }

    pub fn confirm_label(&self) -> String {
        self.title().into()
    }

    pub fn message(&self) -> String {
        let Self {
            machine, candidate, ..
        } = self;
        if self.stopped && !self.running {
            format!("Update the stopped SpaceTerm Core on {machine} to {candidate}?")
        } else {
            format!("Update SpaceTerm Core on {machine} to {candidate}?")
        }
    }

    /// What changes, what ends, and what is kept.
    pub fn detail(&self) -> String {
        let Self {
            previous,
            candidate,
            ..
        } = self;
        if self.running {
            format!(
                "It changes from {previous} to {candidate}. All Terminal Sessions on it end for \
                 every client, and Workspaces are kept."
            )
        } else if self.stopped {
            format!(
                "It changes from {previous} to {candidate}. It isn\u{2019}t running, so no \
                 Terminal Sessions end, and it stays stopped until you start it."
            )
        } else {
            format!(
                "It changes from {previous} to {candidate}. It isn\u{2019}t running, so no \
                 Terminal Sessions end."
            )
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

/// How confirmations name the computer this process runs on.
pub fn this_computer() -> &'static str {
    if cfg!(target_os = "macos") {
        "This Mac"
    } else {
        "This Computer"
    }
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
    pub fn status(&self) -> Result<ServiceStatus, CoreError> {
        self.service.status()
    }

    /// The directories a client watches to learn of service changes without reading the service.
    pub fn state_directories(&self) -> [&std::path::Path; 2] {
        self.service.state_directories()
    }

    /// The running server's process, which a client watches to learn that it exited.
    pub fn core_process(&self) -> Result<Option<u32>, CoreError> {
        self.service.core_process()
    }

    /// Explicit Start is the only operation that reverses deliberate Stop.
    pub fn start(&self) -> Result<ServiceStatus, CoreError> {
        self.service.start()
    }

    /// Change automatic startup without changing the running server or deliberate Stop.
    pub fn set_automatic_start(&self, enabled: bool) -> Result<ServiceStatus, CoreError> {
        self.service.set_automatic_start(enabled)
    }

    /// Request the platform authorization selected by the person's explicit action.
    pub fn enable_linger(&self, authorization: LingerAuthorization) -> Result<(), CoreError> {
        self.service.enable_linger(authorization)
    }

    /// Observe the selected service and describe an action without changing it.
    pub fn prepare(&self, action: Action) -> Result<Confirmation, CoreError> {
        self.prepare_with(action, super::Contention::Wait)
    }

    /// [`Self::prepare`], where an activation's recovery takes the service lock as
    /// `contention` says.
    pub(crate) fn prepare_with(
        &self,
        action: Action,
        contention: super::Contention,
    ) -> Result<Confirmation, CoreError> {
        let running = match self.service.status_with(contention) {
            Ok(status) => {
                if !status.installed {
                    return Err(CoreError::NotInstalled);
                }
                if matches!(action, Action::Stop | Action::Restart) && status.stopped {
                    return Err(CoreError::Stopped);
                }
                status.running
            }
            Err(CoreError::CleanupUnresolved) if action == Action::Stop => {
                let (_, state) = self.service.read()?.ok_or(CoreError::NotInstalled)?;
                if !state.installed {
                    return Err(CoreError::NotInstalled);
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
            wording: Wording::local(action, running, self.startup()),
        })
    }

    /// Execute one confirmed action. The service rechecks native authority and durable Stop.
    pub fn confirm(&self, confirmation: Confirmation) -> Result<ServiceStatus, CoreError> {
        if confirmation.service.identity != self.service.identity
            || confirmation.service.paths.data != self.service.paths.data
            || confirmation.service.paths.runtime != self.service.paths.runtime
        {
            return Err(CoreError::AuthenticationRefused);
        }
        self.service
            .apply_control(confirmation.action, Some(confirmation.running))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_confirmation_names_the_selected_machine_with_the_same_copy() {
        let local = Wording::local(Action::Stop, true, ServiceStartup::AtLogin);
        let remote = Wording::for_machine("penguin", Action::Stop, true, ServiceStartup::AtLogin);
        let computer = if cfg!(target_os = "macos") {
            "This Mac"
        } else {
            "This Computer"
        };
        assert_eq!(
            local.message(),
            format!("Stop SpaceTerm Core on {computer}?")
        );
        assert_eq!(remote.message(), "Stop SpaceTerm Core on penguin?");
        assert_eq!(local.title(), remote.title());
        assert_eq!(local.detail(), remote.detail());
        assert_eq!(
            local.detail(),
            "All Terminal Sessions on it end for every client. Workspaces are kept, and SpaceTerm \
             Core stays stopped until you start it again."
        );
    }

    #[test]
    fn an_uninstall_confirmation_says_what_is_kept_whether_or_not_the_core_runs() {
        let uninstall = |running, startup| {
            Wording::for_machine("penguin", Action::Uninstall, running, startup).detail()
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
    fn a_remote_update_confirmation_names_the_selected_machine_and_candidate() {
        let build = |version: &'static str| BuildName {
            version,
            commit: "0123456789abcdef0123456789abcdef01234567",
            package: "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
        };
        let wording =
            UpdateWording::for_machine("penguin", build("0.6.8"), build("0.7.0"), true, false);
        assert_eq!(
            wording.message(),
            "Update SpaceTerm Core on penguin to 0.7.0 (commit 0123456)?"
        );
        assert_eq!(
            wording.detail(),
            "It changes from 0.6.8 (commit 0123456) to 0.7.0 (commit 0123456). All Terminal \
             Sessions on it end for every client, and Workspaces are kept."
        );
        let observed = |running, stopped| {
            UpdateWording::for_machine("penguin", build("0.6.8"), build("0.7.0"), running, stopped)
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
    }
}
