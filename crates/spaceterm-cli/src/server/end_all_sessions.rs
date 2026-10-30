//! End every live Terminal Session through account-local work admission and the shared journal.
use super::{Context, Reply, offline_token};
use crate::{
    contract::{
        self, Code, Confirmation, EndAllSessionsEffects, EndedSession, Envelope, Failure,
        ServerEndAllSessions, ServerRef,
    },
    state::RouteKey,
    work::dispatch::{self, Dispatch},
};
use spaceterm_server::{
    connection::link::{Health, ServerLink},
    hierarchy::{Inventory, Mutation},
    labels,
};
use std::time::{Duration, Instant, SystemTime};

const COMMAND: &str = "server.end-all-sessions";
const PURPOSE: &str = "end-all-sessions";

pub fn run(context: &Context<'_>, given: Option<&str>) -> Reply {
    if let Err(failure) = context.require_local() {
        return Reply::failure(COMMAND, None, failure);
    }
    let mut server = None;
    let mut recovered = Vec::new();
    let result = (|| {
        if crate::interrupt::interrupted() {
            return Err(Code::Interrupted.into());
        }
        let deadline = Instant::now() + context.timeout(Duration::from_secs(30));
        let connection = context.local_connection()?;
        let link = connection
            .link_cancellable(&|| crate::interrupt::interrupted() || Instant::now() >= deadline)
            .map_err(|error| {
                if crate::interrupt::interrupted() {
                    Code::Interrupted.into()
                } else if Instant::now() >= deadline {
                    Code::Timeout.into()
                } else {
                    contract::from_server_error(error)
                }
            })?;
        let reference = ServerRef {
            name: "local".into(),
            route: "local".into(),
            id: super::hex(
                &connection
                    .admitted_server_id()
                    .map_err(contract::from_server_error)?,
            ),
        };
        server = Some(reference.clone());
        let agent = context.agent();
        let mut connect = |agent: Option<&spaceterm_server::protocol::AgentName>| {
            spaceterm_server::ServerConnection::for_cli(
                context.identity,
                spaceterm_server::CliRoute::Local,
                agent.cloned(),
            )
        };
        let mut journal = dispatch::open(
            &connection,
            &link,
            &dispatch::Configuration {
                identity: context.identity,
                route: RouteKey::Local,
                agent: agent.as_ref(),
                deadline,
                mutating: true,
            },
            &mut connect,
        )?;
        recovered = journal.resolve_leftovers()?;
        let inventory = inventory(&link, deadline)?;
        let result = execute(
            &inventory,
            &mut *journal,
            reference,
            given,
            SystemTime::now(),
        );
        recovered.extend_from_slice(journal.recovered());
        result
    })();
    let mut reply = result.unwrap_or_else(|failure| Reply::failure(COMMAND, server, failure));
    reply.envelope = super::with_recovered(reply.envelope, recovered);
    reply
}

fn inventory(link: &ServerLink, deadline: Instant) -> Result<Inventory, Failure> {
    loop {
        if crate::interrupt::interrupted() {
            return Err(Code::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(Code::Timeout.into());
        }
        if let Some((_, revision)) = link.inventory() {
            return Ok(revision.inventory.clone());
        }
        if let Health::Closed(error) = link.health() {
            return Err(contract::from_server_error(error.into()));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(crate) fn execute(
    inventory: &Inventory,
    journal: &mut dyn Dispatch,
    server: ServerRef,
    given: Option<&str>,
    now: SystemTime,
) -> Result<Reply, Failure> {
    let confirmation = inventory.close.as_ref().ok_or(Code::Changed)?;
    let observation = postcard::to_allocvec(&(
        journal.server_id(),
        journal.client_id(),
        confirmation.boot,
        &confirmation.sessions,
    ))
    .map_err(|_| Failure::new(Code::ProtocolError))?;
    let mut sessions = Vec::new();
    for reviewed in &confirmation.sessions {
        let Some(session) = reviewed.session else {
            continue;
        };
        let pane = inventory
            .workspaces
            .iter()
            .flat_map(|w| &w.tabs)
            .flat_map(|t| &t.panes)
            .find(|p| p.id == reviewed.id)
            .ok_or(Code::ProtocolError)?;
        sessions.push(EndedSession {
            pane: super::hex(&pane.id),
            session: super::hex(&session),
            label: labels::pane(pane).line,
        });
    }
    if let Some(given) = given {
        offline_token::verify(PURPOSE, &observation, given, now)?;
        contract::from_outcome(&journal.submit(Mutation::EndAllSessions {
            confirmation: confirmation.clone(),
        })?)?;
        let text = format!("Ended {} Terminal Sessions.\n", sessions.len());
        return Ok(Reply::new(
            &Envelope::ok(
                COMMAND,
                Some(server),
                ServerEndAllSessions { ended: sessions },
            ),
            text,
        ));
    }
    let offline_token::Issued { token, expires_at } =
        offline_token::issue(PURPOSE, &observation, now);
    let mut text = format!(
        "End {} Terminal Sessions on local? Terminal output and Scrollback will be cleared for every client.\nThe server and saved Workspace, Tab, and Pane organization remain.\n",
        sessions.len(),
    );
    for session in &sessions {
        text.push_str(&format!(
            "  id:{}  {}  Session id:{}\n",
            session.pane, session.label, session.session
        ));
    }
    text.push_str(&format!("Confirm with --confirm {token}\n"));
    Ok(Reply::new(
        &Envelope::confirmation(
            COMMAND,
            Some(server),
            Confirmation {
                token,
                expires_at,
                effects: EndAllSessionsEffects {
                    ends_sessions: sessions.len().try_into().unwrap_or(u32::MAX),
                    sessions,
                },
            },
        ),
        text,
    ))
}

#[cfg(test)]
mod tests;
