//! Connects the service-lane client to this account's Core.
use crate::{CoreError, protocol::capability::Admissions};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use spaceterm_client::service::lane::*;

/// The service lane of this account's own server, over account IPC.
struct Local {
    paths: crate::host::Paths,
    core: [u8; 32],
    client: [u8; 32],
    name: crate::pane::control::ClientName,
    agent: Option<crate::protocol::AgentName>,
    admissions: Arc<Mutex<Admissions>>,
}

impl Connector for Local {
    fn connect(&self, deadline: Instant) -> Result<Connected, CoreError> {
        if Instant::now() >= deadline {
            return Err(CoreError::NotRunning);
        }
        let mut stream = crate::host::connect(&self.paths)?;
        let admitted = crate::protocol::admit_service_lane(
            &mut crate::protocol::LocalPreamble(&mut stream),
            crate::protocol::Caller {
                client: &self.client,
                name: &self.name,
                agent: self.agent.as_ref(),
            },
            &self.core,
            &self.paths.identity,
            deadline,
        );
        record(&self.admissions, &admitted);
        let admitted = admitted?;
        Ok((
            Box::new(crate::protocol::LocalPreamble(stream)),
            admitted.selected,
            admitted.core.service,
        ))
    }
}

/// The lane of the account server at `paths`, as base identity `client` named `name`, or as
/// `agent` when given.
pub(crate) fn local(
    paths: crate::host::Paths,
    core: [u8; 32],
    client: [u8; 32],
    name: crate::pane::control::ClientName,
    agent: Option<crate::protocol::AgentName>,
    admissions: Arc<Mutex<Admissions>>,
) -> Lane {
    Lane::over(Arc::new(Local {
        paths,
        core,
        client,
        name,
        agent,
        admissions,
    }))
}
