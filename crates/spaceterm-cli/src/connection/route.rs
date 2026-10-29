//! Select a single server before any credential or transport is opened.
use super::{CredentialProvider, store_failure, usage};
use crate::args::{Command, Global, ServerCommand};
use crate::contract::{
    Candidate, CandidatesData, Code, Details, Failure, Level, ServerRef, TargetDetails,
};
use crate::scope::IdPrefix;
use crate::state::RouteKey;
use spaceterm_connections::{
    ConnectionStore, CredentialError, CredentialStore, Interaction, SavedConnection,
};
use spaceterm_server::{
    CliRoute, ServerConnection, ServerError,
    direct::{self, DeviceCredential, DirectConnection, DirectConnector, DirectError},
    protocol::AgentName,
};
use std::sync::Arc;
use std::time::Instant;

pub struct AdmittedRoute {
    pub connection: ServerConnection,
    pub link: spaceterm_server::connection::link::ServerLink,
    pub server: ServerRef,
    pub recovered: Vec<crate::contract::RecoveredOperation>,
}

pub enum Route {
    Local,
    Paired(Box<SavedConnection>),
}
#[derive(Debug)]
pub struct ResolutionError {
    pub failure: Failure,
    pub data: CandidatesData,
}
impl From<Failure> for ResolutionError {
    fn from(failure: Failure) -> Self {
        Self {
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
        Command::Server(
            ServerCommand::Setup { .. }
            | ServerCommand::Pair { .. }
            | ServerCommand::Start
            | ServerCommand::Unpair { .. }
            | ServerCommand::Run { .. }
            | ServerCommand::Provision(_),
        ) => Err(Code::LocalOnly.into()),
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
            Details::Target(TargetDetails::level(Level::Server)),
        )
        .into()),
        1 => Ok(Route::Paired(Box::new(
            matches.into_iter().next().expect("one match"),
        ))),
        _ => {
            let details = TargetDetails {
                level: Level::Server,
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
                    level: Level::Server,
                    id: Some(super::hex(&record.route)),
                    label: record.name,
                    path: None,
                    directory: None,
                    worktree: None,
                    panes: 0,
                })
                .collect();
            Err(ResolutionError {
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
    pub fn reference(&self, server: &[u8; 32]) -> ServerRef {
        match self {
            Self::Local => ServerRef {
                name: "local".into(),
                route: "local".into(),
                id: super::hex(server),
            },
            Self::Paired(saved) => ServerRef {
                name: saved.name.clone(),
                route: format!("id:{}", super::hex(&saved.route)),
                id: super::hex(&saved.paired.pin.server),
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
    ) -> Result<ServerConnection, Failure> {
        self.server_connection(identity, agent, credentials, interaction, deadline)
            .map_err(crate::contract::from_server_error)
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
            .map_err(crate::contract::from_server_error)?;
        let server = connection
            .admitted_server_id()
            .map_err(crate::contract::from_server_error)?;
        if let Self::Paired(saved) = self
            && saved.paired.pin.server != server
        {
            link.close();
            return Err(Code::ServerIdentityChanged.into());
        }
        let state =
            crate::state::CliState::open(identity).map_err(crate::contract::from_server_error)?;
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
                self.server_connection(identity, agent.cloned(), credentials, interaction, deadline)
            },
            deadline,
        )?;
        Ok(AdmittedRoute {
            connection,
            link,
            server: self.reference(&server),
            recovered,
        })
    }

    pub(crate) fn server_connection(
        &self,
        identity: &str,
        agent: Option<AgentName>,
        credentials: &dyn CredentialProvider,
        interaction: Interaction,
        deadline: Instant,
    ) -> Result<ServerConnection, ServerError> {
        let route = match self {
            Self::Local => CliRoute::Local,
            Self::Paired(saved) => {
                let credentials = credentials.open().map_err(|error| {
                    ServerError::Direct(match error {
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
        ServerConnection::for_cli(identity, route, agent)
    }
}
struct Connector {
    saved: SavedConnection,
    credentials: Arc<dyn CredentialStore>,
    interaction: Interaction,
    deadline: Instant,
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
        direct::DirectConnection::connect_until(
            &self.saved.endpoint,
            &self.saved.paired,
            &credential,
            self.deadline,
        )
    }
}

pub fn from_global(global: &Global, identity: &str) -> Result<Route, ResolutionError> {
    let environment = if global.server.is_none() {
        std::env::var_os("SPACETERM_SERVER")
    } else {
        None
    };
    let environment = environment
        .as_ref()
        .map(|value| value.to_str().ok_or_else(|| usage("--server")))
        .transpose()?;
    let selector = selected(global.server.as_deref(), environment)?;
    let store = ConnectionStore::new(
        spaceterm_server::cli_state_root(identity).map_err(crate::contract::from_server_error)?,
    );
    resolve(selector, &store)
}
