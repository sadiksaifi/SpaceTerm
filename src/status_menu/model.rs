//! What the status item shows and offers, derived from the service's state alone.
use crate::application_identity::ApplicationIdentity;
use spaceterm_core::{CoreError, service::ServiceStatus};

/// One person-chosen operation the status menu offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    /// Restores this client's windows. It never starts, installs, or stops the server itself.
    Open,
    Start,
    /// Asks first, since it ends every client's Terminal Sessions.
    Stop,
    /// Asks first, since it ends every client's Terminal Sessions.
    Restart,
    /// Opens Settings on Servers and starts the Update Server journey there, which reads the
    /// server again and asks before it changes anything.
    Update,
    /// Exits the whole client, status item included. The server and its Terminal Sessions keep
    /// running. Unavailable while a server operation runs, so exiting never abandons an admitted
    /// operation's cleanup.
    Quit,
}

/// What the status item last learned and what it is doing.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct State {
    /// The last status read, or `None` before the first read finishes.
    pub(crate) status: Option<Result<ServiceStatus, CoreError>>,
    /// The server operation awaiting its result. Menus withhold other server operations meanwhile.
    pub(crate) working: Option<Command>,
    /// A server control window is asking about or running an operation. Menus withhold the
    /// server operations and Quit until it closes.
    pub(crate) asking: bool,
    /// The last read found a verified server build the Server version row offers to update to.
    pub(crate) update: bool,
}

/// One menu row, in the order the menu shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Item {
    pub(crate) command: Command,
    pub(crate) label: String,
    pub(crate) enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Presentation {
    /// The section heading naming the server, such as "SpaceTerm Preflight Server".
    pub(crate) core: String,
    /// The short state, such as "Running".
    pub(crate) state: &'static str,
    /// A sentence explaining the state, when it needs one.
    pub(crate) explanation: Option<String>,
    /// The icon's accessibility label and tooltip, which name the server and its state.
    pub(crate) summary: String,
    /// Whether the server is running. Other states dim the icon.
    pub(crate) running: bool,
    /// Open, then the server operations, then Quit. Hosts separate these three groups.
    pub(crate) open: Item,
    pub(crate) controls: [Item; 3],
    /// Update Server, shown only while a verified build is offered. An approved build offers
    /// nothing to update to, so the row is absent rather than dimmed.
    pub(crate) update: Option<Item>,
    pub(crate) quit: Item,
}

