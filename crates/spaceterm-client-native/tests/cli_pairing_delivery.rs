//! The public CLI bridge carries an existing claim JSON document and an opaque
//! protocol delivery. It never turns either into a normal CLI result envelope.

use spaceterm_client_native::cli::{CliCommand, CliFailure, parse};
use std::ffi::OsString;

const FINGERPRINT: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn invocation(args: &[&str]) -> Result<spaceterm_client_native::cli::CliInvocation, CliFailure> {
    parse(args.iter().map(OsString::from))
}

#[test]
fn claim_delivery_requires_both_explicit_stream_flags_and_exact_device_fingerprint() {
    let parsed = invocation(&[
        "--non-interactive",
        "pair",
        "authorize",
        "--claim-stdin",
        "--authorization-stdout",
        "--device-fingerprint",
        FINGERPRINT,
    ]);
    assert!(
        parsed.is_ok(),
        "the documented private-pipe invocation must parse"
    );

    for args in [
        vec![
            "pair",
            "authorize",
            "--claim-stdin",
            "--device-fingerprint",
            FINGERPRINT,
        ],
        vec![
            "pair",
            "authorize",
            "--authorization-stdout",
            "--device-fingerprint",
            FINGERPRINT,
        ],
        vec![
            "pair",
            "authorize",
            "--claim-stdin",
            "--authorization-stdout",
        ],
        vec![
            "pair",
            "authorize",
            "--claim-stdin",
            "--authorization-stdout",
            "--device-fingerprint",
            "secret",
        ],
        vec![
            "pair",
            "authorize",
            "--claim-stdin",
            "--claim-stdin",
            "--authorization-stdout",
            "--device-fingerprint",
            FINGERPRINT,
        ],
        vec![
            "pair",
            "authorize",
            "22222222222222222222222222222222",
            "--claim-stdin",
            "--authorization-stdout",
            "--device-fingerprint",
            FINGERPRINT,
        ],
    ] {
        assert_eq!(invocation(&args), Err(CliFailure::Usage));
    }
}

#[test]
fn regular_authorize_keeps_its_existing_request_contract() {
    assert!(matches!(
        invocation(&[
            "--json",
            "pair",
            "authorize",
            "22222222222222222222222222222222",
            "--device-fingerprint",
            FINGERPRINT,
        ])
        .unwrap()
        .command,
        CliCommand::PairAuthorize { .. }
    ));
}

