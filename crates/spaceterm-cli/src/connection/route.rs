//! Select a single server before any credential or transport is opened.
use super::{CredentialProvider, store_failure, usage};
use crate::args::{Command, CoreCommand, Global};
use crate::contract::{
    Candidate, CandidatesData, Code, Details, Failure, Level, MachineRef, TargetDetails,
};
use crate::scope::IdPrefix;
use crate::state::RouteKey;
use spaceterm_connections::{
    ConnectionStore, CredentialError, CredentialStore, Interaction, SavedConnection,
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
    pub core: MachineRef,
    pub recovered: Vec<crate::contract::RecoveredOperation>,
}

pub enum Route {
    Local,
    Paired(Box<SavedConnection>),
}
#[derive(Debug)]
pub struct ResolutionError {
    pub core: Option<Box<MachineRef>>,
    pub failure: Failure,
    pub data: CandidatesData,
}
impl From<Failure> for ResolutionError {
    fn from(failure: Failure) -> Self {
        Self {
            core: None,
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
        return Err(usage("--server"));
    }
    if let Some(prefix) = text.strip_prefix("id:") {
        IdPrefix::parse(prefix).ok_or_else(|| usage("--server"))?;
    }
    Ok(text)
}

/// Store commands and machine-only commands reject their route before reading local state.
pub fn validate_command(command: &Command, selector: &str) -> Result<(), Failure> {
    if selector == "local" {
        return Ok(());
    }
    match command {
        Command::Connection(_) => Err(usage("--server")),
        Command::Core(
            CoreCommand::Setup { .. }
            | CoreCommand::Pair { .. }
            | CoreCommand::Start
            | CoreCommand::EndAllSessions(_)
            | CoreCommand::Permissions { .. }
            | CoreCommand::Unpair { .. }
            | CoreCommand::Run { .. }
            | CoreCommand::Provision(_),
        ) => Err(Code::LocalOnly.into()),
        // The desktop on this machine presents only this machine's server to these commands.
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
        let prefix = IdPrefix::parse(prefix).ok_or_else(|| usage("--server"))?;
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
            Details::Target(TargetDetails::level(Level::Core)),
        )
        .into()),
        1 => Ok(Route::Paired(Box::new(
            matches.into_iter().next().expect("one match"),
        ))),
        _ => {
            let details = TargetDetails {
                level: Level::Core,
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
                    level: Level::Core,
                    id: Some(super::hex(&record.route)),
                    label: record.name,
                    path: None,
                    directory: None,
                    worktree: None,
                    panes: 0,
                })
                .collect();
            Err(ResolutionError {
                core: None,
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
            core: self.reference(&core),
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
                let credentials = credentials.open(saved.credential_store).map_err(|error| {
                    CoreError::Direct(match error {
                        CredentialError::InteractionRequired => DirectError::InteractionRequired,
                        CredentialError::Unavailable => DirectError::CredentialUnavailable,
                    })
                })?;
                let connector = Arc::new(Connector {
                    saved: saved.as_ref().clone(),
                    credentials,
                    interaction,
                    deadline,
                });
                CliRoute::Paired {
                    route: saved.route,
                    paired: saved.paired.clone(),
                    connector,
                }
            }
        };
        MachineConnection::for_cli(identity, route, agent)
    }
}
struct Connector {
    saved: SavedConnection,
    credentials: Arc<dyn CredentialStore>,
    interaction: Interaction,
    deadline: Option<Instant>,
}
impl DirectConnector for Connector {
    fn connect(&self) -> Result<DirectConnection, DirectError> {
        let secret = self
            .credentials
            .read(&self.saved.credential, self.interaction)
            .map_err(|error| match error {
                CredentialError::InteractionRequired => DirectError::InteractionRequired,
                CredentialError::Unavailable => DirectError::CredentialUnavailable,
            })?
            .ok_or(DirectError::CredentialUnavailable)?;
        let credential = DeviceCredential::from_bytes(&secret)?;
        match self.deadline {
            Some(deadline) => DirectConnection::connect_until(
                &self.saved.endpoint,
                &self.saved.paired,
                &credential,
                deadline,
            ),
            None => {
                DirectConnection::connect(&self.saved.endpoint, &self.saved.paired, &credential)
            }
        }
    }
}

pub fn from_global(global: &Global, identity: &str) -> Result<Route, ResolutionError> {
    let selector = selector(global)?;
    let store = ConnectionStore::new(
        spaceterm_core::cli_state_root(identity).map_err(crate::contract::from_core_error)?,
    );
    resolve(&selector, &store)
}

pub fn selector(global: &Global) -> Result<String, Failure> {
    let environment = if global.core.is_none() {
        std::env::var_os("SPACETERM_MACHINE")
    } else {
        None
    };
    let environment = environment
        .as_ref()
        .map(|value| value.to_str().ok_or_else(|| usage("--server")))
        .transpose()?;
    selected(global.core.as_deref(), environment).map(str::to_owned)
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
                error.core = Some(Box::new(selected.reference(&saved.paired.pin.core)));
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
