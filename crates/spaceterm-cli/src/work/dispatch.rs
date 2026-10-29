//! The one mutation boundary. Its implementation owns sequence allocation, durable journals,
//! leftover settlement and acknowledgement under one writer lock.
use crate::contract::{Failure, RecoveredOperation};
use spaceterm_server::{
    ServerConnection,
    hierarchy::{Id, Mutation, Outcome},
};

pub trait Dispatch {
    fn server_id(&self) -> Id;
    fn client_id(&self) -> Id;
    fn resolve_leftovers(&mut self) -> Result<Vec<RecoveredOperation>, Failure>;
    /// Returns only after retirement and acknowledgement, or a typed unconfirmed outcome.
    fn submit(&mut self, mutation: Mutation) -> Result<Outcome, Failure>;
}

/// Stream D supplies the concrete single-lock adapter. No other B code opens a journal or
/// takes the identity's writer lock.
pub struct Configuration<'a> {
    pub identity: &'a str,
    pub route: crate::state::RouteKey,
    pub agent: Option<&'a spaceterm_server::protocol::AgentName>,
    pub deadline: std::time::Instant,
}

pub fn open(
    _connection: &ServerConnection,
    _configuration: &Configuration<'_>,
) -> Result<Box<dyn Dispatch>, Failure> {
    Err(Failure::new(crate::contract::Code::Unsupported))
}
