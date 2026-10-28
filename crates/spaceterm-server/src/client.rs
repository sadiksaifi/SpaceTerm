use crate::connection::link::{LinkEvent, LinkSink, ServerLink};
use crate::{
    AuthenticatedStatus, ServerError,
    host::{self, Paths},
    protocol,
    storage::Directory,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, Weak};

/// A stable presentation identity. Possession grants no connection authority.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientIdentity {
    format: String,
    client: [u8; 32],
}

/// Opaque process-local identity for retaining client state across transport replacement.
/// It grants no connection authority and deliberately has no diagnostic or retained encoding.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ClientScope {
    application: String,
    directory: std::path::PathBuf,
    account: String,
    server: [u8; 32],
    client: [u8; 32],
    authority: ClientAuthority,
}

#[derive(Clone, Eq, PartialEq, Hash)]
enum ClientAuthority {
    Local,
    Direct {
        pairing: [u8; 32],
        public_key: [u8; 32],
        certificate: [u8; 32],
    },
}

/// Clones retain the same client identity binding and its attachment writer.
#[derive(Default)]
struct ClientCache {
    attachments: std::sync::OnceLock<Result<crate::attachments::Attachments, ServerError>>,
    scope: std::sync::OnceLock<ClientScope>,
}

/// Resolving this handle never reads or writes storage and never starts a process.
#[derive(Clone)]
pub struct ServerConnection {
    paths: Paths,
    identity: String,
    account: String,
    /// A named Development client's name, which its Client Name carries after the computer's.
    development: Option<String>,
    computer: std::sync::Arc<dyn crate::computer_name::ComputerName>,
    direct: Option<std::sync::Arc<DirectRoute>>,
    shared: std::sync::Arc<SharedLink>,
    client_cache: std::sync::Arc<ClientCache>,
}

/// The one live link that a handle and its clones share with their server, and the sinks that
/// receive its pushed inventory and health.
#[derive(Default)]
struct SharedLink {
    link: Mutex<Option<ServerLink>>,
    subscribers: Mutex<Vec<Weak<dyn LinkSink>>>,
    /// How many links were connected. Each new link may reach a restarted server.
    connected: std::sync::atomic::AtomicU64,
}

/// Delivers a shared link's events to every live subscriber. It holds the shared state weakly, so
/// the link it belongs to never keeps that state alive.
struct Fanout(Weak<SharedLink>);
impl LinkSink for Fanout {
    fn deliver(&self, event: LinkEvent) {
        let Some(shared) = self.0.upgrade() else {
            return;
        };
        let subscribers: Vec<_> = {
            let mut subscribers = shared.subscribers.lock().unwrap_or_else(|e| e.into_inner());
            subscribers.retain(|sink| sink.strong_count() > 0);
            subscribers.iter().filter_map(Weak::upgrade).collect()
        };
        match event {
            LinkEvent::Inventory(revision) => {
                for sink in subscribers {
                    sink.deliver(LinkEvent::Inventory(Arc::clone(&revision)));
                }
            }
            LinkEvent::Health(health) => {
                for sink in subscribers {
                    sink.deliver(LinkEvent::Health(health));
                }
            }
        }
    }
}
struct DirectRoute {
    id: [u8; 32],
    paired: crate::direct::PairedDevice,
    connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectRecord {
    version: u8,
    paired: crate::direct::PairedDevice,
}

impl ServerConnection {
    /// Reads previously bound identity fences without storage, network or credential access.
    /// Local handles bind after successful protocol/capability admission. Direct
    /// handles bind their explicit retained pairing at construction. Clones share the binding;
    /// a fresh handle must establish its own. Live admission still checks every transport.
    pub fn client_scope(&self) -> Result<ClientScope, ServerError> {
        self.client_cache
            .scope
            .get()
            .cloned()
            .ok_or(ServerError::AuthenticationRefused)
    }

    fn scope_for(&self, client: [u8; 32], server: [u8; 32]) -> ClientScope {
        ClientScope {
            application: self.identity.clone(),
            directory: self.paths.data.clone(),
            account: self.account.clone(),
            server,
            client,
            authority: if let Some(direct) = &self.direct {
                ClientAuthority::Direct {
                    pairing: direct.paired.pairing,
                    public_key: direct.paired.pin.public_key,
                    certificate: direct.paired.pin.certificate,
                }
            } else {
                ClientAuthority::Local
            },
        }
    }

    fn bind_scope(&self, scope: ClientScope) -> Result<(), ServerError> {
        let bound = self.client_cache.scope.get_or_init(|| scope.clone());
        if *bound != scope {
            // A handle never silently rebinds retained presentation state to another identity.
            return Err(ServerError::AuthenticationRefused);
        }
        Ok(())
    }

