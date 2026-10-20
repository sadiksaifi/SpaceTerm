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
    remote: Option<std::sync::Arc<RemoteRoute>>,
    shared_pane: std::sync::Arc<crate::pane::SharedPaneConnection>,
    attachments:
        std::sync::Arc<std::sync::OnceLock<Result<crate::attachments::Attachments, ServerError>>>,
}
struct RemoteRoute {
    expected: crate::account_bridge::AccountBridgeIdentity,
    connector: std::sync::Arc<dyn crate::account_bridge::AccountConnector>,
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
    fn server_id(&self) -> Result<[u8; 32], ServerError> {
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
        let account = match std::env::var_os("SPACETERM_DEVELOPMENT_CLIENT") {
            Some(name) => development_account(name.to_str().ok_or(ServerError::Unsupported)?)?,
            None => "desktop".to_owned(),
        };
        Ok(Self {
            identity: paths.identity.clone(),
            paths,
            account,
            remote: None,
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
        self.remote.is_none()
    }
    /// An independent named desktop with a retained nonsecret identity.
    pub fn for_development_client(name: &str, protocol_major: u16) -> Result<Self, ServerError> {
        if protocol_major != protocol::MAJOR {
            return Err(ServerError::IncompatibleProtocol);
        }
        let mut server = Self::for_identity("spaceterm-development")?;
        server.account = development_account(name)?;
        Ok(server)
    }
    fn authenticate(&self) -> Result<AuthenticatedStatus, ServerError> {
        let mut stream = host::connect(&self.paths)?;
        let mut status = protocol::admit_client(
            &mut stream,
            &self.client_id()?,
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
    /// Admits a terminal transport using owning-account authority, without implicit Start.
    pub fn connect_pane(&self) -> Result<crate::pane::PaneConnection, ServerError> {
        if let Some(remote) = &self.remote {
            let client = self.client_id()?;
            let mut bridge = remote.connector.connect()?;
            if bridge.identity() != &remote.expected {
                return Err(ServerError::AuthenticationRefused);
            }
            protocol::admit_transport(
                &mut bridge,
                &client,
                &remote.expected.server,
                &self.identity,
                &[protocol::MAJOR],
            )?;
            return crate::pane::PaneConnection::negotiate(bridge);
        }
        crate::service::check_admission(&self.paths)?;
        let mut stream = host::connect(&self.paths)?;
        protocol::admit_client(
            &mut stream,
            &self.client_id()?,
            &self.server_id()?,
            &self.identity,
            &[protocol::MAJOR],
        )?;
        crate::pane::PaneConnection::negotiate(stream)
    }
    /// Clones share a transport. Independent desktops retain distinct identities and views.
    pub fn connect_shared_pane(&self) -> Result<crate::pane::PaneConnection, ServerError> {
        self.shared_pane.connect(|| self.connect_pane())
    }
    /// Checks an existing server. Never sets up storage or starts it.
    pub fn refresh(&self) -> Result<AuthenticatedStatus, ServerError> {
        if self.remote.is_some() {
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
        if self.remote.is_some() {
            return Err(ServerError::Unsupported);
        }
        let data = Directory::open(&self.paths.data, true)?;
        let _setup = data.lock("setup.lock", false)?;
        data.read_or_create()?;
        self.client_id()?;
        Ok(())
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
        struct Connector {
            opens: AtomicUsize,
            clients: Arc<Mutex<Vec<[u8; 32]>>>,
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
                    let (_, client) =
                        protocol::admit_server(&mut server, &[7; 32], "spaceterm-development")
                            .unwrap();
                    clients.lock().unwrap().push(client);
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
            remote: None,
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
            remote: None,
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
            remote: None,
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