use spaceterm_client::management_cli::{
    AssessmentToken, ConfirmationPrompt, ConfirmationReply, ConfirmedManagementAction,
    ManagementAssessment, ManagementFuture, ManagementProgress,
};
use spaceterm_client_native::cli::{
    bindings::*,
    output::CliOutput,
    pairing_pipe::{self, MAX_AUTHORIZATION_DELIVERY_BYTES, MAX_PUBLIC_CLAIM_BYTES},
    runner::{
        CliDependencies, CliIo, CliSignalContext, Dispatch, dispatch, dispatch_until,
        dispatch_until_with_context, dispatch_with_context,
    },
};
use spaceterm_core::{identity::DeviceId, management::ManagementRequestReference};
use spaceterm_host::{
    native_management::SudoMode,
    startup::{AccountBuildKey, BuildIdentity},
};
use std::{
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use zeroize::Zeroizing;

#[test]
fn mechanical_claim_reader_preserves_exact_json_and_rejects_bound_plus_one() {
    // A supplied public buffer exercises the I/O boundary, not claim authority.
    let json = b"{\"public_claim\":[1,2,3]}\n";
    assert_eq!(
        pairing_pipe::read_public_claim(&mut json.as_slice())
            .unwrap()
            .as_slice(),
        json
    );
    assert_eq!(
        pairing_pipe::read_public_claim(&mut &b""[..]).err(),
        Some(CliFailure::Usage)
    );
    let at_bound = vec![b' '; MAX_PUBLIC_CLAIM_BYTES];
    assert_eq!(
        pairing_pipe::read_public_claim(&mut at_bound.as_slice())
            .unwrap()
            .len(),
        MAX_PUBLIC_CLAIM_BYTES
    );
    let over_bound = vec![b' '; MAX_PUBLIC_CLAIM_BYTES + 1];
    assert_eq!(
        pairing_pipe::read_public_claim(&mut over_bound.as_slice()).err(),
        Some(CliFailure::LimitExceeded)
    );
}

#[test]
fn mechanical_delivery_writer_preserves_binary_without_envelope_or_newline() {
    let bytes = Zeroizing::new(
        (0..=255)
            .cycle()
            .take(MAX_AUTHORIZATION_DELIVERY_BYTES)
            .collect::<Vec<u8>>(),
    );
    let mut output = Zeroizing::new(Vec::new());
    pairing_pipe::write_authorization_delivery(&mut *output, &bytes).unwrap();
    assert!(
        output.as_slice() == bytes.as_slice(),
        "opaque delivery bytes changed"
    );
    output = Zeroizing::new(Vec::new());
    assert_eq!(
        pairing_pipe::write_authorization_delivery(
            &mut *output,
            &vec![0; MAX_AUTHORIZATION_DELIVERY_BYTES + 1]
        ),
        Err(CliFailure::LimitExceeded)
    );
    assert!(output.is_empty());
    assert_eq!(
        pairing_pipe::write_authorization_delivery(&mut *output, &[]),
        Err(CliFailure::UnsafeNativeState)
    );
}

struct BrokenIo;
impl Read for BrokenIo {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("sensitive-native-error"))
    }
}
impl Write for BrokenIo {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("sensitive-native-error"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn mechanical_pipe_errors_have_only_typed_classifications() {
    assert_eq!(
        pairing_pipe::read_public_claim(&mut BrokenIo).err(),
        Some(CliFailure::IoUnavailable)
    );
    let failure =
        pairing_pipe::write_authorization_delivery(&mut BrokenIo, b"opaque-boundary-bytes")
            .unwrap_err();
    assert_eq!(failure, CliFailure::IoUnavailable);
    assert!(!format!("{failure:?} {failure}").contains("sensitive-native-error"));
}

struct StalledDiagnosticWriter {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
    finished: std::sync::mpsc::Sender<()>,
}
impl Write for StalledDiagnosticWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.entered.send(()).unwrap();
        self.release
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| io::Error::other("controlled stalled write"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for StalledDiagnosticWriter {
    fn drop(&mut self) {
        let _ = self.finished.send(());
    }
}

#[test]
fn non_pipe_diagnostic_deadline_bounds_the_wait_for_a_stalled_write() {
    let (entered, writing) = std::sync::mpsc::channel();
    let (release, stalled) = std::sync::mpsc::channel();
    let (finished, closed) = std::sync::mpsc::channel();
    let start = std::time::Instant::now();
    let result = pairing_pipe::write_public_diagnostic_bounded(
        StalledDiagnosticWriter {
            entered,
            release: stalled,
            finished,
        },
        b"{\"schema\":1,\"error\":\"outcome_pending\"}\n",
    );
    let elapsed = start.elapsed();
    writing.recv_timeout(Duration::from_secs(1)).unwrap();
    let _ = release.send(());
    closed.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(result, Err(CliFailure::IoUnavailable));
    assert!(
        elapsed < Duration::from_secs(1),
        "a blocking sink defeated the public diagnostic deadline"
    );
}

#[derive(Default)]
struct Io {
    input: Option<Zeroizing<Vec<u8>>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    delivery: Zeroizing<Vec<u8>>,
}
impl CliIo for Io {
    fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        self.stdout.extend_from_slice(bytes);
        Ok(())
    }
    fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        self.stderr.extend_from_slice(bytes);
        Ok(())
    }
    fn read_authorization_claim(&mut self) -> Result<Zeroizing<Vec<u8>>, CliFailure> {
        self.input
            .take()
            .ok_or(CliFailure::AuthorizationUnavailable)
    }
    fn write_authorization_delivery(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
        pairing_pipe::write_authorization_delivery(&mut *self.delivery, bytes)
    }
    fn read_confirmation(
        &mut self,
        _: &ConfirmationPrompt,
    ) -> Result<ConfirmationReply, CliFailure> {
        panic!("claim authorization must not prompt or consume a controlling terminal")
    }
}
struct ImmediateAuthorization {
    seen: Mutex<Vec<u8>>,
    result: Mutex<Option<Result<PairingAuthorizationDelivery, PairingAuthorizationFailure>>>,
}
impl PairingCommandBindings for ImmediateAuthorization {
    fn enroll<'a>(
        &'a self,
        _: Option<spaceterm_client_native::cli::arguments::PublicFingerprint>,
        _: bool,
        _: SudoMode,
        _: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        panic!("unexpected enrollment")
    }
    fn authorize(
        &self,
        _: spaceterm_client_native::cli::arguments::PublicRequestId,
        _: spaceterm_client_native::cli::arguments::PublicFingerprint,
    ) -> ManagementFuture<'_, ManagementProgress> {
        panic!("unexpected retained-request authorization")
    }
    fn authorize_claim(
        &self,
        claim: Zeroizing<Vec<u8>>,
        expected: spaceterm_client_native::cli::arguments::PublicFingerprint,
    ) -> PairingAuthorizationFuture<'_> {
        assert_eq!(expected.bytes(), [0x11; 32]);
        *self.seen.lock().unwrap() = claim.to_vec();
        Box::pin(async {
            self.result
                .lock()
                .unwrap()
                .take()
                .expect("authorization must be called only once")
        })
    }
    fn list_page(&self, _: Option<InventoryCursor>) -> ManagementFuture<'_, PairingInventoryPage> {
        panic!("unexpected inventory")
    }
    fn assess_unpair(
        &self,
        _: DeviceId,
        _: Option<AssessmentToken>,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        panic!("unexpected unpair assessment")
    }
    fn unpair(&self, _: ConfirmedManagementAction) -> ManagementFuture<'_, UnpairReport> {
        panic!("unexpected unpair")
    }
    fn retry_pending(&self) -> ManagementFuture<'_, Vec<UnpairReport>> {
        panic!("unexpected recovery")
    }
}
/// An external authorization owner admits its own request, then remains pending.
/// Cancellation must drop this wait without calling either authorization again.
struct PausedAuthorization {
    request: Option<ManagementRequestReference>,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    delivery: Mutex<Option<PairingAuthorizationDelivery>>,
    calls: AtomicUsize,
    first_failure: Mutex<Option<PairingAuthorizationFailure>>,
}
impl PausedAuthorization {
    fn new(request: Option<ManagementRequestReference>) -> Self {
        Self {
            request,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            delivery: Mutex::new(None),
            calls: AtomicUsize::new(0),
            first_failure: Mutex::new(None),
        }
    }
}
impl PairingCommandBindings for PausedAuthorization {
    fn enroll<'a>(
        &'a self,
        _: Option<spaceterm_client_native::cli::arguments::PublicFingerprint>,
        _: bool,
        _: SudoMode,
        _: &'a mut dyn CliIo,
    ) -> ManagementFuture<'a, CliOutput> {
        panic!("claim interruption must not enroll")
    }
    fn authorize(
        &self,
        _: spaceterm_client_native::cli::arguments::PublicRequestId,
        _: spaceterm_client_native::cli::arguments::PublicFingerprint,
    ) -> ManagementFuture<'_, ManagementProgress> {
        panic!("claim interruption must not reauthorize a retained request")
    }
    fn authorize_claim(
        &self,
        _: Zeroizing<Vec<u8>>,
        _: spaceterm_client_native::cli::arguments::PublicFingerprint,
    ) -> PairingAuthorizationFuture<'_> {
        panic!("claim dispatch must use the admission-aware interface")
    }
    fn authorize_claim_admitted(
        &self,
        claim: Zeroizing<Vec<u8>>,
        expected: spaceterm_client_native::cli::arguments::PublicFingerprint,
        admission: PairingAuthorizationAdmission,
    ) -> PairingAuthorizationFuture<'_> {
        Box::pin(async move {
            assert_eq!(claim.as_slice(), b"{\"public_claim\":[1,2,3]}\n");
            assert_eq!(expected.bytes(), [0x11; 32]);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(failure) = self.first_failure.lock().unwrap().take() {
                admission.record(*failure.request.as_deref().unwrap());
                return Err(failure);
            }
            if let Some(request) = self.request {
                admission.record(request);
            }
            self.entered.notify_one();
            let delivery = self.delivery.lock().unwrap().take();
            if let Some(delivery) = delivery {
                self.release.notified().await;
                Ok(delivery)
            } else {
                std::future::pending().await
            }
        })
    }
    fn list_page(&self, _: Option<InventoryCursor>) -> ManagementFuture<'_, PairingInventoryPage> {
        panic!("claim interruption must not list devices")
    }
    fn assess_unpair(
        &self,
        _: DeviceId,
        _: Option<AssessmentToken>,
    ) -> ManagementFuture<'_, ManagementAssessment> {
        panic!("claim interruption must not assess unpair")
    }
    fn unpair(&self, _: ConfirmedManagementAction) -> ManagementFuture<'_, UnpairReport> {
        panic!("claim interruption must not unpair")
    }
    fn retry_pending(&self) -> ManagementFuture<'_, Vec<UnpairReport>> {
        panic!("claim interruption must not retry authorization")
    }
}

