//! Exercise the recovered desktop installer, Settings requests and client quit.
//! Only storage, native credential effects and transport endpoints are faked.

use super::super::model::{
    BoundServerRequest, InstalledServers, ServerRequest, Servers, ServersSnapshot,
};
use crate::{
    platform::desktop_client_startup::{self, AcceptanceProtocol, NativeDesktopOwners},
    ui::{
        server_client_runtime::{
            DesktopClientRuntime, composed::install_with_enrollment, testing::wire,
        },
        settings_window::{
            server_enrollment::{
                EnrollmentAttempt, EnrollmentFailure,
                native::{
                    DirectInvitationPreparation, DirectPreparation, NativeEnrollmentServices,
                },
            },
            server_runtime::SettingsServices,
        },
    },
};
use gpui::{Entity, TestAppContext};
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding},
    failure::{ClientFailure, ClientFailureClass},
    journal::{
        ClientJournalChange, JournalController, JournalCredentialSlot, JournalLimits,
        ReceiptVerifier, SavedBinding,
    },
    operations::OperationTracker,
    pairing::{
        credentials::*,
        enrollment::{EnrollmentConnector, PairingTransport},
        journal::RevocationPhase,
        revocation::RestrictedConnector,
    },
    pane_channel::PaneChannels,
    ports::{ConnectionPurpose, CredentialSource, SigningAccess},
    projection::ProjectionBudgets,
    testing::MemoryJournal,
    view::{
        AttachmentIncarnation, AttachmentSuppression, EncodedView, StoredView, ViewCommit,
        ViewStore, ViewStoreFailure, WorkspaceAttachment,
    },
};
use spaceterm_client_native::pairing_runtime::NativePairingRuntime;
use spaceterm_core::{
    authority::StoreRevision, identity::*, operation_ledger::canonical::CanonicalCodec,
    operations::*, pairing::*,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        self, Established, HandshakeConfig, PairingLookup, Principal, ServerHandshake, ServerPin,
        SigningKey,
    },
    connection::RestrictedConnection,
    pairing_adapter::{
        PairingRestrictedCodec, ReceiptCodec, operation_to_wire, receipt_to_wire, target_to_wire,
    },
    wire::pairing,
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

#[derive(Default)]
struct MemoryView {
    version: u64,
    bytes: Option<Vec<u8>>,
}
impl ViewStore for MemoryView {
    type Version = u64;
    fn load(&mut self, _: usize) -> Result<Option<StoredView<u64>>, ViewStoreFailure> {
        Ok(self.bytes.clone().map(|bytes| StoredView {
            version: self.version,
            bytes,
        }))
    }
    fn replace(&mut self, expected: Option<&u64>, document: &EncodedView) -> ViewCommit<u64> {
        assert_eq!(expected.copied(), self.bytes.as_ref().map(|_| self.version));
        self.version += 1;
        self.bytes = Some(document.as_bytes().to_vec());
        ViewCommit::Durable(self.version)
    }
}

struct Verifier;
impl ReceiptVerifier for Verifier {
    fn verify(&self, pin: ServerPin, receipt: &SignedPairingReceipt) -> Result<(), ClientFailure> {
        ReceiptCodec::new(pin)
            .verify(receipt)
            .map(|_| ())
            .map_err(|_| ClientFailure::new(ClientFailureClass::InvalidPeer))
    }
}

struct Pairing {
    binding: PairedServerBinding,
    credential: PreparedCredential,
    saved: SavedBinding,
    route: RevocationRouteReference,
}
impl Pairing {
    fn new(generation: u8) -> Self {
        let original = wire::binding(1);
        let target = original.target();
        Self::at_native_route(
            generation,
            target.pin(),
            target.device(),
            target.route().clone(),
            &wire::TestKey::new(1),
        )
    }
    fn at_native_route(
        generation: u8,
        pin: ServerPin,
        device: DeviceId,
        route: spaceterm_client::api::RouteDescription,
        signer: &dyn SigningKey,
    ) -> Self {
        let server = ServerId::new(pin.identity);
        let pairing = PairingId::new(server, u64::from(generation)).unwrap();
        let client = ClientId::new(server, u64::from(generation)).unwrap();
        let binding = PairedServerBinding::new(
            ConnectionTarget::new(
                pin,
                device,
                pairing,
                client,
                route,
                CredentialReference::new([generation; 16]),
                LocalPairingGeneration::new(u64::from(generation)).unwrap(),
            )
            .unwrap(),
        );
        let stream = |number| {
            OperationStreamId::new(
                OperationStreamKey::new(server, number).unwrap(),
                pairing,
                client,
            )
            .unwrap()
        };
        let streams = PairingClientStreams::new(PairingClientStreamsParts {
            client: ClientRecord {
                id: client,
                pairing,
            },
            ordinary: stream(u64::from(generation) * 3),
            self_revocation: stream(u64::from(generation) * 3 + 1),
        })
        .unwrap();
        let credential = PreparedCredential {
            scope: CredentialScope::new(CredentialScopeParts {
                namespace: CredentialNamespace::new("dev.spaceterm.desktop-scheduler-test".into())
                    .unwrap(),
                purpose: CredentialPurpose::PairingAuthentication { server },
                reference: binding.target().credential(),
            })
            .unwrap(),
            public_key: wire::TestKey::new(99).public_key(),
        };
        let proof = ReceiptCodec::new(pin)
            .sign(
                PairingReceiptBody::ClientBinding {
                    target: PairingTarget::new(PairingTargetParts {
                        server,
                        pairing,
                        device,
                    })
                    .unwrap(),
                    keys: PairingKeys::new(PairingKeysParts {
                        device_identity_public_key: wire::TestKey::new(100).public_key(),
                        pairing_authentication_public_key: credential.public_key,
                    })
                    .unwrap(),
                    client: streams.clone(),
                    operation: OperationBinding {
                        id: OperationId::new(
                            stream(u64::from(generation) * 3 + 2),
                            OperationSequence::new(1).unwrap(),
                        ),
                        digest: RequestDigest::from_bytes([7; 32]),
                    },
                    revision: StoreRevision::new(1),
                },
                signer,
            )
            .unwrap();
        Self {
            saved: SavedBinding::verified(binding.clone(), streams, proof, &Verifier).unwrap(),
            binding,
            credential,
            route: RevocationRouteReference::new([generation; 16]),
        }
    }
    async fn seed(&self, journal: &JournalController) {
        journal
            .transact(vec![
                ClientJournalChange::ReserveCredential(JournalCredentialSlot {
                    namespace: self.credential.scope.parts().namespace.as_str().to_owned(),
                    server: Some(self.binding.target().server()),
                    reference: self.credential.scope.reference(),
                    public_key: None,
                    route_reference: None,
                    route: None,
                    ssh_capture: None,
                    generation: self.binding.target().incarnation(),
                }),
                ClientJournalChange::RecordCredentialPublicKey {
                    reference: self.credential.scope.reference(),
                    public_key: self.credential.public_key,
                },
                ClientJournalChange::BindCredentialRoute {
                    reference: self.credential.scope.reference(),
                    route_reference: self.route,
                    route: self.binding.target().route().clone(),
                },
                ClientJournalChange::SaveBinding(Box::new(self.saved.clone())),
            ])
            .await
            .unwrap();
    }
    fn operation(&self) -> OperationId {
        OperationId::new(
            self.saved.streams().parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        )
    }
}

