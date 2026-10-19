//! Shared policy for local control surfaces. Preparing a confirmation never changes service state.

pub mod cli;

use super::{LingerAuthorization, Service, ServiceStartup, ServiceStatus};
use crate::ServerError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    pub fn new(identity: &str, action: Action, running: bool, startup: ServiceStartup) -> Self {
        Self {
            server: display_name(identity).into(),
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
            Action::Stop => {
                format!("Stop the {server} server and end all of its Terminal Sessions?")
            }
            Action::Restart => {
                format!("Restart the {server} server and end all of its Terminal Sessions?")
            }
            Action::Uninstall if self.running => {
                format!("Uninstall the {server} server and end all of its Terminal Sessions?")
            }
            Action::Uninstall => format!("Uninstall the {server} server?"),
        }
    }

    pub fn detail(&self) -> String {
        let startup = self.startup;
        match self.action {
            Action::Stop => match startup {
                ServiceStartup::AtLogin => "Terminal Sessions end for every client using this server, and programs running in them stop. Workspaces are kept. The server stays stopped, even after you log in again, until you choose Start Server.",
                ServiceStartup::AtBoot => "Terminal Sessions end for every client using this server, and programs running in them stop. Workspaces are kept. The server stays stopped, even after the computer restarts, until you choose Start Server.",
            }.into(),
            Action::Restart => "Terminal Sessions end for every client using this server, and programs running in them stop. Workspaces are kept, and each Pane starts a new shell when you next use it.".into(),
            Action::Uninstall => {
                let ending = if self.running { "Terminal Sessions end for every client using this server, and programs running in them stop. " } else { "" };
                let startup = match startup {
                    ServiceStartup::AtLogin => "It no longer starts when you log in. Workspaces and verified server installations are kept, so you can install it again.",
                    ServiceStartup::AtBoot => "It no longer starts when the computer starts. Workspaces and verified server installations are kept, so you can install it again. Lingering stays on for your account, because other services may rely on it. To turn it off, run loginctl disable-linger.",
                };
                format!("{ending}{startup}")
            }
        }
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

/// Blocking owning-account controls, injected into a CLI or native status-area host.
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
