//! Opt-in accessibility composition. The server, transport, journal and Pane remain real.
//! Only the isolated account anchor and memory-only device key provider are substituted.

use super::*;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::{
    operations::OperationStatus,
    pairing::credentials::{
        CredentialCreationReservation, CredentialDeletion, CredentialScope, CredentialUse,
        PairingFuture, PreparedCredential,
    },
    replica::ReplicaState,
    view::{ReplicaFacts, ReplicaObservation, ReplicaReadiness, ServerViewStatus},
};
use spaceterm_core::{
    operations::{Command, CreatedEntities, Outcome, WorkspaceCreationSource},
    organization::{CreationTopologyReadModel, NameDraft},
};
use spaceterm_host::{
    account_service::test_support::AccountServiceFixture, startup::BuildIdentity,
};
use spaceterm_protocol::{ProtocolError, authentication::SigningKey};
use std::{ffi::OsString, path::PathBuf, sync::Mutex, time::Duration};

#[derive(Clone)]
pub(super) struct Fixture {
    root: PathBuf,
    pub(super) credentials: MemoryCredentials,
}
impl Fixture {
    pub(super) fn from_arguments(
        arguments: impl IntoIterator<Item = OsString>,
    ) -> Result<Option<Self>, crate::desktop_profile::DesktopProfileError> {
        let arguments: Vec<_> = arguments.into_iter().collect();
        if !arguments
            .iter()
            .any(|argument| argument == "--test-account-root")
        {
            return Ok(None);
        }
        if arguments.len() != 2 || arguments[0] != "--test-account-root" {
            return Err(crate::desktop_profile::DesktopProfileError::InvalidCombination);
        }
        let root = PathBuf::from(&arguments[1]);
        if !root.is_absolute() {
            return Err(crate::desktop_profile::DesktopProfileError::InvalidCombination);
        }
        // Opening the fixture below verifies owner-only registration and endpoint authority.
        Ok(Some(Self {
            root,
            credentials: MemoryCredentials::default(),
        }))
    }

    pub(super) fn native(&self) -> Result<NativeClientProvider, ClientFailure> {
        let fixture = AccountServiceFixture::new(&self.root, BuildIdentity::Development)
            .map_err(crate::platform::local_server_client::classify)?;
        let supervisor = fixture.reopen();
        if supervisor
            .discover()
            .map_err(crate::platform::local_server_client::classify)?
            .is_none()
        {
            return Err(fail(ClientFailureClass::Unavailable));
        }
        Ok(NativeClientProvider::from_supervisor(supervisor))
    }

    pub(super) async fn enroll(
        &self,
        native: &NativeClientProvider,
        journal: Arc<JournalController>,
    ) -> Result<(), ClientFailure> {
        crate::platform::local_server_client::enrollment::enroll_with_credentials(
            native,
            journal,
            self.credentials.clone(),
        )
        .await
    }
}

type CredentialInventory = Vec<(PreparedCredential, Arc<MemoryKey>)>;

#[derive(Clone, Default)]
pub(super) struct MemoryCredentials(Arc<Mutex<CredentialInventory>>);
struct MemoryKey(Ed25519KeyPair);
impl SigningKey for MemoryKey {
    fn public_key(&self) -> [u8; 32] {
        let mut public_key = [0; 32];
        public_key.copy_from_slice(self.0.public_key().as_ref());
        public_key
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        self.0
            .sign(bytes)
            .as_ref()
            .try_into()
            .map_err(|_| ProtocolError::Authentication)
    }
}
impl NativeDeviceCredentials for MemoryCredentials {
    fn create(
        &self,
        reservation: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        Box::pin(async move {
            let mut seed = zeroize::Zeroizing::new([0; 32]);
            getrandom::fill(seed.as_mut()).map_err(|_| PairingFailure::CredentialUnavailable)?;
            let key = Arc::new(MemoryKey(
                Ed25519KeyPair::from_seed_unchecked(seed.as_ref())
                    .map_err(|_| PairingFailure::CredentialUnavailable)?,
            ));
            let prepared = PreparedCredential {
                scope: reservation.scope().clone(),
                public_key: key.public_key(),
            };
            self.0
                .lock()
                .map_err(|_| PairingFailure::CredentialUnavailable)?
                .push((prepared.clone(), key));
            Ok(prepared)
        })
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            Ok(self
                .0
                .lock()
                .map_err(|_| PairingFailure::CredentialUnavailable)?
                .iter()
                .find(|(credential, _)| &credential.scope == scope)
                .map(|(credential, _)| credential.clone()))
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        authorization: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            authorization.validate(&credential.scope)?;
            let key = self
                .0
                .lock()
                .map_err(|_| PairingFailure::CredentialUnavailable)?
                .iter()
                .find(|(prepared, _)| prepared == credential)
                .map(|(_, key)| key.clone())
                .ok_or(PairingFailure::CredentialUnavailable)?;
            Ok(key as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async move {
            self.0
                .lock()
                .map_err(|_| PairingFailure::CredentialUnavailable)?
                .retain(|(prepared, _)| prepared != credential);
            Ok(CredentialDeletion::Deleted)
        })
    }
}