struct Credentials {
    original: PreparedCredential,
    items: Mutex<Vec<PreparedCredential>>,
    deletions: Mutex<Vec<PreparedCredential>>,
    journal: Mutex<Weak<JournalController>>,
    cleanup_started: AtomicUsize,
    cleanup_gate: Arc<Semaphore>,
}
impl Credentials {
    fn new(old: &Pairing, block_cleanup: bool) -> Self {
        Self {
            original: old.credential.clone(),
            items: Mutex::new(vec![old.credential.clone()]),
            deletions: Default::default(),
            journal: Default::default(),
            cleanup_started: AtomicUsize::new(0),
            cleanup_gate: Arc::new(Semaphore::new(if block_cleanup { 0 } else { 1 })),
        }
    }
}
impl NativeDeviceCredentials for Credentials {
    fn create(
        &self,
        _: CredentialCreationReservation,
    ) -> PairingFuture<'_, Result<PreparedCredential, PairingFailure>> {
        panic!("recovered retries must not enroll or create credentials")
    }
    fn recover_preparation<'a>(
        &'a self,
        scope: &'a CredentialScope,
    ) -> PairingFuture<'a, Result<Option<PreparedCredential>, PairingFailure>> {
        Box::pin(async move {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .find(|item| &item.scope == scope)
                .cloned())
        })
    }
    fn signing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
        lease: &'a CredentialUse,
    ) -> PairingFuture<'a, Result<Arc<dyn SigningKey>, PairingFailure>> {
        Box::pin(async move {
            lease.validate(&credential.scope)?;
            assert_eq!(credential, &self.original);
            assert!(matches!(
                lease.purpose(),
                CredentialUsePurpose::Restricted { .. }
            ));
            assert_eq!(
                lease.native_access(),
                NativeCredentialAccess::Noninteractive
            );
            assert!(self.items.lock().unwrap().contains(credential));
            Ok(Arc::new(wire::TestKey::new(99)) as Arc<dyn SigningKey>)
        })
    }
    fn delete_pairing_key<'a>(
        &'a self,
        credential: &'a PreparedCredential,
    ) -> PairingFuture<'a, Result<CredentialDeletion, PairingFailure>> {
        Box::pin(async move {
            let journal = self.journal.lock().unwrap().upgrade().unwrap();
            let inventory = journal.snapshot().unwrap().pairing_snapshot().unwrap();
            assert!(
                inventory
                    .revocations
                    .iter()
                    .any(|pending| pending.credential == *credential
                        && matches!(
                            pending.phase,
                            RevocationPhase::ReceiptRecorded {
                                acknowledged: true,
                                ..
                            }
                        ))
            );
            self.cleanup_started.fetch_add(1, Ordering::SeqCst);
            self.cleanup_gate
                .clone()
                .acquire_owned()
                .await
                .unwrap()
                .forget();
            self.deletions.lock().unwrap().push(credential.clone());
            let mut items = self.items.lock().unwrap();
            let index = items.iter().position(|item| item == credential).unwrap();
            items.remove(index);
            Ok(CredentialDeletion::Deleted)
        })
    }
}

struct NoEnrollment;
impl EnrollmentConnector for NoEnrollment {
    fn open<'a>(
        &'a self,
        _: ServerPin,
        _: RevocationRouteReference,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async { panic!("revocation cannot open enrollment") })
    }
}
impl DirectInvitationPreparation for NoEnrollment {
    fn prepare<'a>(
        &'a self,
        _: String,
        _: Zeroizing<String>,
        _: EnrollmentAttempt,
        _: &'a NativePairingRuntime,
    ) -> PairingFuture<'a, Result<DirectPreparation, EnrollmentFailure>> {
        Box::pin(async { panic!("recovery cannot prepare enrollment") })
    }
}
#[derive(Default)]
struct OrdinaryCredentialProbe(AtomicUsize);
impl CredentialSource for OrdinaryCredentialProbe {
    fn signing_access<'a>(
        &'a self,
        target: &'a ConnectionTarget,
        purpose: ConnectionPurpose,
    ) -> spaceterm_client::ports::ClientFuture<'a, SigningAccess> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { wire::Credentials.signing_access(target, purpose).await })
    }
}

