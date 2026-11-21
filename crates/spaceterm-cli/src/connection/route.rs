//! Select a single machine before any credential or transport is opened.
use super::{CredentialProvider, store_failure, usage};
use crate::args::{Command, CoreCommand, Global};
use crate::contract::{
    Candidate, CandidatesData, Code, Details, Failure, Level, MachineRef, TargetDetails,
};
use crate::scope::IdPrefix;
use crate::state::RouteKey;
use spaceterm_client::{
    ConnectionStore, CredentialError, CredentialStore, Interaction, SavedConnection, UnpairState,
};
use spaceterm_core::{
    CliRoute, CoreError, MachineConnection,
    direct::{DeviceCredential, DirectConnection, DirectConnector, DirectError},
    protocol::AgentName,
};
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Instant;

pub struct AdmittedRoute {
    pub connection: MachineConnection,
    pub link: spaceterm_core::connection::link::CoreLink,
    pub machine: MachineRef,
    pub recovered: Vec<crate::contract::RecoveredOperation>,
}

pub enum Route {
    Local,
    Paired(Box<SavedConnection>),
}
#[derive(Debug)]
pub struct ResolutionError {
    pub machine: Option<Box<MachineRef>>,
    pub failure: Failure,
    pub data: CandidatesData,
}
impl From<Failure> for ResolutionError {
    fn from(failure: Failure) -> Self {
        Self {
            machine: None,
            failure,
            data: CandidatesData {
                candidates: Vec::new(),
            },
        }
    }
}

pub fn selected<'a>(
    explicit: Option<&'a str>,
    environment: Option<&'a str>,
) -> Result<&'a str, Failure> {
    let text = explicit.or(environment).unwrap_or("local");
    if text.is_empty() || text.len() > 128 || text.chars().any(char::is_control) {
        return Err(usage("--machine"));
    }
    if let Some(prefix) = text.strip_prefix("id:") {
        IdPrefix::parse(prefix).ok_or_else(|| usage("--machine"))?;
    }
    Ok(text)
}

/// `machine` commands, and commands that act only on this machine, reject a paired route
/// before reading local state.
pub fn validate_command(command: &Command, selector: &str) -> Result<(), Failure> {
    if selector == "local" {
        return Ok(());
    }
    match command {
        Command::Machine(_) => Err(usage("--machine")),
        Command::Core(
            CoreCommand::Status
            | CoreCommand::Restart(_)
            | CoreCommand::Update { .. }
            | CoreCommand::Network { .. },
        ) => Ok(()),
        // A stopped SpaceTerm Core can't be started from a paired machine, so neither stops there.
        Command::Core(_) | Command::Internal(_) => Err(Code::LocalOnly.into()),
        // The desktop on this machine presents only this machine's SpaceTerm Core to these
        // commands.
        Command::Open(_) | Command::Show(_) | Command::Diagnostics => Err(Code::LocalOnly.into()),
        _ => Ok(()),
    }
}

pub fn resolve(selector: &str, store: &ConnectionStore) -> Result<Route, ResolutionError> {
    selected(Some(selector), None)?;
    if selector == "local" {
        return Ok(Route::Local);
    }
    let records = store.load().map_err(store_failure)?;
    let matches: Vec<_> = if let Some(prefix) = selector.strip_prefix("id:") {
        let prefix = IdPrefix::parse(prefix).ok_or_else(|| usage("--machine"))?;
        records
            .into_iter()
            .filter(|record| prefix.matches(&record.route))
            .collect()
    } else {
        records
            .into_iter()
            .filter(|record| record.name == selector)
            .collect()
    };
    match matches.len() {
        0 => Err(Failure::with(
            Code::NotFound,
            Details::Target(TargetDetails::level(Level::Machine)),
        )
        .into()),
        1 => Ok(Route::Paired(Box::new(
            matches.into_iter().next().expect("one match"),
        ))),
        _ => {
            let details = TargetDetails {
                level: Level::Machine,
                parent: None,
                count: Some(matches.len() as u32),
                ids: matches
                    .iter()
                    .map(|record| super::hex(&record.route))
                    .collect(),
            };
            let candidates = matches
                .into_iter()
                .map(|record| Candidate {
                    level: Level::Machine,
                    id: Some(super::hex(&record.route)),
                    label: record.name,
                    path: None,
                    directory: None,
                    worktree: None,
                    panes: 0,
                })
                .collect();
            Err(ResolutionError {
                machine: None,
                failure: Failure::with(Code::AmbiguousTarget, Details::Target(details)),
                data: CandidatesData { candidates },
            })
        }
    }
}

