//! Native manager and credential boundaries are injected. The IPC transport,
//! paired handshake, host journal, original requests and signed custody are real.
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use spaceterm_client::{
    api::{ConnectionTarget, PairedServerBinding},
    ports::{ClientFuture, ConnectionJournal},
};
use spaceterm_client_native::{
    NativeClientProvider,
    native::{
        NativeActionPrompt,
        runtime::{RuntimeClientAccess, RuntimeClientIdentity, RuntimeServiceProcesses},
    },
    route::LocalRouteAuthority,
};
use spaceterm_core::{
    authority::{ServerEpoch, SessionGeneration, SessionKey},
    identity::{ClientId, DeviceId, PairingId, PaneId, ServerId},
    native_management::NativeRetirementAction,
    pairing::{CredentialReference, LocalPairingGeneration},
    retirement::{CleanupResult, ConfirmedCleanupClass, SessionCleanupObservation},
};
use spaceterm_host::{
    account_service::{
        LingerState, RegisteredServer, RuntimeAssessment, ServiceAction, ServiceClock,
        ServiceFailure, ServiceFuture, ServiceManager, ServiceOutcome, ServiceRequestState,
        ServiceTime, SupervisorState,
        runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply, RuntimeCustody},
        test_support::AccountServiceFixture,
    },
    ipc::IpcListener,
    server_keys::ServerKeyStore,
    startup::{BuildIdentity, DesiredServerState},
    transport::{AdmissionLimits, MessageLimit, MessageTransport},
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        Established, HandshakeConfig, PairingLookup, PairingRecord, Principal, ServerHandshake,
        SigningKey,
    },
    codec::Payload,
    negotiation::CAPABILITY_ACCOUNT_RUNTIME,
    wire::{common, v1, v2},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

struct Device(Ed25519KeyPair);
impl SigningKey for Device {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
struct Identity {
    binding: PairedServerBinding,
    signer: Arc<Device>,
}
impl ConnectionJournal for Identity {
    fn recover_bindings(&self) -> ClientFuture<'_, Vec<PairedServerBinding>> {
        Box::pin(async { Ok(vec![self.binding.clone()]) })
    }
    fn ordinary_use_allowed<'a>(
        &'a self,
        binding: &'a PairedServerBinding,
    ) -> ClientFuture<'a, bool> {
        Box::pin(async move { Ok(binding == &self.binding) })
    }
}
struct RuntimeIdentity(Arc<Identity>);
impl RuntimeClientIdentity for RuntimeIdentity {
    fn access<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, RuntimeClientAccess> {
        Box::pin(async move {
            Ok(RuntimeClientAccess {
                binding: self.0.binding.clone(),
                signer: self.0.signer.clone(),
                journal: self.0.clone(),
            })
        })
    }
}
struct Pairings(PairingRecord);
impl PairingLookup for Pairings {
    fn lookup(
        &self,
        server: [u8; 16],
        pairing: [u8; 16],
        client: [u8; 16],
    ) -> Result<PairingRecord, ProtocolError> {
        if (server, pairing, client)
            != (
                self.0.principal.server,
                self.0.principal.pairing,
                self.0.principal.client,
            )
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(self.0)
    }
}
struct Clock;
impl ServiceClock for Clock {
    fn now(&self) -> Result<ServiceTime, ServiceFailure> {
        Ok(ServiceTime {
            boot: [1; 32],
            millis: 1,
        })
    }
}
struct Manager(Mutex<SupervisorState>);
impl ServiceManager for Manager {
    fn status<'a>(&'a self, _: Option<&'a RegisteredServer>) -> ServiceFuture<'a, SupervisorState> {
        Box::pin(async { Ok(*self.0.lock().unwrap()) })
    }
    fn linger(&self) -> ServiceFuture<'_, Option<LingerState>> {
        Box::pin(async { Ok(cfg!(target_os = "linux").then_some(LingerState::Enabled)) })
    }
    fn configure<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn start<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            *self.0.lock().unwrap() = SupervisorState::Running;
            Ok(())
        })
    }
    fn stop<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async {
            *self.0.lock().unwrap() = SupervisorState::Inactive;
            Ok(())
        })
    }
    fn uninstall<'a>(&'a self, _: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn verify_installation(&self, _: &RegisteredServer) -> Result<(), ServiceFailure> {
        Ok(())
    }
}