struct Lookup(Principal);
impl PairingLookup for Lookup {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<authentication::PairingRecord, ProtocolError> {
        assert_eq!(
            (server, pairing, client),
            (self.0.server, self.0.pairing, self.0.client)
        );
        Ok(authentication::PairingRecord {
            principal: self.0,
            device_public_key: wire::TestKey::new(99).public_key(),
            active: false,
        })
    }
}
struct Endpoint {
    major: u32,
    server_key: Arc<dyn SigningKey>,
    available: AtomicBool,
    opens: AtomicUsize,
    tasks: Mutex<Vec<PendingRevocationTask>>,
    acknowledgements: AtomicUsize,
    statuses: AtomicUsize,
    status_gate: Arc<Semaphore>,
    journal: Mutex<Weak<JournalController>>,
}
impl Endpoint {
    fn new(major: u32, block_status: bool) -> Self {
        Self {
            major,
            server_key: Arc::new(wire::TestKey::new(1)),
            available: AtomicBool::new(false),
            opens: AtomicUsize::new(0),
            tasks: Default::default(),
            acknowledgements: AtomicUsize::new(0),
            statuses: AtomicUsize::new(0),
            status_gate: Arc::new(Semaphore::new(if block_status { 0 } else { 1 })),
            journal: Default::default(),
        }
    }
}
struct Connector(Arc<Endpoint>);
impl RestrictedConnector for Connector {
    fn open<'a>(
        &'a self,
        task: &'a PendingRevocationTask,
    ) -> PairingFuture<'a, Result<Box<dyn PairingTransport>, PairingFailure>> {
        Box::pin(async move {
            self.0.opens.fetch_add(1, Ordering::SeqCst);
            self.0.tasks.lock().unwrap().push(task.clone());
            if !self.0.available.load(Ordering::SeqCst) {
                return Err(PairingFailure::Offline);
            }
            Ok(Box::new(Transport {
                endpoint: self.0.clone(),
                task: task.clone(),
                handshake: None,
                connection: None,
                replies: VecDeque::new(),
                wait_status: false,
            }) as Box<dyn PairingTransport>)
        })
    }
}
struct Transport {
    endpoint: Arc<Endpoint>,
    task: PendingRevocationTask,
    handshake: Option<(ServerHandshake, Lookup)>,
    connection: Option<RestrictedConnection>,
    replies: VecDeque<Vec<u8>>,
    wait_status: bool,
}
impl PairingTransport for Transport {
    fn send<'a>(&'a mut self, bytes: &'a [u8]) -> PairingFuture<'a, Result<(), PairingFailure>> {
        Box::pin(async move {
            let p = self.task.parts();
            let pin = ServerPin {
                identity: p.original.server().bytes(),
                public_key: p.pinned_server_public_key,
            };
            if let Some(connection) = &mut self.connection {
                let codec = PairingRestrictedCodec::new(pin);
                let request = connection.decode_pairing_extension(bytes, &codec).unwrap();
                let target = PairingTarget::new(PairingTargetParts {
                    server: p.original.server(),
                    pairing: p.original.pairing(),
                    device: p.original.device(),
                })
                .unwrap();
                match request.kind.unwrap() {
                    pairing::restricted_extension::Kind::Acknowledgement(ack) => {
                        assert_eq!(
                            ack.operation,
                            Some(operation_to_wire(p.original.operation()))
                        );
                        assert_eq!(ack.request_digest, p.request_digest.bytes());
                        assert_eq!(ack.target, Some(target_to_wire(&target)));
                        assert_eq!(ack.acknowledged_through, 1);
                        let journal = self.endpoint.journal.lock().unwrap().upgrade().unwrap();
                        let inventory = journal.snapshot().unwrap().pairing_snapshot().unwrap();
                        assert!(matches!(
                            inventory.task(p.original.operation()).unwrap().phase,
                            RevocationPhase::ReceiptRecorded {
                                acknowledged: false,
                                ..
                            }
                        ));
                        self.endpoint
                            .acknowledgements
                            .fetch_add(1, Ordering::SeqCst);
                        return Ok(());
                    }
                    pairing::restricted_extension::Kind::Revoke(request) => {
                        assert_eq!(
                            request.operation,
                            Some(operation_to_wire(p.original.operation()))
                        );
                        assert_eq!(request.request_digest, p.request_digest.bytes());
                        assert_eq!(request.target, Some(target_to_wire(&target)));
                    }
                    pairing::restricted_extension::Kind::Status(request) => {
                        assert_eq!(
                            request.operation,
                            Some(operation_to_wire(p.original.operation()))
                        );
                        assert_eq!(request.request_digest, p.request_digest.bytes());
                        assert_eq!(request.target, Some(target_to_wire(&target)));
                        self.wait_status = true;
                        self.endpoint.statuses.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => panic!("recovered revocation escalated protocol purpose"),
                }
                let proof = ReceiptCodec::new(pin)
                    .sign(
                        PairingReceiptBody::Revocation {
                            receipt: RevocationReceipt {
                                server: p.original.server(),
                                pairing: p.original.pairing(),
                                device: p.original.device(),
                                operation: p.original.operation(),
                                revision: StoreRevision::new(2),
                                receipt_digest: RequestDigest::from_bytes([0; 32]),
                            },
                            authentication_key_digest: RequestDigest::from_bytes(
                                ring::digest::digest(
                                    &ring::digest::SHA256,
                                    &wire::TestKey::new(99).public_key(),
                                )
                                .as_ref()
                                .try_into()
                                .unwrap(),
                            ),
                            revocation_event: [10; 16],
                        },
                        self.endpoint.server_key.as_ref(),
                    )
                    .unwrap();
                self.replies.push_back(
                    connection
                        .encode_pairing_extension(
                            &pairing::RestrictedExtension {
                                kind: Some(pairing::restricted_extension::Kind::Result(
                                    pairing::RestrictedResult {
                                        request_operation: Some(operation_to_wire(
                                            p.original.operation(),
                                        )),
                                        request_digest: p.request_digest.bytes().to_vec(),
                                        target: Some(target_to_wire(&target)),
                                        disposition: 1,
                                        receipt: Some(receipt_to_wire(&proof)),
                                    },
                                )),
                            },
                            &codec,
                        )
                        .unwrap(),
                );
            } else if let Some((handshake, lookup)) = self.handshake.take() {
                let (established, reply) = handshake.receive_device(&lookup, bytes).unwrap();
                let Established::Restricted(connection) = established else {
                    panic!("retry borrowed ordinary authentication")
                };
                assert_eq!(connection.major(), self.endpoint.major);
                self.connection = Some(connection);
                self.replies.push_back(reply);
            } else {
                let local = |value: u64| {
                    let mut bytes = [0; 16];
                    bytes[8..].copy_from_slice(&value.to_be_bytes());
                    bytes
                };
                let lookup = Lookup(Principal {
                    server: p.original.server().bytes(),
                    device: p.original.device().bytes(),
                    pairing: local(p.original.pairing().local()),
                    client: local(p.original.client().local()),
                });
                let (handshake, reply) = ServerHandshake::accept(
                    pin.identity,
                    1,
                    HandshakeConfig {
                        majors: vec![self.endpoint.major],
                        ..HandshakeConfig::default()
                    },
                    self.endpoint.server_key.clone(),
                    &lookup,
                    bytes,
                )
                .unwrap();
                self.handshake = Some((handshake, lookup));
                self.replies.push_back(reply);
            }
            Ok(())
        })
    }
    fn receive(&mut self) -> PairingFuture<'_, Result<Zeroizing<Vec<u8>>, PairingFailure>> {
        Box::pin(async move {
            if self.wait_status {
                self.endpoint
                    .status_gate
                    .clone()
                    .acquire_owned()
                    .await
                    .unwrap()
                    .forget();
                self.wait_status = false;
            }
            self.replies
                .pop_front()
                .map(Zeroizing::new)
                .ok_or(PairingFailure::Offline)
        })
    }
}