#[tokio::test]
async fn admitted_claim_interruption_preserves_request_without_delivery_or_reauthorization() {
    let request = ManagementRequestReference::Native([0x27; 16]);
    let pairing = Arc::new(PausedAuthorization::new(Some(request)));
    let deps = dependencies(pairing.clone());
    let signal = CliSignalContext::new(raw_invocation());
    let admission = signal.pairing_authorization_admission();
    let mut io = Io {
        input: Some(Zeroizing::new(b"{\"public_claim\":[1,2,3]}\n".to_vec())),
        ..Io::default()
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        dispatch_until_with_context(&signal, &deps, &mut io, pairing.entered.notified()),
    )
    .await
    .unwrap();
    assert!(matches!(result, Dispatch::Exit(exit) if exit.code() == 75));
    assert_eq!(admission.request(), Some(request));
    assert_eq!(pairing.calls.load(Ordering::SeqCst), 1);
    assert!(io.stdout.is_empty());
    assert!(io.delivery.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&io.stderr).unwrap(),
        serde_json::json!({
            "schema": 1,
            "error": "outcome_pending",
            "request": {"native_request": "27272727272727272727272727272727"}
        })
    );
}

#[tokio::test]
async fn reused_dependencies_report_only_the_second_invocations_admitted_request() {
    let first_request = ManagementRequestReference::Native([0x26; 16]);
    let second_request = ManagementRequestReference::Native([0x27; 16]);
    let pairing = Arc::new(PausedAuthorization::new(Some(second_request)));
    *pairing.first_failure.lock().unwrap() = Some(PairingAuthorizationFailure::new(
        CliFailure::OutcomePending,
        Some(first_request),
    ));
    let deps = dependencies(pairing.clone());
    let mut first_io = Io {
        input: Some(Zeroizing::new(b"{\"public_claim\":[1,2,3]}\n".to_vec())),
        ..Io::default()
    };
    let first = dispatch(raw_invocation(), &deps, &mut first_io).await;
    assert!(matches!(first, Dispatch::Exit(exit) if exit.code() == 75));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&first_io.stderr).unwrap()["request"],
        serde_json::json!({"native_request": "26262626262626262626262626262626"})
    );
    let mut second_io = Io {
        input: Some(Zeroizing::new(b"{\"public_claim\":[1,2,3]}\n".to_vec())),
        ..Io::default()
    };
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        dispatch_until(
            raw_invocation(),
            &deps,
            &mut second_io,
            pairing.entered.notified(),
        ),
    )
    .await
    .unwrap();
    assert!(matches!(second, Dispatch::Exit(exit) if exit.code() == 75));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&second_io.stderr).unwrap(),
        serde_json::json!({
            "schema": 1,
            "error": "outcome_pending",
            "request": {"native_request": "27272727272727272727272727272727"}
        })
    );
    assert!(second_io.delivery.is_empty());
    assert!(second_io.stdout.is_empty());
    assert_eq!(pairing.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn reused_dependencies_deliver_both_successful_authorizations() {
    let pairing = Arc::new(ImmediateAuthorization {
        seen: Mutex::new(Vec::new()),
        result: Mutex::new(None),
    });
    let deps = dependencies(pairing.clone());
    for request in [[0x26; 16], [0x27; 16]] {
        let (delivery, bytes) = authorization_delivery_fixture(request);
        *pairing.result.lock().unwrap() = Some(Ok(delivery));
        let mut io = Io {
            input: Some(Zeroizing::new(b"{\"public_claim\":[1,2,3]}\n".to_vec())),
            ..Io::default()
        };
        let result = dispatch(raw_invocation(), &deps, &mut io).await;
        assert!(matches!(result, Dispatch::Exit(exit) if exit.code() == 0));
        assert!(
            io.delivery.as_slice() == bytes.as_slice(),
            "delivery changed"
        );
        assert!(io.stdout.is_empty());
        assert!(io.stderr.is_empty());
    }
}

#[tokio::test]
async fn pre_admission_claim_interruption_does_not_invent_a_request() {
    let pairing = Arc::new(PausedAuthorization::new(None));
    let deps = dependencies(pairing.clone());
    let signal = CliSignalContext::new(raw_invocation());
    let mut io = Io {
        input: Some(Zeroizing::new(b"{\"public_claim\":[1,2,3]}\n".to_vec())),
        ..Io::default()
    };
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        dispatch_until_with_context(&signal, &deps, &mut io, pairing.entered.notified()),
    )
    .await
    .unwrap();
    assert!(matches!(result, Dispatch::Exit(exit) if exit.code() == 75));
    assert_eq!(signal.pairing_authorization_admission().request(), None);
    assert_eq!(pairing.calls.load(Ordering::SeqCst), 1);
    assert!(io.stdout.is_empty());
    assert!(io.delivery.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&io.stderr).unwrap(),
        serde_json::json!({"schema": 1, "error": "outcome_pending"})
    );
}

#[test]
fn admission_clones_preserve_the_first_request_when_a_later_record_disagrees() {
    let admission = PairingAuthorizationAdmission::default();
    let clone = admission.clone();
    let original = ManagementRequestReference::Native([0x27; 16]);
    admission.record(original);
    clone.record(ManagementRequestReference::Native([0x28; 16]));
    assert_eq!(admission.request(), Some(original));
    assert_eq!(clone.request(), Some(original));
}

#[test]
fn independent_interruption_emits_only_public_operation_id_and_keeps_other_envelopes() {
    use spaceterm_core::{
        identity::{ClientId, OperationStreamId, OperationStreamKey, PairingId, ServerId},
        operations::{OperationBinding, OperationId, OperationSequence, RequestDigest},
    };

    let server = ServerId::new([0x31; 16]);
    let operation = OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(server, 4).unwrap(),
            PairingId::new(server, 2).unwrap(),
            ClientId::new(server, 3).unwrap(),
        )
        .unwrap(),
        OperationSequence::new(5).unwrap(),
    );
    for request in [
        ManagementRequestReference::Operation(OperationBinding {
            id: operation,
            digest: RequestDigest::from_bytes([0x39; 32]),
        }),
        ManagementRequestReference::OperationReference(operation),
    ] {
        let signal = CliSignalContext::new(raw_invocation());
        signal.pairing_authorization_admission().record(request);
        let mut signal_io = Io::default();
        assert_eq!(
            signal.emit_interruption(CliFailure::OutcomePending, &mut signal_io),
            Ok(true)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&signal_io.stderr).unwrap(),
            serde_json::json!({
                "schema": 1,
                "error": "outcome_pending",
                "request": {"operation": "31313131313131313131313131313131:2:3:4:5"}
            })
        );
        assert!(signal_io.stdout.is_empty());
        assert!(signal_io.delivery.is_empty());

        let mut ordinary_io = Io::default();
        let ordinary_signal = CliSignalContext::new(invocation(&["--json", "start"]).unwrap());
        assert_eq!(
            ordinary_signal.emit_interruption(CliFailure::OutcomePending, &mut ordinary_io,),
            Ok(true)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&ordinary_io.stderr).unwrap(),
            serde_json::json!({"schema": 1, "error": "outcome_pending"})
        );
    }
}

