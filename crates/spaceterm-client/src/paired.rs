use crate::admission::{CONNECTION_LIMIT, Caller};
use crate::connection::carrier::Carrier;
use crate::connection::link::{CoreLink, LinkSink};
use crate::connection::path::NetworkPath;
use crate::direct::{DirectConnection, DirectConnector, PairedDevice};
use crate::pane::control::ClientName;
use crate::protocol::AgentName;
use crate::service::lane::Lane;
use crate::work::WorkConnection;
use crate::{AuthenticatedStatus, CoreError, WaitLimit};
use std::sync::{Arc, Weak};
use std::time::Instant;

/// Supplies the device name used in admission. It grants no authority.
pub type DeviceName = Arc<dyn Fn() -> ClientName + Send + Sync>;

/// A saved pairing and its work and service-lane connections. Clones share their work link.
#[derive(Clone)]
pub struct PairedMachine {
    route: [u8; 32],
    paired: PairedDevice,
    connector: Arc<dyn DirectConnector>,
    name: DeviceName,
    agent: Option<AgentName>,
    path: NetworkPath,
    work: Arc<WorkConnection>,
}

impl PairedMachine {
    /// Binds the expected build and paired identity without opening a connection.
    pub fn new(
        build: &str,
        route: [u8; 32],
        paired: PairedDevice,
        connector: Arc<dyn DirectConnector>,
        name: DeviceName,
        agent: Option<AgentName>,
        path: NetworkPath,
    ) -> Result<Self, CoreError> {
        if route == [0; 32]
            || paired.pin.build != build
            || paired.pin.core == [0; 32]
            || paired.pin.public_key == [0; 32]
            || paired.pin.certificate == [0; 32]
            || paired.pairing == [0; 32]
            || paired.client == [0; 32]
        {
            return Err(CoreError::AuthenticationRefused);
        }
        Ok(Self {
            route,
            paired,
            connector,
            name,
            agent,
            path,
            work: Arc::default(),
        })
    }

    pub fn route(&self) -> &[u8; 32] {
        &self.route
    }

    pub fn paired(&self) -> &PairedDevice {
        &self.paired
    }

    pub fn agent(&self) -> Option<&AgentName> {
        self.agent.as_ref()
    }

    /// Whether these handles share one live work connection and admission state.
    pub fn same_handle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.work, &other.work)
    }

    pub fn work(&self) -> &Arc<WorkConnection> {
        &self.work
    }

    /// Admits a new, independent work connection over the exact pairing.
    pub fn admit_work(
        &self,
        cancelled: Option<&dyn Fn() -> bool>,
        deadline: Option<Instant>,
    ) -> Result<Box<dyn Carrier>, CoreError> {
        let check = || {
            if cancelled.is_some_and(|cancelled| cancelled()) {
                Err(CoreError::ServiceBusy)
            } else {
                Ok(())
            }
        };
        check()?;
        let connection = match deadline {
            Some(deadline) => self.connector.connect_by(deadline)?,
            None => self.connector.connect()?,
        };
        check()?;
        self.admit_connection(
            connection,
            &WaitLimit::new(deadline.unwrap_or_else(|| Instant::now() + CONNECTION_LIMIT)),
        )
    }

    fn admit_connection(
        &self,
        connection: DirectConnection,
        wait: &WaitLimit,
    ) -> Result<Box<dyn Carrier>, CoreError> {
        if wait.ended() {
            return Err(CoreError::ServiceBusy);
        }
        if connection.paired() != &self.paired {
            return Err(CoreError::AuthenticationRefused);
        }
        let name = (self.name)();
        let admitted = connection.with_path(Arc::clone(&self.path)).admit_with(
            Caller {
                client: &self.paired.client,
                name: &name,
                agent: self.agent(),
            },
            wait,
        );
        self.work.record_work(
            &admitted
                .as_ref()
                .map(|(_, version)| version.clone())
                .map_err(Clone::clone),
        );
        Ok(admitted?.0)
    }

    /// Gets the work link that receives inventory and sends Workspace and directory requests.
    pub fn link(&self) -> Result<CoreLink, CoreError> {
        self.work.link_with(
            |events| self.work.start(self.admit_work(None, None)?, events),
            None,
            None,
        )
    }

    /// Opens or reuses the work link under an absolute deadline.
    pub fn link_by(&self, deadline: Instant) -> Result<CoreLink, CoreError> {
        self.work.link_with(
            |events| {
                self.work
                    .start(self.admit_work(None, Some(deadline))?, events)
            },
            None,
            Some(deadline),
        )
    }

    /// Opens or reuses the single Work link while the caller is willing to wait. The limit
    /// applies to the dial and admission as well as waiting behind another caller's attempt.
    pub fn link_with_limit(&self, wait: &WaitLimit) -> Result<CoreLink, CoreError> {
        if wait.ended() {
            return Err(CoreError::ServiceBusy);
        }
        let link = self.work.link_with(
            |events| {
                let connection = self.connector.connect_with(wait)?;
                let carrier = self.admit_connection(connection, wait)?;
                if wait.ended() {
                    return Err(CoreError::ServiceBusy);
                }
                self.work.start(carrier, events)
            },
            Some(&|| wait.is_cancelled()),
            Some(wait.deadline()),
        );
        if wait.ended() {
            return Err(CoreError::ServiceBusy);
        }
        link
    }

    /// Gives this pairing's Workspace edits their own durable writer. The journal is bound to
    /// the exact Machine and effective Client Identity before it can dispatch a request.
    pub fn workspace_edits(
        &self,
        journal: Box<dyn crate::workspace::JournalStore>,
    ) -> Result<crate::workspace::WorkspaceEditor, CoreError> {
        crate::workspace::WorkspaceEditor::new(self.clone(), journal)
    }

    pub fn refresh(&self) -> Result<AuthenticatedStatus, CoreError> {
        self.link()?;
        let work = self
            .work
            .admissions()
            .capabilities(true)
            .work()
            .map_err(|_| CoreError::Protocol)?;
        Ok(AuthenticatedStatus { work })
    }

    pub fn disconnect(&self) {
        self.work.disconnect();
    }

    pub fn subscribe(&self, sink: Weak<dyn LinkSink>) {
        self.work.subscribe(sink);
    }

    /// Addresses the independent service lane, including when work admission is refused.
    pub fn service_lane(&self) -> Lane {
        Lane::paired(
            Arc::clone(&self.connector),
            self.paired.clone(),
            (self.name)(),
            self.agent.clone(),
            self.work.admitted_lanes(),
        )
    }
}