struct Harness {
    executor: Arc<tokio::runtime::Runtime>,
    storage: Arc<MemoryJournal>,
    old: Pairing,
    credentials: Arc<Credentials>,
    endpoint: Arc<Endpoint>,
    ordinary: Arc<wire::peer::PeerPlan>,
    ordinary_credentials: Arc<OrdinaryCredentialProbe>,
}
impl Harness {
    fn new(major: u32, block_cleanup: bool) -> Self {
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let old = Pairing::new(1);
        Self {
            credentials: Arc::new(Credentials::new(&old, block_cleanup)),
            old,
            executor: Arc::new(executor),
            storage: Arc::new(MemoryJournal::default()),
            endpoint: Arc::new(Endpoint::new(major, !block_cleanup)),
            ordinary: Arc::new(wire::peer::PeerPlan::new(vec![major])),
            ordinary_credentials: Arc::default(),
        }
    }
    fn reopen(&self) -> Arc<JournalController> {
        Arc::new(
            self.executor
                .block_on(JournalController::recover(
                    self.storage.clone(),
                    Arc::new(CanonicalCodec),
                    Arc::new(Verifier),
                    JournalLimits::default(),
                ))
                .unwrap(),
        )
    }
    fn install(
        &self,
        journal: Arc<JournalController>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<DesktopClientRuntime>,
        Arc<NativePairingRuntime>,
        spaceterm_client::registry::ClientRegistry,
    ) {
        let (composition, protocol) = self.executor.block_on(async {
            let tracker = OperationTracker::new(journal.clone());
            let protocol = Arc::new(AcceptanceProtocol::new(journal.clone(), tracker.clone()));
            let channels = Arc::new(
                PaneChannels::new(protocol.clone(), ProjectionBudgets::supported().unwrap())
                    .unwrap(),
            );
            protocol.set_channels(channels.clone()).unwrap();
            let mut services = wire::services(
                vec![],
                Arc::new(wire::peer::Peers::one(
                    &self.old.binding,
                    self.ordinary.clone(),
                )),
            );
            services.credentials = self.ordinary_credentials.clone();
            services.delegate = protocol.clone();
            let composition = desktop_client_startup::recover(
                NativeDesktopOwners {
                    #[cfg(all(feature = "desktop-test-support", debug_assertions))]
                    fixture: false,
                    services,
                    journal: journal.clone(),
                    channels,
                    operations: protocol.clone(),
                    invalidator: protocol.clone(),
                    local_server: None,
                    tracker: Some(tracker),
                    registry_binding: Some(protocol.clone()),
                    settings: SettingsServices::default(),
                },
                MemoryView::default(),
            )
            .await
            .unwrap();
            (composition, protocol)
        });
        let registry = composition.registry.clone();
        let pairing = Arc::new(NativePairingRuntime::with_adapters(
            journal.clone(),
            self.credentials.clone(),
            Arc::new(NoEnrollment),
            Arc::new(Connector(self.endpoint.clone())),
            protocol,
            HandshakeConfig::default(),
        ));
        *self.credentials.journal.lock().unwrap() = Arc::downgrade(&journal);
        *self.endpoint.journal.lock().unwrap() = Arc::downgrade(&journal);
        let runtime = cx.update(|cx| {
            install_with_enrollment(
                composition,
                Some(NativeEnrollmentServices {
                    pairing: pairing.clone(),
                    direct: Arc::new(NoEnrollment),
                    bootstrap: None,
                    executor: self.executor.handle().clone(),
                    local: None,
                }),
                cx,
            )
            .unwrap()
        });
        (runtime, pairing, registry)
    }
    fn drive_until(&self, cx: &mut TestAppContext, message: &str, ready: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            cx.run_until_parked();
            self.executor.block_on(wire::settle());
            assert!(Instant::now() < deadline, "{message}");
        }
        cx.run_until_parked();
    }
    fn settings(&self, cx: &mut TestAppContext) -> Entity<Servers> {
        cx.update(|cx| cx.global::<InstalledServers>().0.clone())
    }
    fn snapshot(&self, cx: &mut TestAppContext) -> ServersSnapshot {
        self.settings(cx)
            .read_with(cx, |settings, _| settings.snapshot().clone())
    }
    fn send(&self, request: ServerRequest, cx: &mut TestAppContext) {
        let source = self.settings(cx);
        let request = BoundServerRequest::bind(request, &self.snapshot(cx)).unwrap();
        let requests = source.read_with(cx, |source, _| source.requests());
        cx.update(|cx| requests.send(request, cx));
    }
    fn quit(&self, runtime: &Entity<DesktopClientRuntime>, cx: &mut TestAppContext) {
        let task = runtime.update(cx, |runtime, cx| runtime.prepare_client_quit(cx));
        let result = Rc::new(RefCell::new(None));
        let completed = result.clone();
        let observer = cx.foreground_executor().spawn(async move {
            *completed.borrow_mut() = Some(task.await);
        });
        self.drive_until(cx, "desktop client quit did not drain", || {
            result.borrow().is_some()
        });
        assert_eq!(result.borrow_mut().take().unwrap(), Ok(()));
        cx.foreground_executor().block_test(observer);
    }
}