/// Creates through the ordinary authenticated operation ledger and attaches through the
/// normal personal view. Native Pane measurement and startup still run after installation.
pub(in crate::platform::desktop_client_startup) async fn initialize_workspace<
    S: ViewStore + 'static,
>(
    mut composition: DesktopClientComposition<S>,
) -> Result<DesktopClientComposition<S>, DesktopClientError> {
    let work = async {
        let server = composition
            .servers
            .first()
            .ok_or_else(|| fail(ClientFailureClass::Unavailable))?
            .server;
        let handle = composition
            .registry
            .server(server)
            .ok_or_else(|| fail(ClientFailureClass::Unavailable))?;
        let saved = composition.tracker.journal().snapshot()?;
        let binding = saved
            .bindings()
            .iter()
            .filter_map(|saved| saved.binding().ok())
            .find(|binding| binding.target().server() == server)
            .ok_or_else(|| fail(ClientFailureClass::AuthorizationDenied))?;
        handle.connect().await?;
        loop {
            if handle.replica().borrow().is_ready()
                && matches!(
                    *handle.state().borrow(),
                    spaceterm_client::api::ConnectionState::Connected { .. }
                )
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let ticket = composition
            .tracker
            .submit(
                &binding,
                Command::CreateWorkspace {
                    server,
                    name: NameDraft::Automatic,
                    source: WorkspaceCreationSource::ServerHome { server },
                },
                None,
                composition.operations.as_ref(),
            )
            .await?;
        // The normal protocol delegate durably resolves and acknowledges results. Follow
        // its ticket, rather than racing that acknowledgement with another server lookup.
        let mut status = ticket.status();
        let (outcome, revision) = loop {
            let observed = status.borrow_and_update().clone();
            match observed {
                OperationStatus::Completed {
                    outcome,
                    publication_revision,
                } => break (outcome, publication_revision),
                OperationStatus::Retired => return Err(fail(ClientFailureClass::InvalidPeer)),
                _ => status
                    .changed()
                    .await
                    .map_err(|_| fail(ClientFailureClass::Unavailable))?,
            }
        };
        let workspace = match outcome {
            Outcome::CreatedWithControl(created) => match created.entities() {
                CreatedEntities::Workspace { workspace, .. } => workspace,
                _ => return Err(fail(ClientFailureClass::InvalidPeer)),
            },
            Outcome::CreatedWorkspace { workspace, .. } => workspace,
            _ => return Err(fail(ClientFailureClass::InvalidPeer)),
        };
        loop {
            let replica = handle.replica().borrow().clone();
            let connection = handle.state().borrow().clone();
            if let (
                ReplicaState::Ready { snapshot },
                spaceterm_client::api::ConnectionState::Connected { info },
            ) = (replica, connection)
                && snapshot.cursor.revision >= revision
                && snapshot.cursor.revision == info.organization_revision
                && snapshot.cursor.epoch == info.epoch
            {
                let observation = ReplicaObservation::new(
                    &snapshot.organization,
                    snapshot.accepted_creations(),
                    ReplicaFacts {
                        pairing: binding.target().pairing(),
                        client: binding.target().client(),
                        connection: Some(info.binding),
                        readiness: ReplicaReadiness::Ready,
                        status: ServerViewStatus::Connected,
                    },
                )
                .map_err(|_| fail(ClientFailureClass::InvalidPeer))?;
                composition
                    .view
                    .reconcile(&observation)
                    .map_err(|_| fail(ClientFailureClass::InvalidPeer))?;
                let (lease, _) = spaceterm_client::view::launch::attach(
                    &mut composition.view,
                    &observation,
                    workspace,
                    &binding,
                    composition.tracker.journal(),
                    &composition.channels,
                )
                .await
                .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
                // Installation reconstructs its own lease from the retained attachment incarnation.
                drop(lease);
                let persisted = composition
                    .view
                    .persisted()
                    .map_err(|_| fail(ClientFailureClass::InvalidStorage))?;
                composition
                    .persistence
                    .save(&persisted)
                    .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
                return Ok::<_, ClientFailure>(composition);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), work)
        .await
        .map_err(|_| DesktopClientError::Offline)?
        .map_err(DesktopClientError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_fixture_requires_explicit_absolute_root_argument() {
        assert!(
            Fixture::from_arguments(Vec::<OsString>::new())
                .unwrap()
                .is_none()
        );
        assert!(Fixture::from_arguments(["--test-account-root".into()]).is_err());
        assert!(
            Fixture::from_arguments(["--test-account-root".into(), "relative".into()]).is_err()
        );
        assert!(
            Fixture::from_arguments(["--test-account-root".into(), "/var/tmp/test".into()])
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn desktop_accessibility_fixture_pairs_and_creates_attached_workspace_through_real_actor()
    {
        use crate::platform::{
            app_directories::{AppDirectories, AppDirectoryEnvironment},
            local_server_client::tests::Root,
            unix_secure_filesystem::UnixSecureFilesystem,
        };
        use spaceterm_server::{
            account_startup::{
                AccountStartup, RecoveredAccountServer,
                test_support::{StartupAbsence, initialize, supervisor},
            },
            adapters::terminal::AccountLaunchFacts,
        };
        let root = Root::new();
        initialize(&root.0).unwrap();
        let supervisor = supervisor(&root.0, Arc::new(StartupAbsence)).unwrap();
        let server = RecoveredAccountServer::recover(
            supervisor.clone(),
            AccountStartup::admit(&supervisor).unwrap(),
            AccountLaunchFacts {
                uid: supervisor.key().account_uid,
                home: root.0.clone(),
                planner: spaceterm_host::shell_launch::ShellLaunchPlanner::for_test(
                    root.0.join("shell"),
                    root.0.clone(),
                ),
                machine: spaceterm_terminal::metadata::LocalMachine::default(),
            },
        )
        .await
        .unwrap();
        let server_handle = server.handle();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(server.run_until(async {
            let _ = stopped.await;
        }));
        let directories = AppDirectories::resolve_xdg(
            "a11y-fixture-development",
            &AppDirectoryEnvironment {
                home: Some(root.0.clone().into_os_string()),
                xdg_state_home: Some(root.0.join("client-state").into_os_string()),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let paths = Arc::new(
            AppPaths::from_directories(directories, 104, Arc::new(UnixSecureFilesystem)).unwrap(),
        );
        let factory = Factory {
            paths: paths.clone(),
            enrollment: Arc::default(),
            ssh: crate::platform::server_client_route::SshStartup {
                executable: "/usr/bin/ssh".into(), home: root.0.clone(), environment: vec![],
            },
            askpass: crate::platform::server_client_route::askpass::DesktopTunnelAskPass::new(
                paths.clone(), Arc::new(crate::platform::unix_askpass_transport::AskPassWindowFactory::new(None)),
            ),
            context: tokio::sync::Mutex::new(None),
            fixture: Some(Fixture {
                root: root.0.clone(),
                credentials: MemoryCredentials::default(),
            }),
        };
        assert!(factory.foreground_setup().await.unwrap().is_none());
        let owners = factory.recover().await.unwrap();
        let result = super::super::super::recover(owners, NativeViewStore::new(paths)).await;
        let _ = stop.send(());
        let _ = running.await.unwrap();
        let composition = result.unwrap();
        assert_eq!(composition.view.workspaces().len(), 1);
        let snapshot = server_handle.snapshot();
        assert_eq!(snapshot.state().workspaces.len(), 1);
        assert_eq!(snapshot.state().panes.len(), 1);
        assert_eq!(
            composition.view.workspaces()[0].workspace(),
            snapshot.state().workspaces[0].id
        );
        composition.runtime.shutdown().await.unwrap();
    }
}