fn dependencies(pairing: Arc<dyn PairingCommandBindings>) -> CliDependencies {
    CliDependencies::new(
        Ok(AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        }),
        Arc::new(UnavailableProtocolManagement),
        Arc::new(UnavailableNativeBindings::default()),
        pairing,
    )
}
fn raw_invocation() -> spaceterm_client_native::cli::CliInvocation {
    invocation(&[
        "--json",
        "--non-interactive",
        "pair",
        "authorize",
        "--claim-stdin",
        "--authorization-stdout",
        "--device-fingerprint",
        FINGERPRINT,
    ])
    .unwrap()
}

#[tokio::test]
async fn claim_dispatch_preserves_input_and_original_request_in_typed_failures() {
    // The producer owns decoding. The CLI must pass its supplied JSON unchanged,
    // without adding retained credential references or another serialization.
    let public_json = b"{\"public_claim\":[1,2,3]}\n";
    let pairing = Arc::new(ImmediateAuthorization {
        seen: Mutex::new(Vec::new()),
        result: Mutex::new(Some(Err(PairingAuthorizationFailure::new(
            CliFailure::OutcomePending,
            Some(ManagementRequestReference::Native([7; 16])),
        )))),
    });
    let mut io = Io {
        input: Some(Zeroizing::new(public_json.to_vec())),
        ..Io::default()
    };
    let result = dispatch(raw_invocation(), &dependencies(pairing.clone()), &mut io).await;
    assert!(matches!(result, Dispatch::Exit(exit) if exit.code() == 75));
    assert_eq!(*pairing.seen.lock().unwrap(), public_json);
    assert!(io.stdout.is_empty());
    assert!(io.delivery.is_empty());
    let failure: serde_json::Value = serde_json::from_slice(&io.stderr).unwrap();
    assert_eq!(
        failure,
        serde_json::json!({"schema":1,"error":"outcome_pending","request":{"native_request":"07070707070707070707070707070707"}})
    );
    assert!(!String::from_utf8_lossy(&io.stderr).contains("public_claim"));
}

fn authorization_delivery_fixture(
    request: [u8; 16],
) -> (PairingAuthorizationDelivery, Zeroizing<Vec<u8>>) {
    use spaceterm_core::{
        authority::{ServerEpoch, StoreRevision},
        identity::{ClientId, OperationStreamId, OperationStreamKey, PairingId, ServerId},
        operations::RequestDigest,
        pairing::{
            EnrollmentIssuer, EnrollmentReservation, EnrollmentReservationParts,
            EnrollmentReservationState, EnrollmentTicketId, EnrollmentTicketParts, PairingKeys,
            PairingKeysParts, PairingTarget, PairingTargetParts,
        },
    };
    use spaceterm_protocol::pairing_adapter::EnrollmentAuthorization;

    let server = ServerId::new([0x31; 16]);
    let pairing = PairingId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    let authorization = EnrollmentAuthorization::generate().unwrap();
    let reservation = EnrollmentReservation::new(EnrollmentReservationParts {
        ticket: EnrollmentTicketId::new(EnrollmentTicketParts {
            server,
            nonce: [0x32; 16],
        })
        .unwrap(),
        target: PairingTarget::new(PairingTargetParts {
            server,
            device: DeviceId::new([0x33; 16]),
            pairing,
        })
        .unwrap(),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [0x34; 32],
            pairing_authentication_public_key: [0x35; 32],
        })
        .unwrap(),
        initial_client: client,
        enrollment_stream: OperationStreamId::new(
            OperationStreamKey::new(server, 1).unwrap(),
            pairing,
            client,
        )
        .unwrap(),
        claim_digest: RequestDigest::from_bytes([0x36; 32]),
        authorization_digest: authorization.digest(),
        enrollment_request_digest: RequestDigest::from_bytes([0x37; 32]),
        issuer: EnrollmentIssuer::AccountOwner {
            native_request: request,
            native_request_digest: RequestDigest::from_bytes([0x38; 32]),
        },
        issued_epoch: ServerEpoch::new(1),
        issued_revision: StoreRevision::new(1),
        issued_at_millis: 0,
        expires_at_millis: 100,
        state: EnrollmentReservationState::Reserved,
    })
    .unwrap();
    let bytes = authorization.delivery_message(&reservation).unwrap();
    let delivery = PairingAuthorizationDelivery::new(
        ManagementRequestReference::Native(request),
        Zeroizing::new(bytes.to_vec()),
    )
    .unwrap_or_else(|_| panic!("valid protocol delivery rejected"));
    (delivery, bytes)
}

#[tokio::test]
async fn independent_admitted_interrupt_prevents_a_later_delivery_without_reauthorization() {
    let request = ManagementRequestReference::Native([0x27; 16]);
    let (delivery, _bytes) = authorization_delivery_fixture([0x27; 16]);
    let pairing = Arc::new(PausedAuthorization::new(Some(request)));
    *pairing.delivery.lock().unwrap() = Some(delivery);
    let deps = dependencies(pairing.clone());
    let signal = CliSignalContext::new(raw_invocation());
    let mut io = Io {
        input: Some(Zeroizing::new(b"{\"public_claim\":[1,2,3]}\n".to_vec())),
        ..Io::default()
    };
    let mut signal_io = Io::default();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(dispatch_with_context(&signal, &deps, &mut io), async {
            pairing.entered.notified().await;
            assert_eq!(
                signal.emit_interruption(CliFailure::OutcomePending, &mut signal_io),
                Ok(true)
            );
            pairing.release.notify_one();
        },)
    })
    .await
    .unwrap();
    assert!(matches!(result, Dispatch::Exit(exit) if exit.code() == 75));
    assert_eq!(pairing.calls.load(Ordering::SeqCst), 1);
    assert!(io.stdout.is_empty());
    assert!(io.delivery.is_empty());
    assert!(io.stderr.is_empty());
    assert!(signal_io.stdout.is_empty());
    assert!(signal_io.delivery.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&signal_io.stderr).unwrap(),
        serde_json::json!({
            "schema": 1,
            "error": "outcome_pending",
            "request": {"native_request": "27272727272727272727272727272727"}
        })
    );
}

