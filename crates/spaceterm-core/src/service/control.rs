//! Shared policy for local control surfaces. Preparing a confirmation never changes service state.

use super::{LingerAuthorization, Service, ServiceStartup, ServiceStatus};
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

/// Shared destructive-action copy, without service access or an authority grant. It names this
/// computer: This Mac on macOS, This Computer elsewhere.
pub struct Wording {
    action: Action,
    running: bool,
    startup: ServiceStartup,
}

impl Wording {
    pub fn local(action: Action, running: bool, startup: ServiceStartup) -> Self {
        Self {
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
        let machine = this_computer();
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
    fn a_stop_confirmation_names_this_computer_and_says_what_is_kept() {
        let local = Wording::local(Action::Stop, true, ServiceStartup::AtLogin);
        let computer = if cfg!(target_os = "macos") {
            "This Mac"
        } else {
            "This Computer"
        };
        assert_eq!(
            local.message(),
            format!("Stop SpaceTerm Core on {computer}?")
        );
        assert_eq!(
            local.detail(),
            "All Terminal Sessions on it end for every client. Workspaces are kept, and SpaceTerm \
             Core stays stopped until you start it again."
        );
    }

    #[test]
    fn an_uninstall_confirmation_says_what_is_kept_whether_or_not_the_core_runs() {
        let uninstall =
            |running, startup| Wording::local(Action::Uninstall, running, startup).detail();
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
}