fn restart_without_connections_completes_revocation(major: u32, cx: &mut TestAppContext) {
    let h = Harness::new(major, false);
    let journal = h.reopen();
    h.executor.block_on(h.old.seed(&journal));
    let (runtime, pairing, registry) = h.install(journal.clone(), cx);
    assert_eq!(registry.servers().len(), 1);
    assert_eq!(h.snapshot(cx).servers.len(), 1);
    h.send(
        ServerRequest::UnpairThisDevice(h.snapshot(cx).servers[0].key.clone()),
        cx,
    );
    h.drive_until(
        cx,
        "Settings did not durably remove the offline pairing",
        || journal.snapshot().unwrap().revocations().len() == 1 && registry.servers().is_empty(),
    );
    h.drive_until(
        cx,
        "desktop installation did not schedule the offline self-unpair",
        || h.endpoint.opens.load(Ordering::SeqCst) > 0,
    );
    assert!(
        runtime
            .read_with(cx, |runtime, _| runtime.servers())
            .is_empty()
    );
    assert_eq!(h.snapshot(cx).pending_unpairs.len(), 1);
    assert!(h.credentials.deletions.lock().unwrap().is_empty());
    let operation = h.old.operation();
    h.send(
        ServerRequest::DismissUnpairNotice(h.snapshot(cx).pending_unpairs[0].id.clone()),
        cx,
    );
    h.drive_until(cx, "dismissal did not hide only the notice", || {
        journal.snapshot().unwrap().revocations()[0].notice_dismissed
    });
    assert!(h.snapshot(cx).pending_unpairs.is_empty());
    h.quit(&runtime, cx);
    drop((runtime, pairing, registry, journal));

    let mut restarted = cx.new_app();
    let journal = h.reopen();
    let opens = h.endpoint.opens.load(Ordering::SeqCst);
    let (runtime, pairing, registry) = h.install(journal.clone(), &mut restarted);
    assert!(registry.servers().is_empty());
    assert!(
        runtime
            .read_with(&mut restarted, |runtime, _| runtime.servers())
            .is_empty()
    );
    assert!(h.snapshot(&mut restarted).servers.is_empty());
    assert!(h.snapshot(&mut restarted).pending_unpairs.is_empty());
    h.drive_until(
        &mut restarted,
        "desktop restart discarded pending work when ordinary connections were empty",
        || h.endpoint.opens.load(Ordering::SeqCst) > opens,
    );
    assert!(journal.snapshot().unwrap().revocations()[0].notice_dismissed);
    assert!(matches!(
        h.executor.block_on(pairing.restore(
            &h.old.binding.target().local_pairing().unwrap(),
            h.old.binding.target().client()
        )),
        Err(PairingFailure::PairingRevoked)
    ));

    // Deliberate later pairing is already durable. The old scheduler owns neither
    // this binding nor its retained attachment or shared device identity key.
    let newer = Pairing::new(2);
    h.executor.block_on(newer.seed(&journal));
    let device = PreparedCredential {
        scope: CredentialScope::new(CredentialScopeParts {
            namespace: h.old.credential.scope.parts().namespace.clone(),
            purpose: CredentialPurpose::DeviceIdentity,
            reference: CredentialReference::new([33; 16]),
        })
        .unwrap(),
        public_key: wire::TestKey::new(100).public_key(),
    };
    h.credentials
        .items
        .lock()
        .unwrap()
        .extend([newer.credential.clone(), device.clone()]);
    let attachment = WorkspaceAttachment::new(
        WorkspaceId::new(newer.binding.target().server(), 1).unwrap(),
        newer.binding.target().pairing(),
        newer.binding.target().client(),
        AttachmentIncarnation::new(1).unwrap(),
    )
    .unwrap();
    h.executor
        .block_on(
            journal.record_attachment(newer.binding.target().local_pairing().unwrap(), attachment),
        )
        .unwrap();
    h.endpoint.available.store(true, Ordering::SeqCst);
    h.drive_until(
        &mut restarted,
        "later availability did not reach restricted acknowledgement",
        || h.endpoint.statuses.load(Ordering::SeqCst) == 1,
    );
    let inventory = journal.snapshot().unwrap().pairing_snapshot().unwrap();
    assert!(matches!(
        inventory.task(operation).unwrap().phase,
        RevocationPhase::ReceiptRecorded {
            acknowledged: false,
            ..
        }
    ));
    assert!(h.credentials.deletions.lock().unwrap().is_empty());
    h.endpoint.status_gate.add_permits(1);
    h.drive_until(
        &mut restarted,
        "signed receipt and restricted acknowledgement did not finish exact cleanup",
        || journal.snapshot().unwrap().revocations().is_empty(),
    );
    assert_eq!(h.endpoint.acknowledgements.load(Ordering::SeqCst), 1);
    assert_eq!(
        *h.credentials.deletions.lock().unwrap(),
        vec![h.old.credential.clone()]
    );
    assert_eq!(
        *h.credentials.items.lock().unwrap(),
        vec![newer.credential.clone(), device]
    );
    let saved = journal.snapshot().unwrap();
    assert!(saved.bindings().contains(&newer.saved));
    assert!(!saved.suppresses(&attachment));
    assert_eq!(saved.incarnation_floor(), Some(attachment.incarnation()));
    assert!(journal.ordinary_use_allowed(&newer.binding).unwrap());
    assert!(
        h.executor
            .block_on(pairing.restore(
                &newer.binding.target().local_pairing().unwrap(),
                newer.binding.target().client()
            ))
            .is_ok()
    );
    assert!(registry.servers().is_empty());
    assert!(
        h.endpoint
            .tasks
            .lock()
            .unwrap()
            .iter()
            .all(|task| task.parts().original.operation() == operation
                && task.parts().original.credential() == h.old.credential.scope.reference()
                && task.parts().original.route() == h.old.route)
    );
    assert_eq!(h.ordinary.opens.load(Ordering::SeqCst), 0);
    assert_eq!(h.ordinary_credentials.0.load(Ordering::SeqCst), 0);
    h.quit(&runtime, &mut restarted);
    restarted.quit();
}