#[tokio::test]
async fn successful_raw_delivery_suppresses_late_independent_interrupt_output() {
    let request = ManagementRequestReference::Native([0x27; 16]);
    let (delivery, bytes) = authorization_delivery_fixture([0x27; 16]);
    // This provider implements only the original authorize_claim interface.
    // The compatibility default observes its completed delivery's request.
    let pairing = Arc::new(ImmediateAuthorization {
        seen: Mutex::new(Vec::new()),
        result: Mutex::new(Some(Ok(delivery))),
    });
    let deps = dependencies(pairing);
    let signal = CliSignalContext::new(raw_invocation());
    let mut io = Io {
        input: Some(Zeroizing::new(b"{\"public_claim\":[1,2,3]}\n".to_vec())),
        ..Io::default()
    };
    let result = dispatch_until_with_context(&signal, &deps, &mut io, std::future::pending()).await;
    assert!(matches!(result, Dispatch::Exit(exit) if exit.code() == 0));
    assert_eq!(
        signal.pairing_authorization_admission().request(),
        Some(request)
    );
    assert!(
        io.delivery.as_slice() == bytes.as_slice(),
        "raw delivery changed"
    );
    assert!(io.stdout.is_empty());
    assert!(io.stderr.is_empty());

    let mut signal_io = Io::default();
    assert_eq!(
        signal.emit_interruption(CliFailure::OutcomePending, &mut signal_io),
        Ok(false)
    );
    assert!(signal_io.stdout.is_empty());
    assert!(signal_io.delivery.is_empty());
    assert!(signal_io.stderr.is_empty());
}

#[tokio::test]
async fn missing_private_io_and_missing_producer_fail_closed_without_normal_stdout() {
    let deps = dependencies(Arc::new(UnavailablePairingBindings));
    for input in [None, Some(Zeroizing::new(b"{}".to_vec()))] {
        let mut io = Io {
            input,
            ..Io::default()
        };
        let result = dispatch(raw_invocation(), &deps, &mut io).await;
        assert!(matches!(result, Dispatch::Exit(exit) if exit.code() == 69));
        assert!(io.stdout.is_empty());
        assert!(io.delivery.is_empty());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&io.stderr).unwrap(),
            serde_json::json!({"schema":1,"error":"authorization_unavailable"})
        );
    }
}