    fn client_directory(&self) -> Result<Directory, ServerError> {
        let directory =
            Directory::open(&self.paths.data.join("clients").join(&self.account), true)?;
        if let Some(direct) = &self.direct {
            let _lock = directory.lock("route.lock", false)?;
            match directory.read_json::<DirectRecord>("route.json")? {
                Some(record) if record.version == 1 && record.paired == direct.paired => {}
                Some(_) => return Err(ServerError::AuthenticationRefused),
                None => directory.write_json(
                    c"route.json",
                    &DirectRecord {
                        version: 1,
                        paired: direct.paired.clone(),
                    },
                )?,
            }
        }
        Ok(directory)
    }
    fn client_id(&self) -> Result<[u8; 32], ServerError> {
        if let Some(direct) = &self.direct {
            self.client_directory()?;
            return Ok(direct.paired.client);
        }
        let directory = self.client_directory()?;
        let _lock = directory.lock("identity.lock", false)?;
        if let Some(record) = directory.read_json::<ClientIdentity>("client.json")? {
            if record.format != "account-local-1" || record.client == [0; 32] {
                return Err(ServerError::UnsafeStorage);
            }
            return Ok(record.client);
        }
        let client = protocol::random()?;
        directory.write_json(
            c"client.json",
            &ClientIdentity {
                format: "account-local-1".into(),
                client,
            },
        )?;
        Ok(client)
    }
    /// What other clients call this one while it controls a Pane.
    fn client_name(&self) -> crate::pane::control::ClientName {
        crate::pane::control::ClientName::for_computer(
            self.computer.as_ref(),
            self.development.as_deref(),
        )
    }
    fn server_id(&self) -> Result<[u8; 32], ServerError> {
        if let Some(direct) = &self.direct {
            self.client_directory()?;
            return Ok(direct.paired.pin.server);
        }
        Ok(Directory::open(&self.paths.data, false)?.read()?.server)
    }
    /// One durable attachment writer shared by the inventory and all Pane subscriptions.
    /// Whether `other` is a clone of this handle rather than a separately created one, such as
    /// the handle a later connection to the same account creates.
    pub fn same_handle(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.shared, &other.shared)
    }

    pub fn attachments(&self) -> Result<crate::attachments::Attachments, ServerError> {
        if let Some(attachments) = self.client_cache.attachments.get() {
            return attachments.clone();
        }
        let server = self.server_id()?;
        self.client_cache
            .attachments
            .get_or_init(|| {
                crate::attachments::Attachments::open(
                    self.client_directory()?,
                    server,
                    self.client_id()?,
                )
            })
            .clone()
    }
    /// One journal writer for this stable client and original server identity.
    pub fn shared_journal(&self) -> Result<crate::hierarchy::ClientJournal, ServerError> {
        crate::hierarchy::ClientJournal::open(
            self.client_directory()?,
            self.server_id()?,
            self.client_id()?,
        )
    }
    /// Opens the private arrangement without contacting or starting the server.
    pub fn client_view_store(&self) -> Result<crate::client_view::ClientViewStore, ServerError> {
        crate::client_view::ClientViewStore::open(
            self.client_directory()?,
            self.server_id()?,
            self.client_id()?,
        )
    }
    /// Opens this desktop's continuous Workspace order without requiring a running server.
    /// Call on the account-local handle; remote routes do not own desktop-wide state.
    pub fn workspace_order_store(
        &self,
    ) -> Result<crate::workspace_order::WorkspaceOrderStore, ServerError> {
        if !self.is_account_local() {
            return Err(ServerError::AuthenticationRefused);
        }
        crate::workspace_order::WorkspaceOrderStore::open(
            self.client_directory()?,
            self.client_id()?,
        )
    }

    /// Fingerprints this bound presentation without storage, network or credential access.
    /// Paths, environment values and credentials never contribute to its retained identity.
    pub fn workspace_owner(&self) -> Result<crate::workspace_order::OwnerId, ServerError> {
        let scope = self
            .client_cache
            .scope
            .get()
            .ok_or(ServerError::AuthenticationRefused)?;
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        digest.update(b"spaceterm-workspace-presentation-owner-1\0");
        digest.update(&(scope.application.len() as u64).to_be_bytes());
        digest.update(scope.application.as_bytes());
        digest.update(&scope.server);
        digest.update(&scope.client);
        match &scope.authority {
            ClientAuthority::Local => digest.update(b"account-local\0"),
            ClientAuthority::Direct {
                pairing,
                public_key,
                certificate,
            } => {
                let direct = self
                    .direct
                    .as_ref()
                    .ok_or(ServerError::AuthenticationRefused)?;
                digest.update(b"paired-direct\0");
                digest.update(public_key);
                digest.update(certificate);
                digest.update(pairing);
                digest.update(&direct.id);
            }
        }
        let mut owner = [0; 32];
        owner.copy_from_slice(digest.finish().as_ref());
        Ok(crate::workspace_order::OwnerId::from_bytes(owner))
    }

    /// Names a Workspace on this admitted server without storage or network access.
    pub fn workspace_reference(
        &self,
        workspace: crate::hierarchy::Id,
    ) -> Result<crate::workspace_order::WorkspaceReference, ServerError> {
        Ok(crate::workspace_order::WorkspaceReference {
            owner: self.workspace_owner()?,
            workspace,
        })
    }