#[gpui::test]
fn desktop_recovered_journal_scheduler_completes_offline_self_unpair_after_empty_restart_major1(
    cx: &mut TestAppContext,
) {
    restart_without_connections_completes_revocation(1, cx);
}
#[gpui::test]
fn desktop_recovered_journal_scheduler_completes_offline_self_unpair_after_empty_restart_major2(
    cx: &mut TestAppContext,
) {
    restart_without_connections_completes_revocation(2, cx);
}
#[gpui::test]
fn desktop_recovered_journal_scheduler_quit_drains_in_progress_exact_cleanup(
    cx: &mut TestAppContext,
) {
    let h = Harness::new(2, true);
    let journal = h.reopen();
    h.executor.block_on(h.old.seed(&journal));
    let (runtime, pairing, _) = h.install(journal.clone(), cx);
    h.endpoint.available.store(true, Ordering::SeqCst);
    h.send(
        ServerRequest::UnpairThisDevice(h.snapshot(cx).servers[0].key.clone()),
        cx,
    );
    h.drive_until(
        cx,
        "desktop did not start its recovered journal cleanup",
        || h.credentials.cleanup_started.load(Ordering::SeqCst) == 1,
    );
    let result = Rc::new(RefCell::new(None));
    let completed = result.clone();
    let quit = runtime.update(cx, |runtime, cx| runtime.prepare_client_quit(cx));
    let observer = cx.foreground_executor().spawn(async move {
        *completed.borrow_mut() = Some(quit.await);
    });
    for _ in 0..100 {
        cx.run_until_parked();
        h.executor.block_on(wire::settle());
    }
    assert!(
        result.borrow().is_none(),
        "app quit returned before its scheduler's native cleanup drained"
    );
    assert!(h.credentials.deletions.lock().unwrap().is_empty());
    h.credentials.cleanup_gate.add_permits(1);
    h.drive_until(cx, "app quit failed to drain its exact scheduler", || {
        result.borrow().is_some()
    });
    assert_eq!(result.borrow_mut().take().unwrap(), Ok(()));
    cx.foreground_executor().block_test(observer);
    assert!(journal.snapshot().unwrap().revocations().is_empty());
    assert_eq!(
        *h.credentials.deletions.lock().unwrap(),
        vec![h.old.credential.clone()]
    );
    // The same physical journal's lifetime claim is released only after quit drains.
    let scheduler = {
        let _entered = h.executor.enter();
        pairing.start_scheduler().unwrap()
    };
    h.executor.block_on(scheduler.shutdown()).unwrap();
    assert_eq!(h.ordinary.opens.load(Ordering::SeqCst), 0);
    assert_eq!(h.ordinary_credentials.0.load(Ordering::SeqCst), 0);
}

impl Harness {
    fn install_native_ipc(
        &self,
        paths: Arc<crate::platform::app_paths::AppPaths>,
        native: spaceterm_client_native::NativeClientProvider,
        journal: Arc<JournalController>,
        cx: &mut TestAppContext,
    ) -> (
        Entity<DesktopClientRuntime>,
        Rc<dyn desktop_client_startup::DesktopClientStartup>,
    ) {
        *self.credentials.journal.lock().unwrap() = Arc::downgrade(&journal);
        *self.endpoint.journal.lock().unwrap() = Arc::downgrade(&journal);
        let startup = desktop_client_startup::pending_ipc_startup(
            paths,
            journal,
            native,
            self.credentials.clone(),
            self.ordinary_credentials.clone(),
            self.executor.clone(),
        );
        let recovery = cx.update(|cx| startup.recover(cx));
        let result = Rc::new(RefCell::new(None));
        let completed = result.clone();
        let observer = cx.foreground_executor().spawn(async move {
            *completed.borrow_mut() = Some(recovery.await);
        });
        self.drive_until(cx, "native desktop recovery did not complete", || {
            result.borrow().is_some()
        });
        let recovered = result.borrow_mut().take().unwrap().unwrap();
        cx.foreground_executor().block_test(observer);
        let runtime = cx.update(|cx| recovered.install(cx)).unwrap();
        (runtime, startup)
    }
}

