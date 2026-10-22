use crate::{
    AuthenticatedStatus, ServerError,
    host::{self, Paths},
    protocol,
    storage::Directory,
};
use serde::{Deserialize, Serialize};

/// A stable presentation identity. Possession grants no connection authority.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientIdentity {
    format: String,
    client: [u8; 32],
}

/// Resolving this handle never reads or writes storage and never starts a process.
#[derive(Clone)]
pub struct LocalServer {
    paths: Paths,
    identity: String,
    account: String,
    /// A named Development client's name, which its Client Name carries after the computer's.
    development: Option<String>,
    computer: std::sync::Arc<dyn crate::computer_name::ComputerName>,
    remote: Option<std::sync::Arc<RemoteRoute>>,
    direct: Option<std::sync::Arc<DirectRoute>>,
    shared_pane: std::sync::Arc<crate::pane::SharedPaneConnection>,
    attachments:
        std::sync::Arc<std::sync::OnceLock<Result<crate::attachments::Attachments, ServerError>>>,
}
struct RemoteRoute {
    expected: crate::account_bridge::AccountBridgeIdentity,
    connector: std::sync::Arc<dyn crate::account_bridge::AccountConnector>,
}
struct DirectRoute {
    paired: crate::direct::PairedDevice,
    connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectRecord {
    version: u8,
    paired: crate::direct::PairedDevice,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteRecord {
    version: u32,
    expected: crate::account_bridge::AccountBridgeIdentity,
}

impl LocalServer {
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
        if let Some(remote) = &self.remote {
            let _lock = directory.lock("route.lock", false)?;
            match directory.read_json::<RemoteRecord>("route.json")? {
                Some(record) if record.version == 1 && record.expected == remote.expected => {}
                Some(_) => return Err(ServerError::AuthenticationRefused),
                None => directory.write_json(
                    c"route.json",
                    &RemoteRecord {
                        version: 1,
                        expected: remote.expected.clone(),
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
        match (self.computer.computer_name(), &self.development) {
            (Some(computer), Some(development)) => format!("{computer} ({development})").into(),
            (Some(computer), None) => computer.into(),
            (None, Some(development)) => development.as_str().into(),
            (None, None) => "".into(),
        }
    }
    fn server_id(&self) -> Result<[u8; 32], ServerError> {
        if let Some(direct) = &self.direct {
            self.client_directory()?;
            return Ok(direct.paired.pin.server);
        }
        if let Some(remote) = &self.remote {
            self.client_directory()?;
            return Ok(remote.expected.server);
        }
        Ok(Directory::open(&self.paths.data, false)?.read()?.server)
    }
    /// One durable attachment writer shared by the inventory and all Pane subscriptions.
    /// Whether `other` is a clone of this handle rather than a separately created one, such as
    /// the handle a later connection to the same account creates.
    pub fn same_handle(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.shared_pane, &other.shared_pane)
    }

    pub fn attachments(&self) -> Result<crate::attachments::Attachments, ServerError> {
        if let Some(attachments) = self.attachments.get() {
            return attachments.clone();
        }
        let server = self.server_id()?;
        self.attachments
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
            remote: None,
            direct: None,
            shared_pane: std::sync::Arc::default(),
            attachments: std::sync::Arc::default(),
        })
    }
    /// Retain one remote account route with the local client's existing journals, views and
    /// attachments. Construction performs no discovery, process creation or storage writes.
    /// `route` is the saved connection's stable nonsecret ID, never a current alias lookup.
    pub fn remote(
        identity: &str,
        route: [u8; 32],
        expected: crate::account_bridge::AccountBridgeIdentity,
        connector: std::sync::Arc<dyn crate::account_bridge::AccountConnector>,
    ) -> Result<Self, ServerError> {
        Self::for_identity(identity)?.with_remote(route, expected, connector)
    }
    fn with_remote(
        mut self,
        route: [u8; 32],
        expected: crate::account_bridge::AccountBridgeIdentity,
        connector: std::sync::Arc<dyn crate::account_bridge::AccountConnector>,
    ) -> Result<Self, ServerError> {
        if expected.build != self.identity
            || expected.server == [0; 32]
            || expected.account == 0
            || route == [0; 32]
        {
            return Err(ServerError::AuthenticationRefused);
        }
        self.account = format!(
            "{}-ssh-{}",
            self.account,
            route.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        self.remote = Some(std::sync::Arc::new(RemoteRoute {
            expected,
            connector,
        }));
        Ok(self)
    }
    /// Whether this route has owning-machine local file authority.
    pub fn is_account_local(&self) -> bool {
        self.remote.is_none() && self.direct.is_none()
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
    fn authenticate(&self) -> Result<AuthenticatedStatus, ServerError> {
        let mut stream = host::connect(&self.paths)?;
        let mut status = protocol::admit_client(
            &mut stream,
            (&self.client_id()?, &self.client_name()),
            &self.server_id()?,
            &self.identity,
            &[protocol::MAJOR],
        )?;
        let mut connection = crate::pane::PaneConnection::new(stream, false);
        status.development_pane = matches!(
            connection.exchange(&crate::pane::Request::Capabilities),
            Ok(crate::pane::Reply::Capabilities {
                development_pane: true,
                ..
            })
        );
        Ok(status)
    }
    /// Admits a transport through its route's verified authority, without implicit Start.
    fn connect_admitted(&self) -> Result<Box<dyn crate::transport::Transport>, ServerError> {
        if let Some(direct) = &self.direct {
            let mut connection = direct.connector.connect().map_err(ServerError::from)?;
            if connection.paired() != &direct.paired {
                return Err(ServerError::AuthenticationRefused);
            }
            protocol::admit_transport(
                &mut connection,
                (&direct.paired.client, &self.client_name()),
                &direct.paired.pin.server,
                &self.identity,
                &[protocol::MAJOR],
            )?;
            return Ok(Box::new(connection));
        }
        if let Some(remote) = &self.remote {
            let client = self.client_id()?;
            let mut bridge = remote.connector.connect()?;
            if bridge.identity() != &remote.expected {
                return Err(ServerError::AuthenticationRefused);
            }
            protocol::admit_transport(
                &mut bridge,
                (&client, &self.client_name()),
                &remote.expected.server,
                &self.identity,
                &[protocol::MAJOR],
            )?;
            return Ok(Box::new(bridge));
        }
        crate::service::check_admission(&self.paths)?;
        let mut stream = host::connect(&self.paths)?;
        protocol::admit_client(
            &mut stream,
            (&self.client_id()?, &self.client_name()),
            &self.server_id()?,
            &self.identity,
            &[protocol::MAJOR],
        )?;
        Ok(Box::new(stream))
    }
    pub fn connect_pane(&self) -> Result<crate::pane::PaneConnection, ServerError> {
        crate::pane::PaneConnection::negotiate(self.connect_admitted()?)
    }
    /// Clones share a transport. Independent desktops retain distinct identities and views.
    pub fn connect_shared_pane(&self) -> Result<crate::pane::PaneConnection, ServerError> {
        self.shared_pane.connect(|| self.connect_pane())
    }
    /// Checks an existing server. Never sets up storage or starts it.
    pub fn refresh(&self) -> Result<AuthenticatedStatus, ServerError> {
        if self.remote.is_some() || self.direct.is_some() {
            self.connect_pane()?;
            return Ok(AuthenticatedStatus {
                protocol_major: protocol::MAJOR,
                development_pane: true,
            });
        }
        crate::service::check_admission(&self.paths)?;
        self.server_id()?;
        self.authenticate()
    }
    /// Explicit first setup retains a nonsecret server identity. It never changes Stop intent.
    pub fn setup(&self) -> Result<(), ServerError> {
        if self.remote.is_some() || self.direct.is_some() {
            return Err(ServerError::Unsupported);
        }
        let data = Directory::open(&self.paths.data, true)?;
        let _setup = data.lock("setup.lock", false)?;
        data.read_or_create()?;
        self.client_id()?;
        Ok(())
    }
}

impl LocalServer {
    /// Retains only caller-supplied nonsecret fences. No storage, network or credential access.
    pub fn direct(
        identity: &str,
        route: [u8; 32],
        paired: crate::direct::PairedDevice,
        connector: std::sync::Arc<dyn crate::direct::DirectConnector>,
    ) -> Result<Self, ServerError> {
        if route == [0; 32]
            || paired.pin.build != identity
            || paired.pin.server == [0; 32]
            || paired.pin.public_key == [0; 32]
            || paired.pin.certificate == [0; 32]
            || paired.pairing == [0; 32]
            || paired.client == [0; 32]
        {
            return Err(ServerError::AuthenticationRefused);
        }
        let mut server = Self::for_identity(identity)?;
        server.account = format!(
            "{}-direct-{}",
            server.account,
            route.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        server.direct = Some(std::sync::Arc::new(DirectRoute { paired, connector }));
        Ok(server)
    }
    fn direct_manage(
        &self,
        request: crate::direct::Management,
    ) -> Result<crate::direct::ManagementReply, crate::direct::DirectError> {
        let mut connection = self
            .connect_admitted()
            .map_err(crate::direct::DirectError::from)?;
        let mut wire = protocol::Wire::terminal(connection.as_mut())?;
        wire.write(&crate::pane::Request::Direct(request))?;
        match wire.read()? {
            crate::pane::Reply::Direct(reply) => reply,
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn direct_status(&self) -> Result<crate::direct::DirectStatus, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::Status)? {
            crate::direct::ManagementReply::Status(status) => Ok(status),
            _ => Err(crate::direct::DirectError::InvalidRequest),
        }
    }
    pub fn set_direct_policy(
        &self,
        policy: Option<crate::direct::NetworkPolicy>,
    ) -> Result<crate::direct::DirectStatus, crate::direct::DirectError> {
        match self.direct_manage(crate::direct::Management::SetPolicy(policy))? {
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
    use crate::account_bridge::{AccountBridge, AccountBridgeIdentity, AccountConnector};
    use std::sync::Arc;

    struct Studio;
    impl crate::computer_name::ComputerName for Studio {
        fn computer_name(&self) -> Option<String> {
            Some("Studio\u{202E}".into())
        }
    }
    struct NoConnect;
    impl AccountConnector for NoConnect {
        fn connect(&self) -> Result<AccountBridge, ServerError> {
            panic!("retained state must not contact SSH")
        }
    }
    fn remote_fixture(
        root: &std::path::Path,
        name: &str,
        expected: AccountBridgeIdentity,
        connector: Arc<dyn AccountConnector>,
    ) -> LocalServer {
        let mut local = LocalServer::for_development_client(name, 1).unwrap();
        local.computer = Arc::new(Studio);
        local.paths = Paths {
            identity: "spaceterm-development".into(),
            data: root.join("data"),
            runtime: root.join("runtime"),
        };
        local.with_remote([3; 32], expected, connector).unwrap()
    }
    fn remote_identity() -> AccountBridgeIdentity {
        AccountBridgeIdentity {
            account: 501,
            build: "spaceterm-development".into(),
            server: [7; 32],
        }
    }

    #[test]
    fn named_clients_keep_independent_remote_identities_for_the_same_route() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-remote-{:x}",
            u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let first = remote_fixture(&root, "first", remote_identity(), Arc::new(NoConnect));
        let second = remote_fixture(&root, "second", remote_identity(), Arc::new(NoConnect));
        assert!(!root.exists());
        assert!(!first.is_account_local());
        assert_ne!(first.client_id().unwrap(), second.client_id().unwrap());
        drop(first);
        drop(second);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_relaunch_keeps_originals_attachments_and_views_and_refuses_rebinding() {
        use crate::{
            client_view::{ClientView, Layout, LayoutNode, TabView, WorkspaceView},
            hierarchy::Mutation,
        };
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-original-{:x}",
            u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let first = remote_fixture(&root, "first", remote_identity(), Arc::new(NoConnect));
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
        let relaunched = remote_fixture(&root, "first", remote_identity(), Arc::new(NoConnect));
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
        let mut changed = remote_identity();
        changed.account += 1;
        let redirected = remote_fixture(&root, "first", changed, Arc::new(NoConnect));
        assert!(matches!(
            redirected.shared_journal(),
            Err(ServerError::AuthenticationRefused)
        ));
        drop(redirected);
        let restored = remote_fixture(&root, "first", remote_identity(), Arc::new(NoConnect));
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
    fn injected_account_bridge_reconnects_with_the_same_client_without_replaying_an_exchange() {
        use crate::pane::{Reply, Request};
        use std::{
            os::unix::net::UnixStream,
            sync::{
                Mutex,
                atomic::{AtomicUsize, Ordering},
            },
        };
        /// A Client Identity and the Client Name it was admitted with.
        type Admitted = ([u8; 32], String);
        struct Connector {
            opens: AtomicUsize,
            clients: Arc<Mutex<Vec<Admitted>>>,
            cleaned: Arc<AtomicUsize>,
        }
        impl AccountConnector for Connector {
            fn connect(&self) -> Result<AccountBridge, ServerError> {
                let attempt = self.opens.fetch_add(1, Ordering::SeqCst);
                let (client, mut server) = UnixStream::pair().unwrap();
                let clients = Arc::clone(&self.clients);
                std::thread::spawn(move || {
                    protocol::Wire::new(&mut server)
                        .unwrap()
                        .write(&remote_identity())
                        .unwrap();
                    let (_, client, name) =
                        protocol::admit_server(&mut server, &[7; 32], "spaceterm-development")
                            .unwrap();
                    clients.lock().unwrap().push((client, name.as_str().into()));
                    let mut wire = protocol::Wire::terminal(&mut server).unwrap();
                    assert!(matches!(
                        wire.read::<Request>().unwrap(),
                        Request::Capabilities
                    ));
                    wire.write(&serde_json::json!({"Capabilities":{"development_pane":true,"workspace_policy":true}})).unwrap();
                    assert!(matches!(
                        wire.read::<Request>().unwrap(),
                        Request::Inventory
                    ));
                    if attempt != 0 {
                        wire.write(&Reply::Inventory {
                            inventory: crate::hierarchy::Inventory {
                                revision: 17,
                                workspaces: Vec::new(),
                                close: None,
                                repositories: None,
                            },
                            next_sequence: 1,
                        })
                        .unwrap();
                    }
                });
                let cleaned = Arc::clone(&self.cleaned);
                AccountBridge::from_streams(
                    Box::new(client.try_clone().unwrap()),
                    Box::new(client),
                    Box::new(move || {
                        cleaned.fetch_add(1, Ordering::SeqCst);
                    }),
                    &remote_identity(),
                )
            }
        }
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-transport-{:x}",
            u64::from_be_bytes(protocol::random().unwrap()[..8].try_into().unwrap())
        ));
        let connector = Arc::new(Connector {
            opens: AtomicUsize::new(0),
            clients: Arc::new(Mutex::new(Vec::new())),
            cleaned: Arc::new(AtomicUsize::new(0)),
        });
        let server = remote_fixture(&root, "first", remote_identity(), connector.clone());
        let mut first = server.connect_shared_pane().unwrap();
        let mut subscriber = server.connect_shared_pane().unwrap();
        assert!(first.exchange(&Request::Inventory).is_err());
        assert_eq!(connector.opens.load(Ordering::SeqCst), 1);
        assert!(matches!(
            subscriber.exchange(&Request::Inventory),
            Err(ServerError::NotRunning)
        ));
        assert_eq!(connector.cleaned.load(Ordering::SeqCst), 1);
        let mut next = server.connect_shared_pane().unwrap();
        assert!(
            matches!(next.exchange(&Request::Inventory).unwrap(),Reply::Inventory {inventory,..} if inventory.revision==17)
        );
        let clients = connector.clients.lock().unwrap();
        assert_eq!(clients.len(), 2);
        assert_eq!(clients[0], clients[1]);
        assert_eq!(
            clients[0].1, "Studio (first)",
            "a Development client is named by its computer and its name"
        );
        drop(clients);
        drop(first);
        drop(subscriber);
        drop(next);
        drop(server);
        assert_eq!(connector.cleaned.load(Ordering::SeqCst), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn attachments_recover_after_initial_setup_on_the_same_client() {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "st-first-setup-{}",
            u128::from_be_bytes(protocol::random().unwrap()[..16].try_into().unwrap())
        ));
        let server = LocalServer {
            paths: Paths {
                identity: "spaceterm-development".into(),
                data: root.join("data"),
                runtime: root.join("runtime"),
            },
            identity: "spaceterm-development".into(),
            account: "desktop".into(),
            development: None,
            computer: std::sync::Arc::new(crate::computer_name::SystemComputerName),
            remote: None,
            direct: None,
            shared_pane: std::sync::Arc::default(),
            attachments: std::sync::Arc::default(),
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
        let server = LocalServer {
            paths: Paths {
                identity: "spaceterm-development".into(),
                data: root.join("data"),
                runtime: root.join("runtime"),
            },
            identity: "spaceterm-development".into(),
            account: "desktop".into(),
            development: None,
            computer: std::sync::Arc::new(crate::computer_name::SystemComputerName),
            remote: None,
            direct: None,
            shared_pane: std::sync::Arc::default(),
            attachments: std::sync::Arc::default(),
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
        let make = |name: &str| LocalServer {
            paths: Paths {
                identity: "spaceterm-development".into(),
                data: root.join("data"),
                runtime: root.join("runtime"),
            },
            identity: "spaceterm-development".into(),
            account: name.into(),
            development: None,
            computer: std::sync::Arc::new(crate::computer_name::SystemComputerName),
            remote: None,
            direct: None,
            shared_pane: std::sync::Arc::default(),
            attachments: std::sync::Arc::default(),
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