#[test]
fn delivery_binding_rejects_malformed_or_oversized_secret_without_exposing_it() {
    let request = ManagementRequestReference::Native([8; 16]);
    for (bytes, expected) in [
        (
            Zeroizing::new(b"secret-invalid-protocol-delivery".to_vec()),
            CliFailure::UnsafeNativeState,
        ),
        (
            Zeroizing::new(vec![42; MAX_AUTHORIZATION_DELIVERY_BYTES + 1]),
            CliFailure::LimitExceeded,
        ),
    ] {
        let failure = PairingAuthorizationDelivery::new(request, bytes)
            .err()
            .unwrap();
        assert_eq!(
            failure,
            PairingAuthorizationFailure::new(expected, Some(request))
        );
        assert!(!format!("{failure:?}").contains("secret-invalid-protocol-delivery"));
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use pairing_pipe::{PipeDirection, verify_private_pipe};
    use std::{
        fs::{File, OpenOptions},
        os::{
            fd::{AsFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
    };

    // Darwin has no pipe2(O_CLOEXEC). Do not let this suite's subprocess spawn
    // between pipe creation and setting CLOEXEC on either endpoint.
    static PIPE_DESCRIPTORS: Mutex<()> = Mutex::new(());

    fn pipe() -> (File, File) {
        let _creation = PIPE_DESCRIPTORS.lock().unwrap();
        let mut fds = [-1; 2];
        // SAFETY: pipe writes two descriptors into the supplied array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        for fd in fds {
            // SAFETY: pipe created both live descriptors owned by this fixture.
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        // SAFETY: the two successful pipe descriptors each transfer one owner.
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    struct PrivateChild(std::process::Child);
    impl Drop for PrivateChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    const READY_FD: libc::c_int = 100;
    struct SignalPipeIo {
        readiness: File,
    }
    impl SignalPipeIo {
        fn report_ready(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
            self.readiness
                .write_all(bytes)
                .map_err(|_| CliFailure::IoUnavailable)
        }
    }
    impl CliIo for SignalPipeIo {
        fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
            spaceterm_client_native::cli::tty::TerminalIo.write_stdout(bytes)
        }
        fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
            spaceterm_client_native::cli::tty::TerminalIo.write_stderr(bytes)
        }
        fn read_authorization_claim(&mut self) -> Result<Zeroizing<Vec<u8>>, CliFailure> {
            spaceterm_client_native::cli::tty::TerminalIo.read_authorization_claim()
        }
        fn write_authorization_delivery_async<'a>(
            &'a mut self,
            bytes: &'a [u8],
            signal: &'a CliSignalContext,
        ) -> ManagementFuture<'a, ()> {
            Box::pin(async move {
                let mut terminal = spaceterm_client_native::cli::tty::TerminalIo;
                let mut writing = terminal.write_authorization_delivery_async(bytes, signal);
                let mut reported = false;
                std::future::poll_fn(|cx| {
                    let result = writing.as_mut().poll(cx);
                    if result.is_pending() && !reported {
                        // Report only after the real verified AsyncFd writer has
                        // yielded to actual OS backpressure, not before polling.
                        reported = true;
                        if let Err(failure) = self.report_ready(b"{\"schema\":1,\"request\":{\"native_request\":\"27272727272727272727272727272727\"}}\n") {
                            return std::task::Poll::Ready(Err(failure));
                        }
                    }
                    result
                }).await
            })
        }
        fn read_confirmation(
            &mut self,
            _: &ConfirmationPrompt,
        ) -> Result<ConfirmationReply, CliFailure> {
            panic!("private pipe fixture must not prompt")
        }
    }

    #[test]
    fn private_full_pipe_signal_child() {
        let Ok(scenario) = std::env::var("SPACETERM_CLI_PIPE_FIXTURE") else {
            // The normal harness exercises every scenario. Child invocations
            // receive the explicit scenario and run only the owned fixture.
            for scenario in [
                "full",
                "aliased",
                "provider-failure",
                "regular-stderr",
                "success",
            ] {
                private_backpressure_child(scenario);
            }
            return;
        };
        assert!(matches!(
            scenario.as_str(),
            "full" | "aliased" | "provider-failure" | "regular-stderr" | "success"
        ));
        // SAFETY: the parent installs this exclusively owned readiness descriptor
        // only in the fixture child. It carries bounded public metadata only.
        let readiness = unsafe { File::from_raw_fd(READY_FD) };
        let mut io = SignalPipeIo { readiness };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let code = runtime.block_on(async {
            let context = CliSignalContext::new(raw_invocation());
            if matches!(scenario.as_str(), "provider-failure" | "regular-stderr") {
                io.report_ready(b"{\"schema\":1,\"fixture_ready\":true}\n")
                    .unwrap();
                // The public claim is only a private parent/child start barrier.
                // Provider failure is injected; no native service is opened.
                io.read_authorization_claim().unwrap();
                let failure = CliFailure::NativeUnavailable;
                let emitted = context.emit_initial_failure(
                    failure,
                    &mut spaceterm_client_native::cli::tty::TerminalIo,
                );
                if scenario == "regular-stderr" {
                    assert_eq!(emitted, Ok(()));
                    // Regular files must use the deadline worker, not status
                    // flags that cannot make their writes nonblocking.
                    let flags = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_GETFL) };
                    assert!(flags >= 0);
                    assert_eq!(flags & libc::O_NONBLOCK, 0);
                }
                return failure.exit_code();
            }
            let (delivery, _) = authorization_delivery_fixture([0x27; 16]);
            let deps = Arc::new(dependencies(Arc::new(ImmediateAuthorization {
                seen: Mutex::new(Vec::new()),
                result: Mutex::new(Some(Ok(delivery))),
            })));
            if scenario == "success" {
                // Model a handler whose cancelled task has not yet been dropped.
                // This owned child exits before the extra context owner releases.
                let deferred = context.clone();
                std::thread::spawn(move || {
                    let _deferred = deferred;
                    std::thread::sleep(Duration::from_secs(10));
                });
            }
            // Install the handler before the parent observes readiness.
            let mut signal =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
            let signal_context = context.clone();
            let interrupt = tokio::spawn(async move {
                signal.recv().await;
                if signal_context.emit_interruption(
                    CliFailure::OutcomePending,
                    &mut spaceterm_client_native::cli::tty::TerminalIo,
                ) != Ok(false)
                {
                    std::process::exit(75);
                }
            });
            io.report_ready(b"{\"schema\":1,\"fixture_ready\":true}\n")
                .unwrap();
            let result = dispatch_with_context(&context, &deps, &mut io).await;
            interrupt.abort();
            match result {
                Dispatch::Exit(exit) => exit.code(),
                Dispatch::Serve => panic!("fixture must not serve"),
            }
        });
        std::process::exit(i32::from(code));
    }

    fn set_nonblocking(file: &File) -> libc::c_int {
        use std::os::fd::AsRawFd;
        // SAFETY: both fcntl calls operate on a live owned descriptor.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        flags
    }

    fn prefill(writer: &mut File) -> usize {
        use std::os::fd::AsRawFd;
        let flags = set_nonblocking(writer);
        let mut count = 0;
        // Fill down to single bytes so even the first opaque byte cannot fit.
        for size in [4096, 1] {
            let filler = vec![b'x'; size];
            loop {
                match writer.write(&filler) {
                    Ok(written) if written > 0 => count += written,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    result => panic!("prefill failed: {result:?}"),
                }
            }
        }
        // SAFETY: restore the owned descriptor's original blocking flags.
        assert_eq!(
            unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETFL, flags) },
            0
        );
        count
    }

    struct OwnedPipeIo {
        io: Io,
        output: File,
    }
    impl CliIo for OwnedPipeIo {
        fn write_stdout(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
            self.io.write_stdout(bytes)
        }
        fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
            self.io.write_stderr(bytes)
        }
        fn read_authorization_claim(&mut self) -> Result<Zeroizing<Vec<u8>>, CliFailure> {
            self.io.read_authorization_claim()
        }
        fn write_authorization_delivery_async<'a>(
            &'a mut self,
            bytes: &'a [u8],
            signal: &'a CliSignalContext,
        ) -> ManagementFuture<'a, ()> {
            Box::pin(pairing_pipe::write_private_authorization_delivery(
                self.output.as_fd(),
                bytes,
                signal,
            ))
        }
        fn read_confirmation(
            &mut self,
            _: &ConfirmationPrompt,
        ) -> Result<ConfirmationReply, CliFailure> {
            panic!("private output must not prompt")
        }
    }

    struct HeldDiagnostic {
        entered: Arc<tokio::sync::Notify>,
        release: std::sync::mpsc::Receiver<()>,
        bytes: Arc<Mutex<Vec<u8>>>,
    }
    impl CliIo for HeldDiagnostic {
        fn write_stdout(&mut self, _: &[u8]) -> Result<(), CliFailure> {
            panic!("signal must not write stdout")
        }
        fn write_stderr(&mut self, bytes: &[u8]) -> Result<(), CliFailure> {
            self.entered.notify_one();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn read_confirmation(
            &mut self,
            _: &ConfirmationPrompt,
        ) -> Result<ConfirmationReply, CliFailure> {
            panic!("signal must not prompt")
        }
    }

    #[tokio::test]
    async fn interrupted_dispatch_waits_for_the_original_request_diagnostic_before_returning() {
        use std::future::Future;
        let (_reader, mut writer) = pipe();
        prefill(&mut writer);
        let (delivery, _) = authorization_delivery_fixture([0x27; 16]);
        let deps = dependencies(Arc::new(ImmediateAuthorization {
            seen: Mutex::new(Vec::new()),
            result: Mutex::new(Some(Ok(delivery))),
        }));
        let signal = CliSignalContext::new(raw_invocation());
        let mut io = OwnedPipeIo {
            output: writer,
            io: Io {
                input: Some(Zeroizing::new(b"{\"public_claim\":[1,2,3]}\n".to_vec())),
                ..Io::default()
            },
        };
        let waiting = dispatch_with_context(&signal, &deps, &mut io);
        tokio::pin!(waiting);
        std::future::poll_fn(|cx| {
            assert!(waiting.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let (release, held) = std::sync::mpsc::channel();
        let emitter = {
            let signal = signal.clone();
            let mut io = HeldDiagnostic {
                entered: entered.clone(),
                release: held,
                bytes: diagnostics.clone(),
            };
            std::thread::spawn(move || {
                signal.emit_interruption(CliFailure::OutcomePending, &mut io)
            })
        };
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        let early = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(match waiting.as_mut().poll(cx) {
                std::task::Poll::Ready(result) => Some(result),
                std::task::Poll::Pending => None,
            })
        })
        .await;
        release.send(()).unwrap();
        assert_eq!(emitter.join().unwrap(), Ok(true));
        assert!(
            early.is_none(),
            "dispatch returned before the interruption diagnostic completed"
        );
        let result = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap();
        assert!(matches!(result, Dispatch::Exit(exit) if exit.code() == 75));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&diagnostics.lock().unwrap()).unwrap(),
            serde_json::json!({
                "schema": 1,
                "error": "outcome_pending",
                "request": {"native_request": "27272727272727272727272727272727"}
            })
        );
    }

    #[tokio::test]
    async fn cancelling_raw_writer_keeps_shared_endpoint_nonblocking_until_invocation_finishes() {
        use std::{future::Future, os::fd::AsRawFd};
        let (_reader, mut writer) = pipe();
        prefill(&mut writer);
        let alias = writer.try_clone().unwrap();
        let (_delivery, bytes) = authorization_delivery_fixture([0x27; 16]);
        let signal = CliSignalContext::new(raw_invocation());
        {
            let writing =
                pairing_pipe::write_private_authorization_delivery(writer.as_fd(), &bytes, &signal);
            tokio::pin!(writing);
            std::future::poll_fn(|cx| {
                assert!(writing.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        // stderr may be dup2'ed from stdout. Cancelling stdout's AsyncFd must
        // not restore blocking flags while the signal diagnostic still owns it.
        let flags = unsafe { libc::fcntl(alias.as_raw_fd(), libc::F_GETFL) };
        assert!(
            flags & libc::O_NONBLOCK != 0,
            "writer cancellation restored blocking flags before diagnostic completion"
        );
        drop(signal);
        let restored = unsafe { libc::fcntl(alias.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(restored & libc::O_NONBLOCK, 0);
    }

    #[tokio::test]
    async fn verified_async_delivery_preserves_bytes_and_suppresses_late_interrupt() {
        let (mut reader, writer) = pipe();
        let (delivery, bytes) = authorization_delivery_fixture([0x27; 16]);
        let signal = CliSignalContext::new(raw_invocation());
        signal
            .pairing_authorization_admission()
            .record(delivery.request());
        pairing_pipe::write_private_authorization_delivery(writer.as_fd(), &bytes, &signal)
            .await
            .unwrap();
        let mut signal_io = Io::default();
        assert_eq!(
            signal.emit_interruption(CliFailure::OutcomePending, &mut signal_io),
            Ok(false)
        );
        assert!(signal_io.stderr.is_empty());
        assert!(signal_io.stdout.is_empty());
        drop(signal);
        drop(writer);
        let mut received = Zeroizing::new(Vec::new());
        reader.read_to_end(&mut received).unwrap();
        assert!(
            received.as_slice() == bytes.as_slice(),
            "opaque delivery changed"
        );
    }

    #[tokio::test]
    async fn interrupted_backpressured_writer_cannot_deliver_after_reader_drains() {
        let (mut reader, mut writer) = pipe();
        let prefilled = prefill(&mut writer);
        let (delivery, bytes) = authorization_delivery_fixture([0x27; 16]);
        let signal = CliSignalContext::new(raw_invocation());
        signal
            .pairing_authorization_admission()
            .record(delivery.request());
        let mut signal_io = Io::default();
        let mut received = Zeroizing::new(Vec::new());
        {
            let writing =
                pairing_pipe::write_private_authorization_delivery(writer.as_fd(), &bytes, &signal);
            tokio::pin!(writing);
            std::future::poll_fn(|cx| {
                use std::future::Future;
                assert!(
                    writing.as_mut().poll(cx).is_pending(),
                    "full OS pipe did not suspend delivery"
                );
                std::task::Poll::Ready(())
            })
            .await;
            assert_eq!(
                signal.emit_interruption(CliFailure::OutcomePending, &mut signal_io),
                Ok(true)
            );
            // Reopen writability only after interruption has selected the diagnostic.
            set_nonblocking(&reader);
            assert_eq!(
                reader.read_to_end(&mut received).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
            assert_eq!(received.len(), prefilled);
            assert_eq!(writing.await, Err(CliFailure::OutcomePending));
        }
        drop(signal);
        drop(writer);
        read_closed_pipe(&mut reader, &mut received);
        assert_eq!(received.len(), prefilled);
        assert!(
            received.iter().all(|&byte| byte == b'x'),
            "delivery followed interruption"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&signal_io.stderr).unwrap(),
            serde_json::json!({
                "schema": 1,
                "error": "outcome_pending",
                "request": {"native_request": "27272727272727272727272727272727"}
            })
        );
    }

    #[tokio::test]
    async fn undrained_private_pipe_delivery_has_a_total_write_deadline() {
        let (_reader, mut writer) = pipe();
        prefill(&mut writer);
        let (_delivery, bytes) = authorization_delivery_fixture([0x27; 16]);
        let signal = CliSignalContext::new(raw_invocation());
        let result = tokio::time::timeout(
            pairing_pipe::AUTHORIZATION_WRITE_DEADLINE + Duration::from_secs(2),
            pairing_pipe::write_private_authorization_delivery(writer.as_fd(), &bytes, &signal),
        )
        .await;
        assert_eq!(result.unwrap(), Err(CliFailure::IoUnavailable));
    }

    #[test]
    fn full_private_stdout_pipe_interrupt_exits_with_original_request_without_delivery() {
        private_backpressure_child("full");
    }

    #[test]
    fn full_aliased_stdout_stderr_pipe_interrupt_still_exits_without_delivery() {
        private_backpressure_child("aliased");
    }

    #[test]
    fn raw_provider_failure_exits_when_its_stderr_pipe_is_full() {
        private_backpressure_child("provider-failure");
    }

    #[test]
    fn raw_regular_file_stderr_keeps_flags_and_exact_typed_envelope() {
        private_backpressure_child("regular-stderr");
    }

    #[test]
    fn successful_pipe_exit_restores_flags_before_deferred_signal_context_drop() {
        private_backpressure_child("success");
    }

    fn private_backpressure_child(scenario: &str) {
        use std::{
            os::{fd::AsRawFd, unix::process::CommandExt},
            process::{Command, Stdio},
            time::Instant,
        };
        let aliased = matches!(scenario, "aliased" | "provider-failure");
        let mut regular_stderr =
            (scenario == "regular-stderr").then(|| tempfile::tempfile().unwrap());
        let (mut output_reader, mut output_writer) = pipe();
        verify_private_pipe(output_writer.as_fd(), PipeDirection::Write).unwrap();
        let (claim_reader, mut claim_writer) = pipe();
        let (mut readiness_reader, readiness_writer) = pipe();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "unix::private_full_pipe_signal_child",
                "--nocapture",
            ])
            .env("SPACETERM_CLI_PIPE_FIXTURE", scenario)
            .stdin(Stdio::from(claim_reader))
            .stdout(Stdio::from(output_writer.try_clone().unwrap()))
            .stderr(if let Some(ref file) = regular_stderr {
                Stdio::from(file.try_clone().unwrap())
            } else if aliased {
                Stdio::from(output_writer.try_clone().unwrap())
            } else {
                Stdio::piped()
            });
        let ready_raw = readiness_writer.as_raw_fd();
        // SAFETY: the child hook uses only async-signal-safe descriptor syscalls.
        // All descriptors and the process belong to this private fixture.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(ready_raw, READY_FD) < 0
                    || libc::fcntl(READY_FD, libc::F_SETFD, 0) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let spawning = PIPE_DESCRIPTORS.lock().unwrap();
        let mut child = PrivateChild(command.spawn().unwrap());
        drop(command);
        drop(spawning);
        drop(readiness_writer);
        let mut stderr = child
            .0
            .stderr
            .take()
            .map(|stderr| File::from(stderr.as_fd().try_clone_to_owned().unwrap()));
        set_nonblocking(&readiness_reader);
        let mut readiness = Vec::new();
        let ready_deadline = Instant::now() + Duration::from_secs(5);
        wait_readiness(&mut readiness_reader, &mut readiness, 1, ready_deadline);
        // Libtest's heading precedes the fixture. Drain it, then keep stdout
        // completely full and undrained until the owned child has terminated.
        set_nonblocking(&output_reader);
        let mut heading = Vec::new();
        assert_eq!(
            output_reader.read_to_end(&mut heading).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let prefilled = if scenario == "success" {
            0
        } else {
            prefill(&mut output_writer)
        };
        let retained_writer = output_writer.try_clone().unwrap();
        drop(output_writer);
        claim_writer
            .write_all(b"{\"public_claim\":[1,2,3]}\n")
            .unwrap();
        drop(claim_writer);
        if !matches!(scenario, "provider-failure" | "regular-stderr" | "success") {
            // The second marker follows an actual Pending poll of AsyncFd.
            wait_readiness(&mut readiness_reader, &mut readiness, 2, ready_deadline);
            // SIGINT targets this fixture child, never a native service.
            assert_eq!(
                unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGINT) },
                0
            );
        }
        let exit_deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < exit_deadline,
                "private pipe backpressure prevented bounded child exit"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(
            status.code(),
            Some(if scenario == "success" {
                0
            } else if matches!(scenario, "provider-failure" | "regular-stderr") {
                69
            } else {
                75
            })
        );
        // The shell or another parent writer may retain the same open-file
        // description. Even abrupt SIGINT exit must restore its original flags.
        let flags = unsafe { libc::fcntl(retained_writer.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            flags & libc::O_NONBLOCK,
            0,
            "child exit leaked nonblocking status flags into its parent"
        );
        drop(retained_writer);
        if let Some(ref mut stderr) = stderr {
            let mut diagnostics = Vec::new();
            set_nonblocking(stderr);
            read_closed_pipe(stderr, &mut diagnostics);
            if scenario == "success" {
                assert!(
                    diagnostics.is_empty(),
                    "successful delivery emitted a diagnostic"
                );
            } else {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&diagnostics).unwrap(),
                    serde_json::json!({
                        "schema": 1,
                        "error": "outcome_pending",
                        "request": {"native_request": "27272727272727272727272727272727"}
                    })
                );
            }
        }
        if let Some(ref mut file) = regular_stderr {
            use std::io::Seek;
            file.rewind().unwrap();
            let mut diagnostics = Vec::new();
            file.read_to_end(&mut diagnostics).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&diagnostics).unwrap(),
                serde_json::json!({
                    "schema": 1,
                    "error": "native_unavailable"
                })
            );
        }
        let mut received = Zeroizing::new(Vec::new());
        read_closed_pipe(&mut output_reader, &mut received);
        if scenario == "success" {
            use spaceterm_protocol::pairing_adapter::EnrollmentAuthorization;
            assert!(
                EnrollmentAuthorization::from_delivery(received).is_ok(),
                "opaque successful delivery was changed"
            );
        } else {
            assert_eq!(received.len(), prefilled);
            assert!(
                received.iter().all(|&byte| byte == b'x'),
                "delivery or extra diagnostics followed interruption"
            );
        }
    }

    fn read_closed_pipe(reader: &mut File, bytes: &mut Vec<u8>) {
        // A concurrent private fork can briefly retain a CLOEXEC endpoint before
        // exec closes it. Require EOF within a bound rather than immediately.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let mut buffer = [0; 4096];
            match reader.read(&mut buffer) {
                Ok(0) => return,
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => panic!("owned pipe unavailable"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned writer did not close its pipe"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_readiness(
        reader: &mut File,
        bytes: &mut Vec<u8>,
        lines: usize,
        deadline: std::time::Instant,
    ) {
        while bytes.iter().filter(|&&byte| byte == b'\n').count() < lines {
            let mut buffer = [0; 1024];
            match reader.read(&mut buffer) {
                Ok(0) => panic!("owned child exited before readiness"),
                Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => panic!("owned child readiness unavailable"),
            }
            assert!(
                std::time::Instant::now() < deadline,
                "owned child did not reach the controlled I/O boundary"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn os_verifier_accepts_only_private_anonymous_pipes_in_the_correct_direction() {
        let (read, write) = pipe();
        assert_eq!(
            verify_private_pipe(read.as_fd(), PipeDirection::Read),
            Ok(())
        );
        assert_eq!(
            verify_private_pipe(write.as_fd(), PipeDirection::Write),
            Ok(())
        );
        assert_eq!(
            verify_private_pipe(read.as_fd(), PipeDirection::Write),
            Err(CliFailure::AuthorizationDenied)
        );
        assert_eq!(
            verify_private_pipe(write.as_fd(), PipeDirection::Read),
            Err(CliFailure::AuthorizationDenied)
        );
        let file = tempfile::tempfile().unwrap();
        assert_eq!(
            verify_private_pipe(file.as_fd(), PipeDirection::Write),
            Err(CliFailure::AuthorizationDenied)
        );
        let null = File::open("/dev/null").unwrap();
        assert_eq!(
            verify_private_pipe(null.as_fd(), PipeDirection::Read),
            Err(CliFailure::AuthorizationDenied)
        );
        let (socket, _) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(
            verify_private_pipe(socket.as_fd(), PipeDirection::Write),
            Err(CliFailure::AuthorizationDenied)
        );
    }

    #[test]
    fn os_verifier_rejects_named_and_unlinked_fifos() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("claim");
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: cpath is NUL-terminated and names an isolated test directory.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        use std::os::unix::fs::OpenOptionsExt;
        let read = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let write = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        for unlinked in [false, true] {
            if unlinked {
                std::fs::remove_file(&path).unwrap();
            }
            assert_eq!(
                verify_private_pipe(read.as_fd(), PipeDirection::Read),
                Err(CliFailure::AuthorizationDenied)
            );
            assert_eq!(
                verify_private_pipe(write.as_fd(), PipeDirection::Write),
                Err(CliFailure::AuthorizationDenied)
            );
        }
    }

    #[test]
    fn verified_real_pipes_transfer_exact_bounded_documents_and_binary() {
        let (mut claim_reader, mut claim_writer) = pipe();
        let (mut delivery_reader, mut delivery_writer) = pipe();
        verify_private_pipe(claim_reader.as_fd(), PipeDirection::Read).unwrap();
        verify_private_pipe(delivery_writer.as_fd(), PipeDirection::Write).unwrap();
        let claim = b"{\"public_claim\":[1,2,3]}\n";
        claim_writer.write_all(claim).unwrap();
        drop(claim_writer);
        assert_eq!(
            pairing_pipe::read_public_claim(&mut claim_reader)
                .unwrap()
                .as_slice(),
            claim
        );
        let bytes = Zeroizing::new(vec![0, 0xff, b'\n', b'\r', 0x1b, 0, 1]);
        pairing_pipe::write_authorization_delivery(&mut delivery_writer, &bytes).unwrap();
        drop(delivery_writer);
        let mut received = Zeroizing::new(Vec::new());
        delivery_reader.read_to_end(&mut received).unwrap();
        assert!(
            received.as_slice() == bytes.as_slice(),
            "opaque delivery bytes changed"
        );
    }
}