impl Route {
    pub fn key(&self) -> RouteKey {
        match self {
            Self::Local => RouteKey::Local,
            Self::Paired(saved) => RouteKey::Paired(saved.route),
        }
    }
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
    pub fn reference(&self, core: &[u8; 32]) -> MachineRef {
        match self {
            Self::Local => MachineRef {
                name: "local".into(),
                route: "local".into(),
                id: super::hex(core),
            },
            Self::Paired(saved) => MachineRef {
                name: saved.name.clone(),
                route: format!("id:{}", super::hex(&saved.route)),
                id: super::hex(&saved.paired.pin.core),
            },
        }
    }
    pub fn connect(
        &self,
        identity: &str,
        agent: Option<AgentName>,
        credentials: &dyn CredentialProvider,
        interaction: Interaction,
        deadline: Instant,
    ) -> Result<MachineConnection, Failure> {
        self.machine_connection(identity, agent, credentials, interaction, Some(deadline))
            .map_err(crate::contract::from_core_error)
    }

    /// Admit the selected route and settle its held outcomes before a passive command.
    /// Mutating callers use `Editor::open`, which owns the writer lock and settlement.
    pub fn admit(
        &self,
        identity: &str,
        agent: Option<AgentName>,
        credentials: &dyn CredentialProvider,
        interaction: Interaction,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<AdmittedRoute, Failure> {
        let connection =
            self.connect(identity, agent.clone(), credentials, interaction, deadline)?;
        let link = connection
            .link_cancellable(&|| cancelled() || Instant::now() >= deadline)
            .map_err(crate::contract::from_core_error)?;
        let core = connection
            .admitted_machine_id()
            .map_err(crate::contract::from_core_error)?;
        if let Self::Paired(saved) = self
            && saved.paired.pin.core != core
        {
            link.close();
            return Err(Code::MachineIdentityChanged.into());
        }
        let state =
            crate::state::CliState::open(identity).map_err(crate::contract::from_core_error)?;
        let recovered = crate::journal::settle(
            &state,
            &self.key(),
            crate::journal::Current {
                agent: agent.as_ref(),
                connection: &connection,
                link: &link,
                writer: None,
            },
            &mut |agent| {
                self.machine_connection(
                    identity,
                    agent.cloned(),
                    credentials,
                    interaction,
                    Some(deadline),
                )
            },
            deadline,
        )?;
        Ok(AdmittedRoute {
            connection,
            link,
            machine: self.reference(&core),
            recovered,
        })
    }

    pub(crate) fn machine_connection(
        &self,
        identity: &str,
        agent: Option<AgentName>,
        credentials: &dyn CredentialProvider,
        interaction: Interaction,
        deadline: Option<Instant>,
    ) -> Result<MachineConnection, CoreError> {
        let route = match self {
            Self::Local => CliRoute::Local,
            Self::Paired(saved) => {
                if saved.unpair != UnpairState::Paired {
                    return Err(DirectError::Revoked.into());
                }
                let credentials = credentials
                    .open(saved.credential_store)
                    .map_err(credential_error)?;
                CliRoute::Paired {
                    route: saved.route,
                    paired: saved.paired.clone(),
                    connector: Arc::new(PairedRouteConnector {
                        saved: saved.as_ref().clone(),
                        credentials,
                        interaction,
                        deadline,
                    }),
                }
            }
        };
        MachineConnection::for_cli(identity, route, agent)
    }
}

/// The CLI reads a fresh native credential in the foreground so it can prompt the person.
/// Only the network dial uses the action's transport deadline.
struct PairedRouteConnector {
    saved: SavedConnection,
    credentials: Arc<dyn CredentialStore>,
    interaction: Interaction,
    deadline: Option<Instant>,
}

impl PairedRouteConnector {
    fn dial(&self, deadline: Option<Instant>) -> Result<DirectConnection, DirectError> {
        let secret = self
            .credentials
            .read(&self.saved.credential, self.interaction)
            .map_err(credential_error)?
            .ok_or(DirectError::CredentialUnavailable)?;
        let credential = DeviceCredential::from_bytes(&secret)?;
        let endpoint = &self.saved.endpoint;
        let paired = &self.saved.paired;
        match deadline {
            Some(deadline) => {
                DirectConnection::connect_until(endpoint, paired, &credential, deadline)
            }
            None => DirectConnection::connect(endpoint, paired, &credential),
        }
    }
}

impl DirectConnector for PairedRouteConnector {
    fn connect(&self) -> Result<DirectConnection, DirectError> {
        self.dial(self.deadline)
    }