    pub fn for_identity(directory_name: &str) -> Result<Self, ServerError> {
        Self::with_paths(Paths::for_identity(directory_name)?)
    }
    pub(crate) fn with_paths(paths: Paths) -> Result<Self, ServerError> {
        let development = std::env::var_os("SPACETERM_DEVELOPMENT_CLIENT")
            .map(|name| name.into_string().map_err(|_| ServerError::Unsupported))
            .transpose()?;
        let account = match &development {
            Some(name) => development_account(name)?,
            None => "desktop".to_owned(),
        };
        Ok(Self {
            identity: paths.identity.clone(),
            paths,
            account,
            development,
            computer: std::sync::Arc::new(crate::computer_name::SystemComputerName),
            direct: None,
            shared: std::sync::Arc::default(),
            client_cache: std::sync::Arc::default(),
        })
    }
    /// Whether this route has owning-machine local file authority.
    pub fn is_account_local(&self) -> bool {
        self.direct.is_none()
    }
    /// An independent named desktop with a retained nonsecret identity.
    pub fn for_development_client(name: &str, protocol_major: u16) -> Result<Self, ServerError> {
        if protocol_major != protocol::MAJOR {
            return Err(ServerError::IncompatibleProtocol);
        }
        let mut server = Self::for_identity("spaceterm-development")?;
        server.account = development_account(name)?;
        server.development = Some(name.to_owned());
        Ok(server)
    }
    /// Admits a new connection through its route's verified authority, without implicit Start,
    /// and returns its carrier.
    fn connect_admitted(
        &self,
    ) -> Result<(Box<dyn crate::connection::carrier::Carrier>, ClientScope), ServerError> {
        if let Some(direct) = &self.direct {
            let connection = direct.connector.connect().map_err(ServerError::from)?;
            if connection.paired() != &direct.paired {
                return Err(ServerError::AuthenticationRefused);
            }
            // `with_direct` fixed the pin's build to this handle's identity.
            let carrier = connection.admit((&direct.paired.client, &self.client_name()))?;
            return Ok((
                carrier,
                self.scope_for(direct.paired.client, direct.paired.pin.server),
            ));
        }
        crate::service::check_admission(&self.paths)?;
        let mut stream = host::connect(&self.paths)?;
        let client = self.client_id()?;
        let server = self.server_id()?;
        protocol::admit_client(
            &mut stream,
            (&client, &self.client_name()),
            &server,
            &self.identity,
            &[protocol::MAJOR],
        )?;
        let carrier =
            crate::connection::unix::UnixCarrier::new(stream).map_err(|_| ServerError::Protocol)?;
        Ok((Box::new(carrier), self.scope_for(client, server)))
    }
    /// Opens a new connection of its own, separate from the shared one. `events` receives its
    /// pushed inventory and health. Dropping every handle of the link closes it.
    pub fn connect(&self, events: Arc<dyn LinkSink>) -> Result<ServerLink, ServerError> {
        let (carrier, scope) = self.connect_admitted()?;
        self.bind_scope(scope)?;
        crate::connection::io::spawn_link(carrier, events).map_err(|_| ServerError::Protocol)
    }
    /// The one live connection this handle and its clones share with the server. Connects when
    /// none is live. A link that ended is never reused; its pending requests already failed.
    pub fn link(&self) -> Result<ServerLink, ServerError> {
        // Callers wait for one connection attempt, so simultaneous Pane workers never open a
        // connection each.
        let mut saved = self.shared.link.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(link) = saved.as_ref()
            && link.health() == crate::connection::link::Health::Live
        {
            return Ok(link.clone());
        }
        *saved = None;
        let link = self.connect(Arc::new(Fanout(Arc::downgrade(&self.shared))))?;
        *saved = Some(link.clone());
        self.shared
            .connected
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(link)
    }
    /// Makes `link`, already admitted, this handle's shared connection while it stays live. Tests
    /// use it to serve a handle in process, without a socket.
    #[cfg(feature = "test-support")]
    pub fn adopt(&self, link: ServerLink) {
        *self.shared.link.lock().unwrap_or_else(|e| e.into_inner()) = Some(link);
    }
    /// Identifies the shared link: it changes each time [`Self::link`] connects a new one. Facts
    /// read over an older link may describe a server that has since restarted.
    pub fn link_generation(&self) -> u64 {
        self.shared
            .connected
            .load(std::sync::atomic::Ordering::Acquire)
    }
    /// Delivers the shared link's pushed inventory and health to `sink` while it lives, across
    /// reconnects.
    pub fn subscribe(&self, sink: Weak<dyn LinkSink>) {
        self.shared
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(sink);
    }
    /// Checks an existing server through the shared connection, connecting only when none is
    /// live. Never sets up storage or starts it.
    pub fn refresh(&self) -> Result<AuthenticatedStatus, ServerError> {
        if self.direct.is_none() {
            crate::service::check_admission(&self.paths)?;
            self.server_id()?;
        }
        self.link()?;
        Ok(AuthenticatedStatus {
            protocol_major: protocol::MAJOR,
            development_pane: true,
        })
    }
    /// Explicit first setup retains a nonsecret server identity. It never changes Stop intent.
    pub fn setup(&self) -> Result<(), ServerError> {
        if self.direct.is_some() {
            return Err(ServerError::Unsupported);
        }
        let data = Directory::open(&self.paths.data, true)?;
        let _setup = data.lock("setup.lock", false)?;
        data.read_or_create()?;
        self.client_id()?;
        Ok(())
    }
}