impl Presentation {
    /// Names the build and its SpaceTerm Core from `identity`.
    pub(crate) fn of(identity: ApplicationIdentity, state: State) -> Self {
        let name = identity.display_name();
        let phase = Phase::of(state.status);
        let (label, explanation) = match state.working {
            Some(Command::Start) => ("Starting\u{2026}", None),
            Some(Command::Stop) => ("Stopping\u{2026}", None),
            Some(Command::Restart) => ("Restarting\u{2026}", None),
            _ => phase.describe(name),
        };
        let idle = state.working.is_none() && !state.asking;
        let offered = |command| idle && phase.offers(command);
        let control = |command, label: &str| Item {
            command,
            label: label.to_owned(),
            enabled: offered(command),
        };
        let core = identity.core_name().to_owned();
        Self {
            summary: format!("{core}: {label}"),
            core,
            state: label,
            explanation,
            running: phase == Phase::Running,
            open: Item {
                command: Command::Open,
                label: format!("Open {name}"),
                enabled: true,
            },
            controls: [
                control(Command::Start, "Start SpaceTerm Core"),
                control(Command::Stop, "Stop SpaceTerm Core\u{2026}"),
                control(Command::Restart, "Restart SpaceTerm Core\u{2026}"),
            ],
            update: state.update.then(|| Item {
                command: Command::Update,
                label: "Update SpaceTerm Core\u{2026}".to_owned(),
                enabled: idle && phase.offers(Command::Update),
            }),
            quit: Item {
                command: Command::Quit,
                label: format!("Quit {name}"),
                enabled: idle,
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
    Unavailable(CoreError),
}

impl Phase {
    /// A running process wins over a recorded Stop, since the process is what ends Sessions.
    fn of(status: Option<Result<ServiceStatus, CoreError>>) -> Self {
        match status {
            None => Self::Checking,
            Some(Err(CoreError::NotInstalled | CoreError::NotSetUp)) => Self::NotInstalled,
            Some(Err(error)) => Self::Unavailable(error),
            Some(Ok(status)) if !status.installed => Self::NotInstalled,
            Some(Ok(status)) if status.running => Self::Running,
            Some(Ok(status)) if status.stopped => Self::Stopped,
            Some(Ok(_)) => Self::NotRunning,
        }
    }

    fn offers(self, command: Command) -> bool {
        match self {
            Self::Running => matches!(command, Command::Stop | Command::Restart | Command::Update),
            Self::Stopped | Self::NotRunning => matches!(command, Command::Start | Command::Update),
            // A Stop whose cleanup was not confirmed is retried only by another explicit Stop.
            Self::Unavailable(CoreError::CleanupUnresolved) => command == Command::Stop,
            Self::Checking | Self::NotInstalled | Self::Unavailable(_) => false,
        }
    }

    fn describe(self, name: &str) -> (&'static str, Option<String>) {
        match self {
            Self::Checking => ("Checking\u{2026}", None),
            Self::Running => ("Running", None),
            Self::Stopped => (
                "Stopped",
                Some("SpaceTerm Core stays stopped until you start it.".to_owned()),
            ),
            Self::NotRunning => ("Not running", Some("Installed but not running.".to_owned())),
            Self::NotInstalled => (
                "Not installed",
                Some(format!("Open {name} to set up SpaceTerm Core.")),
            ),
            Self::Unavailable(CoreError::CleanupUnresolved) => (
                badge(CoreError::CleanupUnresolved),
                Some(
                    "SpaceTerm couldn\u{2019}t confirm that SpaceTerm Core\u{2019}s Terminal Sessions ended. \
                     Choose Stop SpaceTerm Core to try again."
                        .to_owned(),
                ),
            ),
            Self::Unavailable(CoreError::Stopped) => (
                badge(CoreError::Stopped),
                Some("Choose Start SpaceTerm Core.".to_owned()),
            ),
            Self::Unavailable(error) => (badge(error), Some(error.to_string())),
        }
    }
}

/// The short state for a status read that failed, matching Settings > Servers.
fn badge(error: CoreError) -> &'static str {
    match error {
        CoreError::UnsafeStorage => "Unsafe storage",
        CoreError::CleanupUnresolved => "Cleanup unresolved",
        CoreError::IncompatibleState => "Incompatible state",
        CoreError::RuntimeUnavailable => "Runtime unavailable",
        CoreError::Unsupported => "Unsupported",
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
            asking: false,
            update: false,
        }
    }

    #[test]
    fn each_build_names_spaceterm_core_in_full_before_its_qualifier() {
        for (identity, core) in [
            (ApplicationIdentity::production(), "SpaceTerm Core"),
            (ApplicationIdentity::preflight(), "SpaceTerm Core Preflight"),
            (
                ApplicationIdentity::development(),
                "SpaceTerm Core Development",
            ),
        ] {
            let presentation = Presentation::of(identity, status(true, true, false));
            assert_eq!(presentation.core, core);
            assert_eq!(presentation.summary, format!("{core}: Running"));
        }
    }

    fn offered(state: State) -> Vec<Command> {
        Presentation::of(ApplicationIdentity::preflight(), state)
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
            status: Some(Err(CoreError::StorageUnavailable)),
            ..State::default()
        };
        assert!(offered(failed).is_empty());
        let unresolved = State {
            status: Some(Err(CoreError::CleanupUnresolved)),
            ..State::default()
        };
        assert_eq!(offered(unresolved), [Stop]);
        let stopping = State {
            working: Some(Stop),
            ..status(true, true, false)
        };
        assert!(offered(stopping).is_empty());
        // Exiting would abandon the admitted Stop before the service manager confirms it.
        assert!(
            !Presentation::of(ApplicationIdentity::preflight(), stopping)
                .quit
                .enabled
        );
        // A control window may be about to admit an operation, or already running one.
        let asking = State {
            asking: true,
            ..status(true, true, false)
        };
        assert!(offered(asking).is_empty());
        assert!(
            !Presentation::of(ApplicationIdentity::preflight(), asking)
                .quit
                .enabled
        );
    }

    #[test]
    fn update_machine_appears_only_while_a_verified_build_is_offered() {
        let update =
            |state: State| Presentation::of(ApplicationIdentity::preflight(), state).update;
        assert_eq!(update(status(true, true, false)), None);
        let offered = |state: State| State {
            update: true,
            ..state
        };
        for state in [status(true, true, false), status(true, false, true)] {
            let item = update(offered(state)).expect("the row is shown");
            assert_eq!(item.label, "Update SpaceTerm Core\u{2026}");
            assert!(item.enabled);
        }
        // Another operation, or a control window asking, holds it back.
        for state in [
            State {
                working: Some(Command::Stop),
                ..offered(status(true, true, false))
            },
            State {
                asking: true,
                ..offered(status(true, true, false))
            },
            State {
                status: Some(Err(CoreError::StorageUnavailable)),
                ..offered(State::default())
            },
        ] {
            assert!(!update(state).expect("the row is shown").enabled);
        }
    }
}