    fn connect_by(&self, deadline: Instant) -> Result<DirectConnection, DirectError> {
        // Service actions can start a new network budget after a confirmation prompt.
        self.dial(Some(deadline))
    }
}

fn credential_error(error: CredentialError) -> DirectError {
    match error {
        CredentialError::InteractionRequired => DirectError::InteractionRequired,
        CredentialError::Unavailable => DirectError::CredentialUnavailable,
    }
}

pub fn from_global(global: &Global, identity: &str) -> Result<Route, ResolutionError> {
    let selector = selector(global)?;
    let store = spaceterm_core::native_client::connection_store(
        spaceterm_core::cli_state_root(identity).map_err(crate::contract::from_core_error)?,
    );
    resolve(&selector, &store)
}

pub fn selector(global: &Global) -> Result<String, Failure> {
    let environment = if global.machine.is_none() {
        std::env::var_os("SPACETERM_MACHINE")
    } else {
        None
    };
    let environment = environment
        .as_ref()
        .map(|value| value.to_str().ok_or_else(|| usage("--machine")))
        .transpose()?;
    selected(global.machine.as_deref(), environment).map(str::to_owned)
}

pub struct RoutedConnection {
    pub selected: Route,
    pub connection: MachineConnection,
    pub agent: Option<AgentName>,
    pub interaction: Interaction,
}

/// Construct the selected transport without opening a work or service lane.
/// An absent deadline leaves long service operations free to reconnect later.
pub fn connect(
    identity: &str,
    global: &Global,
    credentials: &dyn CredentialProvider,
    deadline: Option<Instant>,
) -> Result<RoutedConnection, ResolutionError> {
    let agent = if let Some(agent) = &global.agent {
        Some(agent.clone())
    } else {
        std::env::var_os("SPACETERM_AS")
            .map(|name| {
                name.to_str()
                    .and_then(|name| name.parse().ok())
                    .ok_or_else(|| usage("--as"))
            })
            .transpose()?
    };
    let selected = from_global(global, identity)?;
    let interaction = if global.json
        || global.no_input
        || !std::io::stdin().is_terminal()
        || !std::io::stderr().is_terminal()
    {
        Interaction::Quiet
    } else {
        Interaction::Requested
    };
    let connection = selected
        .machine_connection(identity, agent.clone(), credentials, interaction, deadline)
        .map_err(|error| {
            let mut error = ResolutionError::from(crate::contract::from_core_error(error));
            if let Route::Paired(saved) = &selected {
                error.machine = Some(Box::new(selected.reference(&saved.paired.pin.core)));
            }
            error
        })?;
    Ok(RoutedConnection {
        selected,
        connection,
        agent,
        interaction,
    })
}

impl ResolutionError {
    pub fn operational_data(&self) -> serde_json::Value {
        if self.data.candidates.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({"candidates": self.data.candidates})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DirectConnector, PairedRouteConnector, validate_command};
    use crate::args::Cli;
    use clap::Parser;