impl ServerConnection {
    /// Retains only caller-supplied nonsecret fences. No storage, network or credential access.
    pub fn direct(
        identity: &str,
        route: [u8; 32],
        paired: crate::direct::PairedDevice,
        connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
    ) -> Result<Self, ServerError> {
        Self::for_identity(identity)?.with_direct(route, paired, connector)
    }
    fn with_direct(
        mut self,
        route: [u8; 32],
        paired: crate::direct::PairedDevice,
        connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
    ) -> Result<Self, ServerError> {
        if route == [0; 32]
            || paired.pin.build != self.identity
            || paired.pin.server == [0; 32]
            || paired.pin.public_key == [0; 32]
            || paired.pin.certificate == [0; 32]
            || paired.pairing == [0; 32]
            || paired.client == [0; 32]
        {
            return Err(ServerError::AuthenticationRefused);
        }
        self.account = format!(
            "{}-direct-{}",
            self.account,
            route.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        self.direct = Some(std::sync::Arc::new(DirectRoute {
            id: route,
            paired,
            connector,
        }));
        self.client_cache = std::sync::Arc::default();
        let direct = self
            .direct
            .as_ref()
            .ok_or(ServerError::AuthenticationRefused)?;
        self.bind_scope(self.scope_for(direct.paired.client, direct.paired.pin.server))?;
        Ok(self)
    }
    fn direct_manage(
        &self,
        request: crate::direct::Management,
    ) -> Result<crate::direct::ManagementReply, crate::direct::DirectError> {
        let reply = self
            .link()
            .and_then(|link| {
                link.request(crate::protocol::Operation::Direct(request))
                    .map_err(ServerError::from)
            })
            .map_err(crate::direct::DirectError::from)?
            .wait()
            .map_err(|error| crate::direct::DirectError::from(ServerError::from(error)))?;
        match reply {
            crate::protocol::ReplyBody::Direct(reply) => reply,
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn direct_status(&self) -> Result<crate::direct::DirectStatus, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Status)? {
            crate::direct::ManagementReply::Status(status) => Ok(status),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    /// Select a bind policy and its exposure atomically. None retains the configured address.
    pub fn set_direct_policy(
        &self,
        policy: Option<crate::direct::NetworkPolicy>,
        enabled: bool,
    ) -> Result<crate::direct::DirectStatus, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::SetPolicy { policy, enabled })? {
            crate::direct::ManagementReply::Status(status) => Ok(status),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn invite_device(&self) -> Result<crate::direct::Invitation, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Invite)? {
            crate::direct::ManagementReply::Invitation(text) => {
                crate::direct::Invitation::parse(&text)
            }
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn withdraw_invitation(
        &self,
        id: crate::direct::ClaimId,
    ) -> Result<(), crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Withdraw(id))? {
            crate::direct::ManagementReply::Rejected => Ok(()),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn pairing_claims(
        &self,
    ) -> Result<Vec<crate::direct::PairingClaim>, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Claims)? {
            crate::direct::ManagementReply::Claims(claims) => Ok(claims),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn approve_device(
        &self,
        claim: &crate::direct::PairingClaim,
    ) -> Result<crate::direct::DeviceInfo, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Approve(claim.clone()))? {
            crate::direct::ManagementReply::Approved(device) => Ok(device),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn reject_device(
        &self,
        claim: crate::direct::ClaimId,
    ) -> Result<(), crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Reject(claim))? {
            crate::direct::ManagementReply::Rejected => Ok(()),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn paired_devices(
        &self,
    ) -> Result<Vec<crate::direct::DeviceInfo>, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Devices)? {
            crate::direct::ManagementReply::Devices(devices) => Ok(devices),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn revoke_device(
        &self,
        operation: crate::direct::OperationId,
        target: crate::direct::PairingId,
    ) -> Result<crate::direct::RevocationReceipt, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Revoke { operation, target })? {
            crate::direct::ManagementReply::Revoked(receipt) => Ok(receipt),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
}

fn development_account(name: &str) -> Result<String, ServerError> {
    if name.is_empty()
        || name.len() > 32
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(ServerError::Unsupported);
    }
    Ok(format!("desktop-{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn local_status_admits_major_two_and_binds_the_scope_without_an_operation() {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};

        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-cap-{:x}",
            u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let paths = Paths {
            identity: "spaceterm-development".into(),
            data: root.join("d"),
            runtime: root.join("r"),
        };
        let server = ServerConnection::with_paths(paths.clone()).unwrap();
        server.setup().unwrap();
        Directory::open(&paths.runtime, true).unwrap();
        let listener = UnixListener::bind(paths.socket()).unwrap();
        std::fs::set_permissions(paths.socket(), std::fs::Permissions::from_mode(0o600)).unwrap();
        let pin = server.server_id().unwrap();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            protocol::admit_server(&mut stream, &pin, &paths.identity).unwrap();
            stream.set_nonblocking(false).unwrap();
            let mut received = Vec::new();
            std::io::Read::read_to_end(&mut stream, &mut received).unwrap();
            received
        });
        assert_eq!(
            server.workspace_owner().err(),
            Some(ServerError::AuthenticationRefused)
        );
        assert_eq!(
            server.refresh(),
            Ok(AuthenticatedStatus {
                protocol_major: 2,
                development_pane: true,
            })
        );
        assert_eq!(
            server.workspace_owner().unwrap(),
            server.clone().workspace_owner().unwrap()
        );
        drop(server);
        assert!(peer.join().unwrap().is_empty(), "status sends no operation");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clones_share_one_live_link_that_fails_together_and_reconnects_after_it_ends() {
        use crate::connection::link::Health;
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-link-{:x}",
            u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let paths = Paths {
            identity: "spaceterm-development".into(),
            data: root.join("d"),
            runtime: root.join("r"),
        };
        let server = ServerConnection::with_paths(paths.clone()).unwrap();
        server.setup().unwrap();
        Directory::open(&paths.runtime, true).unwrap();
        let listener = UnixListener::bind(paths.socket()).unwrap();
        std::fs::set_permissions(paths.socket(), std::fs::Permissions::from_mode(0o600)).unwrap();
        let pin = server.server_id().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let (sockets, served) = std::sync::mpsc::channel();
        {
            let accepted = Arc::clone(&accepted);
            let identity = paths.identity.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    accepted.fetch_add(1, Ordering::SeqCst);
                    sockets.send(stream.try_clone().unwrap()).unwrap();
                    let identity = identity.clone();
                    std::thread::spawn(move || {
                        let (_, client, name) =
                            protocol::admit_server(&mut stream, &pin, &identity).unwrap();
                        let (sync, _) = crate::connection::sync::SyncWorker::spawn().unwrap();
                        let environment = host::Environment::for_tests(
                            crate::direct::tests::service(crate::direct::tests::Refusing),
                            sync,
                            None,
                        );
                        let (waker, wake) = crate::connection::io::Waker::new().unwrap();
                        host::serve_admitted(
                            Box::new(crate::connection::unix::UnixCarrier::new(stream).unwrap()),
                            crate::connection::service::Peer {
                                client,
                                connection: protocol::random().unwrap(),
                                pairing: None,
                            },
                            name,
                            crate::connection::outbox::Outbox::new(wake),
                            waker,
                            &environment,
                        );
                    });
                }
            });
        }
        let answers = |link: &ServerLink| {
            matches!(
                link.request(crate::protocol::Operation::Shutdown)
                    .unwrap()
                    .wait(),
                Err(crate::connection::link::RequestError::Refused(
                    crate::pane::Refusal::Unsupported
                ))
            )
        };
        let first = server.link().unwrap();
        let second = server.clone().link().unwrap();
        assert!(answers(&first) && answers(&second));
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            1,
            "clones share one connection"
        );
        assert_eq!(server.link_generation(), 1);
        // The server's end of that connection closes: every holder sees the same failure.
        served
            .recv()
            .unwrap()
            .shutdown(std::net::Shutdown::Both)
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while first.health() == Health::Live || second.health() == Health::Live {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(first.health(), second.health());
        let third = server.link().unwrap();
        assert!(answers(&third));
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            2,
            "an ended link reconnects once"
        );
        assert_eq!(
            server.clone().link_generation(),
            2,
            "a new link has a new identity"
        );
        assert!(answers(&server.clone().link().unwrap()));
        assert_eq!(accepted.load(Ordering::SeqCst), 2);
        drop((first, second, third, server));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn paired_direct_policy_changes_are_refused_without_changing_policy_or_other_connections() {
        use crate::direct::{self, DirectConnection, DirectConnector, Management, ManagementReply};
        struct Connector {
            endpoint: String,
            paired: direct::PairedDevice,
            credential: direct::DeviceCredential,
        }
        impl DirectConnector for Connector {
            fn connect(&self) -> Result<DirectConnection, direct::DirectError> {
                DirectConnection::connect(&self.endpoint, &self.paired, &self.credential)
            }
        }
        for change_address in [false, true] {
            let fixture = direct::tests::Fixture::new();
            fixture.enable();
            let credential = direct::DeviceCredential::generate().unwrap();
            let paired = fixture.enroll(&credential);
            let endpoint = fixture.endpoint();
            // Refusing administration must preserve the same pairing's other transport.
            let other_worker = fixture.serve_next();
            let connection = DirectConnection::connect(&endpoint, &paired, &credential).unwrap();
            let other =
                direct::tests::admitted(connection, &paired.client, "Other view", &paired).unwrap();
            let policy = change_address.then(|| {
                direct::NetworkPolicy::new(direct::tests::free_udp(
                    std::net::Ipv4Addr::LOCALHOST.into(),
                ))
                .unwrap()
            });
            let local = ServerConnection::with_paths(fixture.paths.clone())
                .unwrap()
                .with_direct(
                    [42; 32],
                    paired.clone(),
                    Arc::new(Connector {
                        endpoint,
                        paired,
                        credential,
                    }),
                )
                .unwrap();
            let ManagementReply::Status(initial) = fixture.management(Management::Status).unwrap()
            else {
                panic!()
            };
            let issuer_worker = fixture.serve_next();
            let result = local.set_direct_policy(policy.clone(), policy.is_some());
            // The request used the handle's shared connection, which ends with the handle.
            drop(local);
            let _ = issuer_worker.join().unwrap();
            let ManagementReply::Status(observed) = fixture.management(Management::Status).unwrap()
            else {
                panic!()
            };
            assert_eq!(result, Err(direct::DirectError::AuthenticationRefused));
            assert_eq!(observed.enabled, initial.enabled);
            assert_eq!(observed.policy, initial.policy);
            assert!(matches!(direct::tests::manage(&other, Management::Status),
                Ok(ManagementReply::Status(status))
                if status.enabled == initial.enabled && status.policy == initial.policy));
            drop(other);
            let _ = other_worker.join().unwrap();
            assert!(
                matches!(fixture.management(Management::Devices), Ok(ManagementReply::Devices(devices)) if devices.len() == 1)
            );
            fixture.state.lock().unwrap().stop();
            let reopened = direct::host::State::open(&fixture.paths).unwrap();
            let (ManagementReply::Status(restored), _) = reopened
                .lock()
                .unwrap()
                .manage(None, Management::Status)
                .unwrap()
            else {
                panic!()
            };
            assert_eq!(
                restored.policy, initial.policy,
                "refused remote administration must not change retained listener policy"
            );
            assert_eq!(restored.enabled, initial.enabled);
            reopened.lock().unwrap().stop();
        }
    }

    struct NoConnect;
    impl crate::direct::DirectConnector for NoConnect {
        fn connect(&self) -> Result<crate::direct::DirectConnection, crate::direct::DirectError> {
            panic!("retained state must not contact QUIC")
        }
    }
    fn paired_identity() -> crate::direct::PairedDevice {
        crate::direct::PairedDevice {
            pin: crate::direct::ServerPin {
                build: "spaceterm-development".into(),
                server: [7; 32],
                public_key: [8; 32],
                certificate: [9; 32],
            },
            pairing: [10; 32],
            client: [11; 32],
        }
    }
    fn direct_fixture(
        root: &std::path::Path,
        paired: crate::direct::PairedDevice,
    ) -> ServerConnection {
        let mut local = ServerConnection::for_development_client("first", 2).unwrap();
        local.paths = Paths {
            identity: "spaceterm-development".into(),
            data: root.join("data"),
            runtime: root.join("runtime"),
        };
        local
            .with_direct([3; 32], paired, Arc::new(NoConnect))
            .unwrap()
    }
    #[test]
    fn workspace_owners_distinguish_full_pins_clients_pairings_and_saved_routes() {
        let paired = paired_identity();
        let reference = direct_fixture(std::path::Path::new("/unused"), paired.clone())
            .workspace_reference([42; 32])
            .unwrap();
        for (field, changed) in [
            ("server", {
                let mut changed = paired.clone();
                changed.pin.server[0] ^= 1;
                changed
            }),
            ("public key", {
                let mut changed = paired.clone();
                changed.pin.public_key[0] ^= 1;
                changed
            }),
            ("certificate", {
                let mut changed = paired.clone();
                changed.pin.certificate[0] ^= 1;
                changed
            }),
            ("client", {
                let mut changed = paired.clone();
                changed.client[0] ^= 1;
                changed
            }),
            ("pairing", {
                let mut changed = paired.clone();
                changed.pairing[0] ^= 1;
                changed
            }),
        ] {
            let other = direct_fixture(std::path::Path::new("/unused"), changed);
            assert_ne!(
                other.workspace_reference(reference.workspace).unwrap(),
                reference,
                "changing {field} must not reuse another presentation's order"
            );
        }
        let other_route = ServerConnection::direct(
            "spaceterm-development",
            [4; 32],
            paired,
            Arc::new(NoConnect),
        )
        .unwrap();
        assert_ne!(
            other_route
                .workspace_reference(reference.workspace)
                .unwrap(),
            reference,
            "two saved routes retain independent presentations even with identical pairing fences"
        );
    }

    #[test]
    fn local_and_direct_workspace_owners_stay_distinct_when_server_and_client_ids_collide() {
        let paired = paired_identity();
        let local = ServerConnection::for_development_client("first", 2).unwrap();
        // Supply the admitted identity at the transport boundary without starting a server.
        local
            .bind_scope(local.scope_for(paired.client, paired.pin.server))
            .unwrap();
        let remote = direct_fixture(std::path::Path::new("/unused"), paired);
        assert_ne!(
            local.workspace_reference([42; 32]).unwrap(),
            remote.workspace_reference([42; 32]).unwrap()
        );
    }

    #[test]
    fn workspace_owners_ignore_storage_paths_and_development_client_environment_names() {
        let paired = paired_identity();
        let original = direct_fixture(std::path::Path::new("/first"), paired.clone());
        let mut relaunched = ServerConnection::for_development_client("second", 2).unwrap();
        relaunched.paths.data = std::path::PathBuf::from("/second");
        let relaunched = relaunched
            .with_direct([3; 32], paired, Arc::new(NoConnect))
            .unwrap();
        assert_eq!(
            [
                original.clone().workspace_owner().unwrap(),
                relaunched.workspace_owner().unwrap()
            ],
            [original.workspace_owner().unwrap(); 2]
        );
    }

    #[test]
    fn workspace_owners_distinguish_application_identity() {
        let original = direct_fixture(std::path::Path::new("/unused"), paired_identity());
        let mut paired = paired_identity();
        paired.pin.build = "spaceterm-preflight".into();
        let other =
            ServerConnection::direct("spaceterm-preflight", [3; 32], paired, Arc::new(NoConnect))
                .unwrap();
        assert_ne!(
            original.workspace_owner().unwrap(),
            other.workspace_owner().unwrap()
        );
    }

    #[test]
    fn desktop_workspace_order_is_available_offline_and_isolated_from_other_clients_and_routes() {
        use crate::workspace_order::{OwnerId, WorkspaceOrder, WorkspaceReference};
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-desktop-order-{:x}",
            u128::from_be_bytes(protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let local = |name| {
            let mut connection = ServerConnection::for_development_client(name, 2).unwrap();
            connection.paths = Paths {
                identity: "spaceterm-development".into(),
                data: root.join("data"),
                runtime: root.join("runtime"),
            };
            connection
        };
        let a = WorkspaceReference {
            owner: OwnerId::from_bytes([1; 32]),
            workspace: [3; 32],
        };
        let b = WorkspaceReference {
            owner: OwnerId::from_bytes([2; 32]),
            workspace: [3; 32],
        };
        let mut order = WorkspaceOrder::default();
        order.record(&[b, a]).unwrap();
        {
            let connection = local("first");
            assert_eq!(
                connection.client_scope().err(),
                Some(ServerError::AuthenticationRefused)
            );
            connection
                .workspace_order_store()
                .unwrap()
                .save(order)
                .unwrap();
        }
        assert_eq!(
            local("first")
                .workspace_order_store()
                .unwrap()
                .order()
                .restore(&[a, b]),
            [b, a]
        );
        assert_eq!(
            local("second").workspace_order_store().unwrap().order(),
            &WorkspaceOrder::default()
        );
        let remote = direct_fixture(&root, paired_identity());
        assert_eq!(
            remote.workspace_reference([3; 32]).unwrap().owner,
            remote.workspace_owner().unwrap()
        );
        assert_eq!(
            remote.workspace_order_store().err(),
            Some(ServerError::AuthenticationRefused)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn paired_relaunch_keeps_originals_attachments_and_views_and_refuses_rebinding() {
        use crate::{
            client_view::{ClientView, Layout, LayoutNode, TabView, WorkspaceView},
            hierarchy::Mutation,
        };
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-original-{:x}",
            u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let first = direct_fixture(&root, paired_identity());
        let reference = first.workspace_reference([9; 32]).unwrap();
        let client = first.client_id().unwrap();
        let mutation = Mutation::CreateWorkspace {
            name: "Remote original".into(),
            directory: None,
        };
        first
            .shared_journal()
            .unwrap()
            .submit(3, mutation.clone())
            .unwrap();
        let view = ClientView {
            workspaces: vec![WorkspaceView {
                workspace: [9; 32],
                tabs: vec![TabView {
                    tab: [10; 32],
                    layout: Layout {
                        nodes: vec![LayoutNode::Pane([11; 32])],
                    },
                }],
            }],
        };
        first
            .client_view_store()
            .unwrap()
            .save(view.clone())
            .unwrap();
        let inventory = crate::hierarchy::Inventory {
            revision: 0,
            workspaces: Vec::new(),
            close: None,
            repositories: None,
        };
        first
            .attachments()
            .unwrap()
            .initialize(&inventory, None)
            .unwrap();
        drop(first);
        let relaunched = direct_fixture(&root, paired_identity());
        assert_eq!(relaunched.workspace_reference([9; 32]).unwrap(), reference);
        assert_eq!(relaunched.client_id().unwrap(), client);
        assert_eq!(
            relaunched
                .shared_journal()
                .unwrap()
                .pending()
                .unwrap()
                .mutation,
            mutation
        );
        assert_eq!(relaunched.client_view_store().unwrap().view(), &view);
        assert!(
            !relaunched
                .attachments()
                .unwrap()
                .initialize(&inventory, None)
                .unwrap()
        );
        drop(relaunched);
        let mut changed = paired_identity();
        changed.client[0] ^= 1;
        let redirected = direct_fixture(&root, changed);
        assert!(matches!(
            redirected.shared_journal(),
            Err(ServerError::AuthenticationRefused)
        ));
        drop(redirected);
        let restored = direct_fixture(&root, paired_identity());
        assert_eq!(
            restored
                .shared_journal()
                .unwrap()
                .pending()
                .unwrap()
                .mutation,
            mutation
        );
        drop(restored);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn attachments_recover_after_initial_setup_on_the_same_client() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-first-setup-{}",
            u128::from_be_bytes(protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let server = ServerConnection {
            paths: Paths {
                identity: "spaceterm-development".into(),
                data: root.join("data"),
                runtime: root.join("runtime"),
            },
            identity: "spaceterm-development".into(),
            account: "desktop".into(),
            development: None,
            computer: std::sync::Arc::new(crate::computer_name::SystemComputerName),
            direct: None,
            shared: std::sync::Arc::default(),
            client_cache: std::sync::Arc::default(),
        };
        let subscription = server.clone();
        assert!(matches!(
            subscription.attachments(),
            Err(ServerError::NotSetUp)
        ));
        server.setup().unwrap();
        let opened = server.attachments();
        assert!(
            opened.is_ok(),
            "initial setup must recover without relaunch"
        );
        drop(opened);
        drop(subscription);
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn setup_only_recreates_state_after_reset_while_retaining_durable_stop() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-stopped-{}",
            u128::from_be_bytes(protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let server = ServerConnection {
            paths: Paths {
                identity: "spaceterm-development".into(),
                data: root.join("data"),
                runtime: root.join("runtime"),
            },
            identity: "spaceterm-development".into(),
            account: "desktop".into(),
            development: None,
            computer: std::sync::Arc::new(crate::computer_name::SystemComputerName),
            direct: None,
            shared: std::sync::Arc::default(),
            client_cache: std::sync::Arc::default(),
        };
        let data = Directory::open(&server.paths.data, true).unwrap();
        let stopped = serde_json::json!({"installation": "a".repeat(64), "stopped": true, "installed": true, "run": null});
        data.write_json(c"service.json", &stopped).unwrap();
        assert_eq!(server.refresh(), Err(ServerError::Stopped));
        server.setup().unwrap();
        assert_ne!(data.read().unwrap().server, [0; 32]);
        assert_eq!(
            data.read_json::<serde_json::Value>("service.json").unwrap(),
            Some(stopped)
        );
        assert_eq!(server.refresh(), Err(ServerError::Stopped));
        assert!(!server.paths.runtime.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn relaunch_keeps_each_nonsecret_identity_and_pending_original_bound_to_its_server() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-client-{}",
            u128::from_be_bytes(protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let make = |name: &str| ServerConnection {
            paths: Paths {
                identity: "spaceterm-development".into(),
                data: root.join("data"),
                runtime: root.join("runtime"),
            },
            identity: "spaceterm-development".into(),
            account: name.into(),
            development: None,
            computer: std::sync::Arc::new(crate::computer_name::SystemComputerName),
            direct: None,
            shared: std::sync::Arc::default(),
            client_cache: std::sync::Arc::default(),
        };
        let first = make("first");
        let second = make("second");
        assert!(!root.exists(), "construction cannot provision state");
        first.setup().unwrap();
        second.setup().unwrap();
        let id = first.client_id().unwrap();
        assert_ne!(id, second.client_id().unwrap());
        let mutation = crate::hierarchy::Mutation::CreateWorkspace {
            name: "Original".into(),
            directory: None,
        };
        {
            let mut journal = first.shared_journal().unwrap();
            journal.submit(3, mutation.clone()).unwrap();
        }
        drop(first);
        let relaunched = make("first");
        assert_eq!(relaunched.client_id().unwrap(), id);
        let journal = relaunched.shared_journal().unwrap();
        assert_eq!(journal.pending().unwrap().mutation, mutation);
        assert!(second.shared_journal().unwrap().pending().is_none());
        drop(journal);
        let directory = Directory::open(&root.join("data"), false).unwrap();
        let mut record = directory.read().unwrap();
        record.server = protocol::random().unwrap();
        directory.write(&record).unwrap();
        assert!(matches!(
            relaunched.shared_journal(),
            Err(ServerError::AuthenticationRefused)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
