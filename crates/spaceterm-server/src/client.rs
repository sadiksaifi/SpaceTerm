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
    shared_pane: std::sync::Arc<crate::pane::SharedPaneConnection>,
    client_cache: std::sync::Arc<ClientCache>,
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
        std::sync::Arc::ptr_eq(&self.shared_pane, &other.shared_pane)
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
            shared_pane: std::sync::Arc::default(),
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
    fn authenticate(&self) -> Result<AuthenticatedStatus, ServerError> {
        let mut stream = host::connect(&self.paths)?;
        let client = self.client_id()?;
        let server = self.server_id()?;
        let mut status = protocol::admit_client(
            &mut stream,
            (&client, &self.client_name()),
            &server,
            &self.identity,
            &[protocol::MAJOR],
        )?;
        // Status is used by desktop startup too. Apply the same required-capability
        // admission as remote routes before reporting this server ready.
        crate::pane::PaneConnection::negotiate(stream)?;
        self.bind_scope(self.scope_for(client, server))?;
        status.development_pane = true;
        Ok(status)
    }
    /// Admits a transport through its route's verified authority, without implicit Start.
    fn connect_admitted(
        &self,
    ) -> Result<(Box<dyn crate::transport::Transport>, ClientScope), ServerError> {
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
            return Ok((
                Box::new(connection),
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
        Ok((Box::new(stream), self.scope_for(client, server)))
    }
    pub fn connect_pane(&self) -> Result<crate::pane::PaneConnection, ServerError> {
        let (stream, scope) = self.connect_admitted()?;
        let connection = crate::pane::PaneConnection::negotiate(stream)?;
        self.bind_scope(scope)?;
        Ok(connection)
    }
    /// Clones share a transport. Independent desktops retain distinct identities and views.
    pub fn connect_shared_pane(&self) -> Result<crate::pane::PaneConnection, ServerError> {
        self.shared_pane.connect(|| self.connect_pane())
    }
    /// Checks an existing server. Never sets up storage or starts it.
    pub fn refresh(&self) -> Result<AuthenticatedStatus, ServerError> {
        if self.direct.is_some() {
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
        self.direct = Some(std::sync::Arc::new(DirectRoute { paired, connector }));
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
        let (mut connection, _) = self
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
    use std::sync::Arc;

    #[test]
    fn local_status_requires_workspace_capability_before_reporting_ready() {
        use crate::pane::Request;
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};

        for workspace_policy in [None, Some(false), Some(true)] {
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
            std::fs::set_permissions(paths.socket(), std::fs::Permissions::from_mode(0o600))
                .unwrap();
            let pin = server.server_id().unwrap();
            let peer = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                protocol::admit_server(&mut stream, &pin, &paths.identity).unwrap();
                let mut wire = protocol::Wire::terminal(&mut stream).unwrap();
                assert!(matches!(
                    wire.read::<Request>().unwrap(),
                    Request::Capabilities
                ));
                let mut reply = serde_json::json!({"Capabilities":{"development_pane":true}});
                if let Some(supported) = workspace_policy {
                    reply["Capabilities"]["workspace_policy"] = supported.into();
                }
                wire.write(&reply).unwrap();
                assert!(
                    wire.read::<Request>().is_err(),
                    "status must send no operation"
                );
            });
            let result = server.refresh();
            peer.join().unwrap();
            if workspace_policy == Some(true) {
                assert_eq!(
                    result,
                    Ok(AuthenticatedStatus {
                        protocol_major: 1,
                        development_pane: true,
                    }),
                    "missing optional capabilities must not prevent admission"
                );
            } else {
                assert_eq!(result, Err(ServerError::MissingRequiredCapability));
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn paired_direct_policy_changes_return_the_committed_status_before_disconnect() {
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
            // The same pairing has another transport, which must close immediately.
            let other_worker = fixture.serve_next();
            let mut other = DirectConnection::connect(&endpoint, &paired, &credential).unwrap();
            protocol::admit_transport(
                &mut other,
                (&paired.client, &"Other view".into()),
                &paired.pin.server,
                &paired.pin.build,
                &[protocol::MAJOR],
            )
            .unwrap();
            let policy = change_address.then(|| {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                direct::NetworkPolicy::new(listener.local_addr().unwrap()).unwrap()
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
            let issuer_worker = fixture.serve_next();
            let result = local.set_direct_policy(policy.clone());
            let _ = issuer_worker.join().unwrap();
            let ManagementReply::Status(committed) =
                fixture.management(Management::Status).unwrap()
            else {
                panic!()
            };
            assert_eq!(committed.policy, policy);
            assert_eq!(
                result,
                Ok(committed),
                "a successful durable policy change must reach its paired caller"
            );
            assert!(
                protocol::Wire::terminal(&mut other)
                    .unwrap()
                    .read_frame()
                    .is_err()
            );
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
                restored.policy, policy,
                "the reply must describe the policy retained across restart"
            );
            reopened.lock().unwrap().stop();
        }
    }

    struct NoConnect;
    impl crate::direct::DirectConnector for NoConnect {
        fn connect(&self) -> Result<crate::direct::DirectConnection, crate::direct::DirectError> {
            panic!("retained state must not contact WSS")
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
        let mut local = ServerConnection::for_development_client("first", 1).unwrap();
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
            shared_pane: std::sync::Arc::default(),
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
            shared_pane: std::sync::Arc::default(),
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
            shared_pane: std::sync::Arc::default(),
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
