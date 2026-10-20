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
    shared_pane: std::sync::Arc<crate::pane::SharedPaneConnection>,
    attachments:
        std::sync::Arc<std::sync::OnceLock<Result<crate::attachments::Attachments, ServerError>>>,
}
impl LocalServer {
    fn client_directory(&self) -> Result<Directory, ServerError> {
        Directory::open(&self.paths.data.join("clients").join(&self.account), true)
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
        Ok(Directory::open(&self.paths.data, false)?.read()?.server)
    }
    /// One durable attachment writer shared by the inventory and all Pane subscriptions.
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
            shared_pane: std::sync::Arc::default(),
            attachments: std::sync::Arc::default(),
        })
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
        crate::service::check_admission(&self.paths)?;
        self.server_id()?;
        self.authenticate()
    }
    /// Explicit first setup retains a nonsecret server identity. It never changes Stop intent.
    pub fn setup(&self) -> Result<(), ServerError> {
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
