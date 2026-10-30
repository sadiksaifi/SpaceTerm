//! The one mutation boundary. The journal owns sequence allocation, recovery and
//! acknowledgement under one writer lock.
use crate::{
    contract::{Code, Failure, RecoveredOperation, from_server_error},
    journal::{self, Editor},
    state::{CliState, RouteKey},
};
use spaceterm_server::{
    ServerConnection, ServerError,
    connection::link::ServerLink,
    hierarchy::{Id, Mutation, Outcome},
    protocol::AgentName,
};
use std::time::Instant;

pub trait Dispatch {
    fn server_id(&self) -> Id;
    fn client_id(&self) -> Id;
    fn resolve_leftovers(&mut self) -> Result<Vec<RecoveredOperation>, Failure>;
    fn submit(&mut self, mutation: Mutation) -> Result<Outcome, Failure>;
    /// Settlement repeated under the writer may discover outcomes an active writer held earlier.
    fn recovered(&self) -> &[RecoveredOperation] {
        &[]
    }
}

pub struct Configuration<'a> {
    pub identity: &'a str,
    pub route: RouteKey,
    pub agent: Option<&'a AgentName>,
    pub deadline: Instant,
    pub mutating: bool,
}

type Connect<'a> = dyn FnMut(Option<&AgentName>) -> Result<ServerConnection, ServerError> + 'a;

pub fn open<'a>(
    connection: &'a ServerConnection,
    link: &'a ServerLink,
    configuration: &Configuration<'_>,
    connect: &'a mut Connect<'a>,
) -> Result<Box<dyn Dispatch + 'a>, Failure> {
    Ok(Box::new(Journal {
        server: connection.admitted_server_id().map_err(from_server_error)?,
        client: connection.admitted_client_id().map_err(from_server_error)?,
        state: CliState::open(configuration.identity).map_err(from_server_error)?,
        route: configuration.route,
        agent: configuration.agent.cloned(),
        connection,
        link,
        connect,
        deadline: configuration.deadline,
        mutating: configuration.mutating,
        editor: None,
        settled: false,
    }))
}

struct Journal<'a> {
    server: Id,
    client: Id,
    state: CliState,
    route: RouteKey,
    agent: Option<AgentName>,
    connection: &'a ServerConnection,
    link: &'a ServerLink,
    connect: &'a mut Connect<'a>,
    deadline: Instant,
    mutating: bool,
    editor: Option<Editor<'a>>,
    settled: bool,
}

impl Dispatch for Journal<'_> {
    fn server_id(&self) -> Id {
        self.server
    }
    fn client_id(&self) -> Id {
        self.client
    }
    fn resolve_leftovers(&mut self) -> Result<Vec<RecoveredOperation>, Failure> {
        if self.settled {
            return Ok(Vec::new());
        }
        let recovered = journal::settle(
            &self.state,
            &self.route,
            journal::Current {
                agent: self.agent.as_ref(),
                connection: self.connection,
                link: self.link,
                writer: None,
            },
            &mut *self.connect,
            self.deadline,
        )?;
        self.settled = true;
        Ok(recovered)
    }
    fn submit(&mut self, mutation: Mutation) -> Result<Outcome, Failure> {
        if !self.mutating {
            return Err(Failure::new(Code::ProtocolError));
        }
        if self.editor.is_none() {
            self.editor = Some(Editor::open(
                &self.state,
                self.route,
                self.agent.as_ref(),
                self.connection,
                self.link,
                &mut *self.connect,
                self.deadline,
            )?);
        }
        let editor = self
            .editor
            .as_mut()
            .ok_or_else(|| Failure::new(Code::ProtocolError))?;
        Ok(editor.mutate(mutation, self.deadline)?.outcome.outcome)
    }
    fn recovered(&self) -> &[RecoveredOperation] {
        self.editor
            .as_ref()
            .map_or(&[], |editor| editor.recovered())
    }
}