/// Reuse the authenticated restricted protocol peer over a real private IPC
/// listener. The production NativePairingConnector is not replaced.
fn serve_pending_ipc(
    executor: &tokio::runtime::Runtime,
    listener: spaceterm_host::ipc::IpcListener,
    endpoint: Arc<Endpoint>,
    task: PendingRevocationTask,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    use spaceterm_host::transport::MessageTransport;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let service = executor.spawn(async move {
        let mut stopped = std::pin::pin!(stopped);
        let mut workers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                Some(result) = workers.join_next(), if !workers.is_empty() => result.unwrap(),
                accepted = listener.accept() => {
                    let pending = accepted.unwrap();
                    assert!(pending.peer_facts().local_uid.is_some());
                    endpoint.opens.fetch_add(1, Ordering::SeqCst);
                    endpoint.tasks.lock().unwrap().push(task.clone());
                    let endpoint = endpoint.clone();
                    let task = task.clone();
                    workers.spawn(async move {
                        let mut peer = Transport {
                            endpoint,
                            task,
                            handshake: None,
                            connection: None,
                            replies: VecDeque::new(),
                            wait_status: false,
                        };
                        for _ in 0..2 {
                            let bytes = pending.recv().await.unwrap().unwrap();
                            peer.send(&bytes).await.unwrap();
                            let reply = peer.receive().await.unwrap();
                            pending.send(&reply).await.unwrap();
                        }
                        assert!(peer.connection.is_some());
                        let stream = pending.finish_authentication().unwrap();
                        while let Ok(Some(bytes)) = stream.recv().await {
                            peer.send(&bytes).await.unwrap();
                            if !peer.replies.is_empty() {
                                let reply = peer.receive().await.unwrap();
                                stream.send(&reply).await.unwrap();
                            }
                        }
                        stream.close().await.unwrap();
                    });
                }
            }
        }
        while let Some(result) = workers.join_next().await {
            result.unwrap();
        }
        listener.close().unwrap();
    });
    (stop, service)
}

