use super::*;
use spaceterm_host::native_deployment::contracts::NativeRequestId;
use spaceterm_protocol::{
    authentication::ServerPin,
    pairing_adapter::{
        EnrollmentAuthorization, EnrollmentClient, EnrollmentConnection,
        PublicEnrollmentReservation, ReceiptCodec,
    },
};
use spaceterm_server::deployment::owner::{
    OWNER_FRAME_LIMIT, OwnerEnrollmentReply, OwnerEnrollmentRequest, OwnerReplyDisposition,
};
use std::{
    io::{Read as _, Write as _},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};
use zeroize::Zeroizing;

struct OwnerChild {
    child: Child,
    input: Option<ChildStdin>,
    output: ChildStdout,
}
impl OwnerChild {
    fn start(root: &std::path::Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_spaceterm-server"));
        command
            .args([
                std::ffi::OsStr::new("--test-account-root"),
                root.as_os_str(),
                std::ffi::OsStr::new("owner-enrollment"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        Self {
            input: child.stdin.take(),
            output: child.stdout.take().unwrap(),
            child,
        }
    }
    fn send(&mut self, bytes: &[u8]) {
        let input = self.input.as_mut().unwrap();
        input
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        input.write_all(bytes).unwrap();
        input.flush().unwrap();
    }
    fn recv(&mut self) -> Zeroizing<Vec<u8>> {
        let mut prefix = [0; 4];
        self.output.read_exact(&mut prefix).unwrap();
        let size = u32::from_be_bytes(prefix) as usize;
        assert!((1..=OWNER_FRAME_LIMIT).contains(&size));
        let mut bytes = Zeroizing::new(vec![0; size]);
        self.output.read_exact(&mut bytes).unwrap();
        bytes
    }
    fn issue(
        &mut self,
        request: NativeRequestId,
        server: ServerId,
        seed: u8,
    ) -> (
        OwnerEnrollmentReply,
        PublicEnrollmentReservation,
        Option<EnrollmentAuthorization>,
    ) {
        let claim = verified_claim(server, seed);
        let (device_proof, authentication_proof) = VerifiedDeviceClaim::proofs(
            claim.proposal(),
            &DeviceKey::new(seed),
            &DeviceKey::new(seed + 1),
        )
        .unwrap();
        self.send(
            &serde_json::to_vec(&OwnerEnrollmentRequest {
                request,
                proposal: claim.proposal().clone(),
                device_proof: device_proof.to_vec(),
                authentication_proof: authentication_proof.to_vec(),
            })
            .unwrap(),
        );
        let reply: OwnerEnrollmentReply = serde_json::from_slice(&self.recv()).unwrap();
        let bytes = self.recv();
        let (public, authorization) = if reply.disposition
            == OwnerReplyDisposition::AuthorizationDelivered
        {
            let (public, authorization) = EnrollmentAuthorization::from_delivery(bytes).unwrap();
            (public, Some(authorization))
        } else {
            (PublicEnrollmentReservation::decode(&bytes).unwrap(), None)
        };
        (reply, public, authorization)
    }
    fn handshake(
        &mut self,
        pin: ServerPin,
        public: PublicEnrollmentReservation,
        seed: u8,
        major: u32,
    ) -> EnrollmentConnection {
        let (client, hello) = EnrollmentClient::begin(
            pin,
            public,
            vec![major],
            Arc::new(DeviceKey::new(seed)),
            Arc::new(DeviceKey::new(seed + 1)),
        )
        .unwrap();
        self.send(&hello);
        let (client, proof) = client.receive_server(&self.recv()).unwrap();
        self.send(&proof);
        client.receive_ready(&self.recv()).unwrap()
    }
    fn finish(mut self) {
        self.input.take();
        let status = self.child.wait().unwrap();
        let mut diagnostics = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut diagnostics)
            .unwrap();
        assert!(status.success(), "{diagnostics}");
        assert!(diagnostics.is_empty());
    }
}
impl Drop for OwnerChild {
    fn drop(&mut self) {
        self.input.take();
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn receipt(
    connection: &mut EnrollmentConnection,
    reply: &[u8],
    pin: ServerPin,
) -> spaceterm_core::pairing::SignedPairingReceipt {
    let spaceterm_protocol::wire::pairing::enrollment_message::Kind::Result(result) =
        connection.decode(reply).unwrap().kind.unwrap()
    else {
        panic!("restricted result");
    };
    ReceiptCodec::new(pin)
        .from_wire(&result.receipt.unwrap())
        .unwrap()
}

#[test]
fn production_owner_enrollment_endpoint_preserves_stopped_server() {
    for major in [1, 2] {
        let (root, fixture, _) = initialized_account(DesiredServerState::Stopped);
        let supervisor = fixture.reopen();
        let registration = supervisor.discover().unwrap().unwrap();
        let pin = registration.server_pin();
        let key = fs::read(fixture.data_directory().join("identity")).unwrap();
        let generation = registration.generation();
        let request = supervisor.allocate_request().unwrap();
        let mut child = OwnerChild::start(root.path());
        let (reply, public, authorization) = child.issue(request, ServerId::new(pin.identity), 61);
        assert_eq!(
            reply.disposition,
            OwnerReplyDisposition::AuthorizationDelivered
        );
        let mut connection = child.handshake(pin, public, 61, major);
        let submission = connection
            .submission_message(authorization.as_ref().unwrap(), &DeviceKey::new(61))
            .unwrap();
        child.send(&submission);
        let original_receipt = receipt(&mut connection, &child.recv(), pin);
        child.finish();
        // Compact native authority, reconstruct the executable and recover the
        // original receipt. Neither retry can return a replacement authorization.
        supervisor.acknowledge_request(request).unwrap();
        let mut child = OwnerChild::start(root.path());
        let (reply, public, authorization) = child.issue(request, ServerId::new(pin.identity), 61);
        assert_eq!(reply.disposition, OwnerReplyDisposition::ReceiptAvailable);
        assert!(authorization.is_none());
        let mut connection = child.handshake(pin, public, 61, major);
        child.send(&connection.lookup_message().unwrap());
        assert_eq!(
            receipt(&mut connection, &child.recv(), pin),
            original_receipt
        );
        child.finish();
        let current = supervisor.discover().unwrap().unwrap();
        assert_eq!(current.desired_state(), DesiredServerState::Stopped);
        assert_eq!(current.server_pin(), pin);
        assert_eq!(current.generation(), generation);
        assert!(fs::read(fixture.data_directory().join("identity")).unwrap() == key);
        assert!(!current.endpoint().unwrap().path().exists());
    }
}

#[test]
fn production_owner_endpoint_reports_lost_initial_reply_without_replacement_authorization() {
    let (root, fixture, _) = initialized_account(DesiredServerState::Stopped);
    let supervisor = fixture.reopen();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let request = supervisor.allocate_request().unwrap();
    let mut child = OwnerChild::start(root.path());
    let (original, _, authorization) = child.issue(request, ServerId::new(pin.identity), 71);
    assert!(authorization.is_some());
    // Drop the transient delivery without consuming it.
    drop(authorization);
    child.finish();
    let mut child = OwnerChild::start(root.path());
    let (replayed, _, authorization) = child.issue(request, ServerId::new(pin.identity), 71);
    assert_eq!(replayed.outcome, original.outcome);
    assert_eq!(
        replayed.disposition,
        OwnerReplyDisposition::AuthorizationInvalidated
    );
    assert!(authorization.is_none());
    child.finish();
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
}

#[test]
fn malformed_owner_proof_and_oversized_native_frame_have_no_authority_or_store_effects() {
    let (root, fixture, _) = initialized_account(DesiredServerState::Stopped);
    let supervisor = fixture.reopen();
    let request = supervisor.allocate_request().unwrap();
    let pin = supervisor.discover().unwrap().unwrap().server_pin();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    let database = fixture
        .data_directory()
        .join("organization")
        .join(spaceterm_host::storage::DATABASE_FILE_NAME);
    let store_before = fs::read(&database).unwrap();
    for invalid in [false, true] {
        let mut child = OwnerChild::start(root.path());
        if invalid {
            child
                .input
                .as_mut()
                .unwrap()
                .write_all(&((OWNER_FRAME_LIMIT + 1) as u32).to_be_bytes())
                .unwrap();
        } else {
            child.send(
                &serde_json::to_vec(&OwnerEnrollmentRequest {
                    request,
                    proposal: verified_claim(ServerId::new(pin.identity), 81)
                        .proposal()
                        .clone(),
                    device_proof: vec![0; 64],
                    authentication_proof: vec![0; 64],
                })
                .unwrap(),
            );
        }
        child.input.take();
        assert!(!child.child.wait().unwrap().success());
        let mut diagnostic = String::new();
        child
            .child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut diagnostic)
            .unwrap();
        assert!(matches!(
            diagnostic.trim(),
            "owner request invalid" | "owner channel unavailable"
        ));
        let mut output = Vec::new();
        child.output.read_to_end(&mut output).unwrap();
        assert!(output.is_empty());
        assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
        assert_eq!(fs::read(&database).unwrap(), store_before);
    }
}

#[tokio::test]
async fn production_stopped_owner_composes_real_native_providers_before_issuance() {
    let (root, fixture, _) = initialized_account(DesiredServerState::Stopped);
    let supervisor = spaceterm_server::account_startup::test_support::supervisor(
        root.path(),
        Arc::new(spaceterm_server::account_startup::test_support::StartupAbsence),
    )
    .unwrap();
    let registration = supervisor.discover().unwrap().unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let request = supervisor.allocate_request().unwrap();
    let claim = verified_claim(server, 61);
    let home = spaceterm_host::account_service::TrustedAccountFacts::current()
        .unwrap()
        .home()
        .to_owned();
    let executor = spaceterm_server::account_startup::production_stopped_owner(
        supervisor.clone(),
        home,
        request,
        &claim,
    )
    .await
    .unwrap_or_else(|failure| panic!("production composition: {failure:?}"));
    let issued = executor
        .issue(request, claim)
        .await
        .unwrap_or_else(|failure| panic!("production issue: {failure:?}"));
    assert!(issued.authorization.is_some());
    let report = executor.close().await.unwrap().unwrap();
    assert!(report.cleanup_confirmed && report.started_writes_finished);
    assert_eq!(
        fixture
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}

#[tokio::test]
async fn production_owner_endpoint_routes_running_requests_to_original_actor_without_opening_storage()
 {
    use spaceterm_server::{
        account_startup::AccountStartup, adapters::core::CorePorts,
        deployment::owner::OwnerEndpoint, service::RecoveredServer,
    };
    let (root, fixture, home) = initialized_account(DesiredServerState::Running);
    let supervisor = Arc::new(fixture.reopen());
    let registration = supervisor.discover().unwrap().unwrap();
    let pin = registration.server_pin();
    let identity = StoreIdentity {
        server: ServerId::new(pin.identity),
        account: registration.key(),
        server_public_key: pin.public_key,
    };
    let account = AccountStartup::open(registration)
        .unwrap()
        .open_store(home.clone())
        .unwrap();
    let mut boundary = native::Fixture::new();
    let runtime = boundary.factory.clone();
    let host = boundary.host.clone();
    boundary.config.host.authority = Arc::new(RunningAuthority(pin));
    boundary.config.core =
        CorePorts::production(identity, home, boundary.dependencies.clock.clone()).unwrap();
    boundary.dependencies.store = Box::new(account.store);
    boundary.dependencies.credentials = account.credentials;
    let server = RecoveredServer::recover(boundary.config, boundary.dependencies)
        .await
        .unwrap();
    let before = server.handle().snapshot();
    let endpoint = OwnerEndpoint::start(supervisor.clone(), server.handle()).unwrap();
    // Blocking pipe observers run off the Tokio reactor that owns the listener.
    for (major, seed) in [(1, 91), (2, 93)] {
        let request = supervisor.allocate_request().unwrap();
        let root = root.path().to_owned();
        tokio::task::spawn_blocking(move || {
            let mut child = OwnerChild::start(&root);
            let (reply, public, authorization) = child.issue(request, identity.server, seed);
            assert_eq!(
                reply.disposition,
                OwnerReplyDisposition::AuthorizationDelivered
            );
            let mut connection = child.handshake(pin, public, seed, major);
            child.send(
                &connection
                    .submission_message(authorization.as_ref().unwrap(), &DeviceKey::new(seed))
                    .unwrap(),
            );
            receipt(&mut connection, &child.recv(), pin);
            child.finish();
        })
        .await
        .unwrap();
    }
    let after = server.handle().snapshot();
    assert_eq!(after.state(), before.state());
    assert_eq!(after.epoch(), before.epoch());
    assert_eq!(after.runtime(), before.runtime());
    assert!(runtime.starts.lock().unwrap().is_empty());
    assert_eq!(
        server.handle().phase(),
        spaceterm_server::api::ServicePhase::Ready
    );
    endpoint.close().await.unwrap();
    assert_eq!(
        supervisor.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Running
    );
    server
        .handle()
        .shutdown(spaceterm_server::shutdown::TrustedShutdownRequest { authority: host })
        .await
        .unwrap();
    server.wait().await.unwrap();
}