    /// The one credential of a Saved Connection.
    struct OneCredential {
        bytes: zeroize::Zeroizing<Vec<u8>>,
        caller: std::thread::ThreadId,
    }
    impl spaceterm_client::CredentialStore for OneCredential {
        fn read(
            &self,
            _: &[u8; 32],
            _: super::Interaction,
        ) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, super::CredentialError> {
            assert_eq!(std::thread::current().id(), self.caller);
            Ok(Some(self.bytes.clone()))
        }
        fn insert(&self, _: &[u8; 32], _: &[u8]) -> Result<(), super::CredentialError> {
            Err(super::CredentialError::Unavailable)
        }
        fn remove(
            &self,
            _: &[u8; 32],
            _: super::Interaction,
        ) -> Result<(), super::CredentialError> {
            Err(super::CredentialError::Unavailable)
        }
    }

    /// An interactive confirmation can outlast the deadline the paired connector took when the
    /// command began. The approved action's service-lane dials then follow its own budget, so
    /// the receipt dial after the person answered does not fail at once.
    #[test]
    fn a_service_lane_dial_follows_its_request_deadline_after_a_confirmation_prompt() {
        use spaceterm_core::direct::{DeviceCredential, MachinePin, PairedDevice};
        use std::time::{Duration, Instant};
        // A silent endpoint never answers a handshake, so a dial ends only at its deadline.
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let credential = DeviceCredential::generate().unwrap();
        let saved = super::SavedConnection {
            name: "Penguin".into(),
            route: [1; 32],
            endpoint: format!("quic://{}", silent.local_addr().unwrap()),
            paired: PairedDevice {
                pin: MachinePin {
                    build: "spaceterm-development".into(),
                    core: [3; 32],
                    public_key: [4; 32],
                    certificate: [5; 32],
                },
                pairing: [6; 32],
                client: [7; 32],
            },
            credential: [1; 32],
            credential_store: spaceterm_client::StoreKind::Keychain,
            unpair: spaceterm_client::UnpairState::Paired,
        };
        let connector = PairedRouteConnector {
            saved,
            credentials: std::sync::Arc::new(OneCredential {
                bytes: zeroize::Zeroizing::new(credential.expose_bytes().to_vec()),
                caller: std::thread::current().id(),
            }),
            interaction: super::Interaction::Quiet,
            // Taken when the command began; reading the confirmation used all of it.
            deadline: Some(Instant::now()),
        };
        let began = Instant::now();
        assert!(connector.connect().is_err());
        assert!(
            began.elapsed() < Duration::from_millis(250),
            "{:?}",
            began.elapsed()
        );
        let began = Instant::now();
        assert!(
            connector
                .connect_by(began + Duration::from_millis(600))
                .is_err()
        );
        let waited = began.elapsed();
        assert!(
            waited >= Duration::from_millis(500) && waited < Duration::from_secs(3),
            "{waited:?}"
        );
    }

    #[test]
    fn paired_routes_admit_remote_admin_verbs_and_work_commands() {
        for args in [
            &["status"][..],
            &["restart"],
            &["restart", "--confirm", "c1-00"],
            &["update"],
            &["update", "--archive", "a.tar", "--signature", "a.sig"],
            &["network"],
            &["network", "off", "--confirm", "c1-00"],
            &["list"],
            &["workspace", "create", "api", "--directory", "/srv/api"],
            &["worktree", "create", "feature"],
            &["tab", "create"],
            &["pane", "split"],
            &["pane", "read"],
        ] {
            let cli = Cli::try_parse_from(
                ["spaceterm", "--machine", "penguin"]
                    .into_iter()
                    .chain(args.iter().copied()),
            )
            .unwrap();
            assert!(
                validate_command(&cli.command, "penguin").is_ok(),
                "{args:?}"
            );
        }
    }
}