fn production_pending_only_ipc_restart(major: u32, cx: &mut TestAppContext) {
    use crate::platform::{
        app_directories::{AppDirectories, AppDirectoryEnvironment},
        app_paths::AppPaths,
        unix_secure_filesystem::UnixSecureFilesystem,
    };
    use spaceterm_client_native::{NativeClientProvider, route::LocalRouteAuthority};
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture,
        ipc::IpcListener,
        server_keys::ServerKeyStore,
        startup::{BuildIdentity, DesiredServerState},
        transport::{AdmissionLimits, MessageLimit},
    };
    cx.executor().allow_parking();
    #[cfg(target_os = "macos")]
    let parent = "/private/tmp";
    #[cfg(not(target_os = "macos"))]
    let parent = "/var/tmp";
    let directory = tempfile::tempdir_in(parent).unwrap();
    let root = directory.path();
    let fixture = AccountServiceFixture::new(root, BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let identity = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(registration.server_pin())
        .unwrap();
    let route = LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap();
    let environment = AppDirectoryEnvironment {
        home: Some(root.as_os_str().to_owned()),
        xdg_state_home: Some(root.join("state").into_os_string()),
        ..Default::default()
    };
    let paths = Arc::new(
        AppPaths::from_directories(
            AppDirectories::resolve_xdg("pending-ipc-composition-test", &environment, None)
                .unwrap(),
            104,
            Arc::new(UnixSecureFilesystem),
        )
        .unwrap(),
    );
    let old = Pairing::at_native_route(
        1,
        registration.server_pin(),
        DeviceId::new([91; 16]),
        route.description().clone(),
        identity.as_ref(),
    );
    let mut endpoint = Endpoint::new(major, true);
    endpoint.server_key = identity.clone();
    let h = Harness {
        executor: Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        ),
        credentials: Arc::new(Credentials::new(&old, false)),
        old,
        storage: Arc::new(MemoryJournal::default()),
        endpoint: Arc::new(endpoint),
        ordinary: Arc::new(wire::peer::PeerPlan::new(vec![major])),
        ordinary_credentials: Arc::default(),
    };
    let journal = h.reopen();
    h.executor.block_on(h.old.seed(&journal));
    let (runtime, startup) = h.install_native_ipc(
        paths.clone(),
        NativeClientProvider::from_supervisor(fixture.reopen()),
        journal.clone(),
        cx,
    );
    assert_eq!(h.snapshot(cx).servers.len(), 1);
    h.send(
        ServerRequest::UnpairThisDevice(h.snapshot(cx).servers[0].key.clone()),
        cx,
    );
    h.drive_until(
        cx,
        "native offline self-unpair did not retain restricted work",
        || {
            let snapshot = journal.snapshot().unwrap();
            snapshot.bindings().is_empty()
                && snapshot.revocations().len() == 1
                && snapshot.revocations()[0].attempts > 0
        },
    );
    let original = journal.snapshot().unwrap().revocations()[0].clone();
    assert_eq!(original.target.target().unwrap(), *h.old.binding.target());
    assert!(
        runtime
            .read_with(cx, |runtime, _| runtime.servers())
            .is_empty()
    );
    h.send(
        ServerRequest::DismissUnpairNotice(h.snapshot(cx).pending_unpairs[0].id.clone()),
        cx,
    );
    h.drive_until(cx, "native pending notice was not dismissed", || {
        journal.snapshot().unwrap().revocations()[0].notice_dismissed
    });
    h.quit(&runtime, cx);
    drop((startup, runtime, journal));

    let mut restarted = cx.new_app();
    let journal = h.reopen();
    assert!(journal.snapshot().unwrap().bindings().is_empty());
    let ordinary_reads = h.ordinary_credentials.0.load(Ordering::SeqCst);
    let attempts = journal.snapshot().unwrap().revocations()[0].attempts;
    let (runtime, startup) = h.install_native_ipc(
        paths,
        NativeClientProvider::from_supervisor(fixture.reopen()),
        journal.clone(),
        &mut restarted,
    );
    assert!(h.snapshot(&mut restarted).servers.is_empty());
    assert!(h.snapshot(&mut restarted).pending_unpairs.is_empty());
    h.drive_until(
        &mut restarted,
        "pending-only native restart lost its scheduler",
        || journal.snapshot().unwrap().revocations()[0].attempts > attempts,
    );
    assert!(!journal.ordinary_use_allowed(&h.old.binding).unwrap());
    let ordinary_routes = crate::platform::server_client_route::refreshing_connector(
        Some(NativeClientProvider::from_supervisor(fixture.reopen())),
        journal.clone(),
        crate::platform::server_client_route::connector(
            vec![LocalRouteAuthority::new(registration.endpoint().unwrap()).unwrap()],
            None,
        )
        .unwrap(),
    );
    assert!(matches!(
        h.executor.block_on(ordinary_routes.open(h.old.binding.target(), ConnectionPurpose::Ordinary)),
        Err(failure) if failure.class() == ClientFailureClass::StaleAuthority
    ));
    assert_eq!(
        h.ordinary_credentials.0.load(Ordering::SeqCst),
        ordinary_reads
    );

    // A deliberate later pairing is durable before availability returns. It must
    // not redirect the old route, credential, pin or operation, or be cleaned up.
    let newer = Pairing::at_native_route(
        2,
        registration.server_pin(),
        h.old.binding.target().device(),
        route.description().clone(),
        identity.as_ref(),
    );
    h.executor.block_on(newer.seed(&journal));
    let device = PreparedCredential {
        scope: CredentialScope::new(CredentialScopeParts {
            namespace: h.old.credential.scope.parts().namespace.clone(),
            purpose: CredentialPurpose::DeviceIdentity,
            reference: CredentialReference::new([33; 16]),
        })
        .unwrap(),
        public_key: wire::TestKey::new(100).public_key(),
    };
    h.credentials
        .items
        .lock()
        .unwrap()
        .extend([newer.credential.clone(), device.clone()]);
    let attachment = WorkspaceAttachment::new(
        WorkspaceId::new(newer.binding.target().server(), 1).unwrap(),
        newer.binding.target().pairing(),
        newer.binding.target().client(),
        AttachmentIncarnation::new(1).unwrap(),
    )
    .unwrap();
    h.executor
        .block_on(
            journal.record_attachment(newer.binding.target().local_pairing().unwrap(), attachment),
        )
        .unwrap();
    let (stop, service) = {
        let _entered = h.executor.enter();
        let listener = IpcListener::bind(
            registration.endpoint().unwrap(),
            MessageLimit::new(spaceterm_protocol::limits::MAX_FRAME_BYTES).unwrap(),
            AdmissionLimits::new(8, Duration::from_secs(5)).unwrap(),
        )
        .unwrap();
        serve_pending_ipc(
            &h.executor,
            listener,
            h.endpoint.clone(),
            original.task.clone(),
        )
    };
    h.drive_until(
        &mut restarted,
        "production native route never authenticated retained IPC revocation",
        || h.endpoint.statuses.load(Ordering::SeqCst) == 1,
    );
    assert!(matches!(
        journal
            .snapshot()
            .unwrap()
            .pairing_snapshot()
            .unwrap()
            .task(h.old.operation())
            .unwrap()
            .phase,
        RevocationPhase::ReceiptRecorded {
            acknowledged: false,
            ..
        }
    ));
    assert!(h.credentials.deletions.lock().unwrap().is_empty());
    h.endpoint.status_gate.add_permits(1);
    h.drive_until(
        &mut restarted,
        "native restricted receipt and acknowledgement did not finish exact cleanup",
        || journal.snapshot().unwrap().revocations().is_empty(),
    );
    assert_eq!(h.endpoint.acknowledgements.load(Ordering::SeqCst), 1);
    assert_eq!(
        *h.credentials.deletions.lock().unwrap(),
        vec![h.old.credential.clone()]
    );
    assert_eq!(
        *h.credentials.items.lock().unwrap(),
        vec![newer.credential.clone(), device]
    );
    let saved = journal.snapshot().unwrap();
    assert!(saved.bindings().contains(&newer.saved));
    assert!(!saved.suppresses(&attachment));
    assert!(journal.ordinary_use_allowed(&newer.binding).unwrap());
    assert!(
        runtime
            .read_with(&mut restarted, |runtime, _| runtime.servers())
            .is_empty()
    );
    assert_eq!(
        h.ordinary_credentials.0.load(Ordering::SeqCst),
        ordinary_reads
    );
    assert!(
        h.endpoint
            .tasks
            .lock()
            .unwrap()
            .iter()
            .all(|task| task == &original.task)
    );
    let found = fixture.reopen().discover().unwrap().unwrap();
    assert_eq!(found.server_pin(), registration.server_pin());
    assert_eq!(found.key(), registration.key());
    assert_eq!(found.generation(), registration.generation());
    assert_eq!(found.desired_state(), DesiredServerState::Running);
    h.quit(&runtime, &mut restarted);
    drop((startup, runtime));
    stop.send(()).unwrap();
    h.executor.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), service)
            .await
            .unwrap()
            .unwrap();
    });
    restarted.quit();
}

#[gpui::test]
fn desktop_production_pending_only_ipc_restart_completes_exact_revocation_major1(
    cx: &mut TestAppContext,
) {
    production_pending_only_ipc_restart(1, cx);
}

#[gpui::test]
fn desktop_production_pending_only_ipc_restart_completes_exact_revocation_major2(
    cx: &mut TestAppContext,
) {
    production_pending_only_ipc_restart(2, cx);
}