#[tokio::test]
async fn production_cli_native_lifecycle_preserves_exact_assessment_and_request_across_reopen() {
    for major in [1, 2] {
        let root = tempfile::Builder::new()
            .prefix("st-rpc-")
            .tempdir_in("/var/tmp")
            .unwrap();
        let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registration = fixture.initialize(DesiredServerState::Running).unwrap();
        let server = ServerId::new(registration.server_pin().identity);
        let signer = ServerKeyStore::open(&registration)
            .unwrap()
            .load_identity(registration.server_pin())
            .unwrap();
        let epoch = ServerEpoch::new(1);
        let custody = RuntimeCustody::begin(&registration, epoch, signer.clone()).unwrap();
        let session = SessionKey {
            pane: PaneId::new(server, 1).unwrap(),
            epoch,
            generation: SessionGeneration::new(3),
        };
        // The owning runtime inventory is the uncontrolled native runtime boundary.
        // Separate server actor tests prove acquisition and real Session ownership.
        let inventory = RuntimeAssessment {
            server,
            epoch: Some(epoch),
            generation: 17,
            sessions: vec![session],
            token: [7; 32],
        };
        let endpoint = registration.endpoint().unwrap();
        let route = LocalRouteAuthority::new(endpoint.clone()).unwrap();
        let device = Arc::new(Device(
            Ed25519KeyPair::from_seed_unchecked(&[8; 32]).unwrap(),
        ));
        let target = ConnectionTarget::new(
            registration.server_pin(),
            DeviceId::new([4; 16]),
            PairingId::new(server, 1).unwrap(),
            ClientId::new(server, 1).unwrap(),
            route.description().clone(),
            CredentialReference::new([2; 16]),
            LocalPairingGeneration::new(1).unwrap(),
        )
        .unwrap();
        let identity = Arc::new(Identity {
            binding: PairedServerBinding::new(target),
            signer: device.clone(),
        });
        let local_id = {
            let mut id = [0; 16];
            id[15] = 1;
            id
        };
        let pairings = Pairings(PairingRecord {
            principal: Principal {
                server: server.bytes(),
                device: [4; 16],
                pairing: local_id,
                client: local_id,
            },
            device_public_key: device.public_key(),
            active: true,
        });
        let listener = IpcListener::bind(
            endpoint,
            MessageLimit::envelope(),
            AdmissionLimits::new(4, Duration::from_secs(5)).unwrap(),
        )
        .unwrap();
        let runtime_inventory = inventory.clone();
        let native = fixture.reopen();
        let served = tokio::spawn(async move {
            let mut listener = Some(listener);
            let mut calls = Vec::new();
            loop {
                let pending = listener.as_ref().unwrap().accept().await.unwrap();
                let hello = pending.recv().await.unwrap().unwrap();
                let (handshake, reply) = ServerHandshake::accept(
                    server.bytes(),
                    1,
                    HandshakeConfig {
                        majors: vec![major],
                        capabilities: vec![CAPABILITY_ACCOUNT_RUNTIME],
                        ..HandshakeConfig::default()
                    },
                    signer.clone(),
                    &pairings,
                    &hello,
                )
                .unwrap();
                pending.send(&reply).await.unwrap();
                let proof = pending.recv().await.unwrap().unwrap();
                let (Established::Ordinary(mut codec), ready) =
                    handshake.receive_device(&pairings, &proof).unwrap()
                else {
                    panic!("ordinary paired Client required")
                };
                pending.send(&ready).await.unwrap();
                let transport = pending.finish_authentication().unwrap();
                let bytes = transport.recv().await.unwrap().unwrap();
                let request = match codec.decode(&bytes).unwrap() {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::AccountRuntimeRequest(value)),
                    }) => value,
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::AccountRuntimeRequest(value)),
                    }) => value,
                    _ => panic!("native runtime request required"),
                };
                let call: AccountRuntimeCall = serde_json::from_slice(&request.body).unwrap();
                calls.push(call.clone());
                let reply = match call {
                    AccountRuntimeCall::Assess => {
                        AccountRuntimeReply::Assessment(runtime_inventory.clone())
                    }
                    AccountRuntimeCall::Reserve(original) => {
                        assert_eq!(original, runtime_inventory);
                        AccountRuntimeReply::Reserved
                    }
                    AccountRuntimeCall::Quiesce {
                        request,
                        action,
                        assessment,
                    } => {
                        assert_eq!(assessment, runtime_inventory);
                        assert_eq!(action, NativeRetirementAction::Stop);
                        let registration = native.discover().unwrap().unwrap();
                        assert_eq!(
                            native.verify_retirement_request(server, epoch).unwrap(),
                            (request, action)
                        );
                        // Lose the reply while retaining the original cleanup owner.
                        transport.close().await.unwrap();
                        listener.take().unwrap().close().unwrap();
                        let observation = SessionCleanupObservation::new(
                            session,
                            action.reason(),
                            CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                        )
                        .unwrap();
                        custody
                            .completed(&registration, request, action, &assessment, &[observation])
                            .unwrap();
                        break;
                    }
                    AccountRuntimeCall::Abort(_) => panic!("successful admission must not abort"),
                    AccountRuntimeCall::AssessNativeLifecycle(_)
                    | AccountRuntimeCall::AdmitNativeLifecycle(_)
                    | AccountRuntimeCall::AssessNativeStage(_)
                    | AccountRuntimeCall::AdmitNativeStage(_) => {
                        panic!("local runtime bridge must retain its own native admission")
                    }
                };
                let message = common::AccountRuntimeMessage {
                    request: request.request,
                    body: serde_json::to_vec(&reply).unwrap(),
                    request_generation: request.request_generation,
                };
                let payload = match major {
                    1 => Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::AccountRuntimeReply(message)),
                    }),
                    _ => Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
                    }),
                };
                transport
                    .send(&codec.encode(&payload).unwrap())
                    .await
                    .unwrap();
                transport.close().await.unwrap();
            }
            calls
        });
        let manager = Arc::new(Manager(Mutex::new(SupervisorState::Running)));
        let processes = Arc::new(RuntimeServiceProcesses::new(Arc::new(RuntimeIdentity(
            identity.clone(),
        ))));
        let provider = NativeClientProvider::from_supervisor(fixture.with_providers(
            manager.clone(),
            processes.clone(),
            Arc::new(Clock),
            None,
        ));
        let assessment = provider
            .management()
            .assess(ServiceAction::Stop)
            .await
            .unwrap();
        assert_eq!(assessment.affected_sessions(), 1);
        let prompt = NativeActionPrompt::from_assessment(&assessment).unwrap();
        drop(assessment);
        drop(provider);
        let reopened = NativeClientProvider::from_supervisor(fixture.with_providers(
            manager,
            processes,
            Arc::new(Clock),
            None,
        ));
        let assessment = reopened
            .management()
            .restore_assessment(ServiceAction::Stop, prompt.token)
            .await
            .unwrap();
        assert_eq!(assessment.request(), prompt.request);
        assert_eq!(assessment.affected_sessions(), 1);
        let progress = reopened
            .management()
            .stop(assessment.confirm().unwrap())
            .await
            .unwrap();
        assert_eq!(progress.request, prompt.request);
        assert_eq!(
            progress.state,
            ServiceRequestState::Completed(ServiceOutcome::Stopped)
        );
        let resolved = reopened.management().resolve(prompt.request).await.unwrap();
        assert_eq!(resolved.request_digest, progress.request_digest);
        assert_eq!(resolved.state, progress.state);
        let calls = served.await.unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0], AccountRuntimeCall::Assess);
        assert_eq!(calls[1], AccountRuntimeCall::Reserve(inventory.clone()));
        assert!(
            matches!(&calls[2], AccountRuntimeCall::Quiesce { request, assessment, .. } if request.id_bytes() == *prompt.request.as_bytes() && *assessment == inventory)
        );
    }
}
