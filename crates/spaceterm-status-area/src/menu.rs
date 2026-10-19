//! What the status item shows and offers, derived from the service's state alone.
use spaceterm_server::{ServerError, service::ServiceStatus};

/// One person-chosen operation the status menu offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    /// Launches the desktop. It never starts, installs, or stops the server itself.
    Open,
    Start,
    /// Asks first, since it ends every client's Terminal Sessions.
    Stop,
    /// Asks first, since it ends every client's Terminal Sessions.
    Restart,
    /// Removes the status item until the next login. The server keeps running. Unavailable while
    /// a server operation runs, so exiting never abandons an admitted operation's cleanup.
    Hide,
}

/// What the status item last learned and what it is doing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct State {
    /// The last status read, or `None` before the first read finishes.
    pub status: Option<Result<ServiceStatus, ServerError>>,
    /// The server operation awaiting its result. Menus withhold other server operations meanwhile.
    pub working: Option<Command>,
}

/// One menu row, in the order the menu shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Item {
    pub command: Command,
    pub label: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Presentation {
    /// The section heading naming the server, such as "SpaceTerm Preflight Server".
    pub server: String,
    /// The short state, such as "Running".
    pub state: &'static str,
    /// A sentence explaining the state, when it needs one.
    pub explanation: Option<String>,
    /// The icon's accessibility label and tooltip, which name the server and its state.
    pub summary: String,
    /// Whether the server is running. Other states dim the icon.
    pub running: bool,
    /// Open, then the server operations, then Hide. Hosts separate these three groups.
    pub open: Item,
    pub controls: [Item; 3],
    pub hide: Item,
}

impl Presentation {
    /// `name` is the build's display name, such as "SpaceTerm Preflight". `open` is whether this
    /// installation can launch its desktop.
    pub fn of(name: &str, state: State, open: bool) -> Self {
        let phase = Phase::of(state.status);
        let (label, explanation) = match state.working {
            Some(Command::Start) => ("Starting\u{2026}", None),
            Some(Command::Stop) => ("Stopping\u{2026}", None),
            Some(Command::Restart) => ("Restarting\u{2026}", None),
            _ => phase.describe(name),
        };
        let offered = |command| state.working.is_none() && phase.offers(command);
        let control = |command, label: &str| Item {
            command,
            label: label.to_owned(),
            enabled: offered(command),
        };
        let server = format!("{name} Server");
        Self {
            summary: format!("{server}: {label}"),
            server,
            state: label,
            explanation,
            running: phase == Phase::Running,
            open: Item {
                command: Command::Open,
                label: format!("Open {name}"),
                enabled: open,
            },
            controls: [
                control(Command::Start, "Start Server"),
                control(Command::Stop, "Stop Server\u{2026}"),
                control(Command::Restart, "Restart Server\u{2026}"),
            ],
            hide: Item {
                command: Command::Hide,
                label: "Hide Status Icon".to_owned(),
                enabled: state.working.is_none(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Checking,
    Running,
    /// The person stopped it. It stays stopped until an explicit Start.
    Stopped,
    /// Installed and not stopped by the person, yet not running, such as after a crash.
    NotRunning,
    NotInstalled,
    Unavailable(ServerError),
}

impl Phase {
    /// A running process wins over a recorded Stop, since the process is what ends Sessions.
    fn of(status: Option<Result<ServiceStatus, ServerError>>) -> Self {
        match status {
            None => Self::Checking,
            Some(Err(ServerError::NotInstalled | ServerError::NotSetUp)) => Self::NotInstalled,
            Some(Err(error)) => Self::Unavailable(error),
            Some(Ok(status)) if !status.installed => Self::NotInstalled,
            Some(Ok(status)) if status.running => Self::Running,
            Some(Ok(status)) if status.stopped => Self::Stopped,
            Some(Ok(_)) => Self::NotRunning,
        }
    }

    fn offers(self, command: Command) -> bool {
        match self {
            Self::Running => matches!(command, Command::Stop | Command::Restart),
            Self::Stopped | Self::NotRunning => command == Command::Start,
            // A Stop whose cleanup was not confirmed is retried only by another explicit Stop.
            Self::Unavailable(ServerError::CleanupUnresolved) => command == Command::Stop,
            Self::Checking | Self::NotInstalled | Self::Unavailable(_) => false,
        }
    }

    fn describe(self, name: &str) -> (&'static str, Option<String>) {
        match self {
            Self::Checking => ("Checking\u{2026}", None),
            Self::Running => ("Running", None),
            Self::Stopped => (
                "Stopped",
                Some("You stopped the server. It stays stopped until you start it.".to_owned()),
            ),
            Self::NotRunning => (
                "Not Running",
                Some("The server is installed but not running.".to_owned()),
            ),
            Self::NotInstalled => (
                "Not Installed",
                Some(format!("Open {name} to set up the server.")),
            ),
            Self::Unavailable(ServerError::CleanupUnresolved) => (
                badge(ServerError::CleanupUnresolved),
                Some(
                    "SpaceTerm couldn't confirm that the server's Terminal Sessions ended. \
                     Choose Stop Server to try again."
                        .to_owned(),
                ),
            ),
            Self::Unavailable(error) => (badge(error), Some(error.to_string())),
        }
    }
}

/// The short state for a status read that failed, matching Settings > Servers.
fn badge(error: ServerError) -> &'static str {
    match error {
        ServerError::UnsafeStorage => "Unsafe Storage",
        ServerError::CleanupUnresolved => "Cleanup Unresolved",
        ServerError::IncompatibleState => "Incompatible State",
        ServerError::RuntimeUnavailable => "Runtime Unavailable",
        ServerError::Unsupported => "Unsupported",
        _ => "Unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(installed: bool, running: bool, stopped: bool) -> State {
        State {
            status: Some(Ok(ServiceStatus {
                installed,
                enabled: installed && !stopped,
                running,
                stopped,
            })),
            working: None,
        }
    }

    fn offered(state: State) -> Vec<Command> {
        Presentation::of("SpaceTerm Preflight", state, true)
            .controls
            .into_iter()
            .filter(|item| item.enabled)
            .map(|item| item.command)
            .collect()
    }

    #[test]
    fn offers_only_the_operations_the_state_allows() {
        use Command::*;
        assert_eq!(offered(status(true, true, false)), [Stop, Restart]);
        // Only an explicit Start leaves a deliberate Stop.
        assert_eq!(offered(status(true, false, true)), [Start]);
        assert_eq!(offered(status(true, false, false)), [Start]);
        // A process running against a recorded Stop is still what ends Sessions.
        assert_eq!(offered(status(true, true, true)), [Stop, Restart]);
        assert!(offered(status(false, false, false)).is_empty());
        assert!(offered(State::default()).is_empty());
        let failed = State {
            status: Some(Err(ServerError::StorageUnavailable)),
            working: None,
        };
        assert!(offered(failed).is_empty());
        let unresolved = State {
            status: Some(Err(ServerError::CleanupUnresolved)),
            working: None,
        };
        assert_eq!(offered(unresolved), [Stop]);
        let stopping = State {
            working: Some(Stop),
            ..status(true, true, false)
        };
        assert!(offered(stopping).is_empty());
        // Exiting would abandon the admitted Stop before the service manager confirms it.
        assert!(
            !Presentation::of("SpaceTerm Preflight", stopping, true)
                .hide
                .enabled
        );
    }
}
