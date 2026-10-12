//! Native peer injection replaces authority observations only. STN2, locked
//! memory, pipe ownership, crypto and exact-child retirement remain production code.

use std::{
    io::Write,
    num::NonZeroU64,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use spaceterm_host::{
    boot_credentials::LockedSecret32,
    native_deployment::contracts::{DataGenerationId, NativeFailure},
    native_receiver::{
        NativeReceiver, ReceiverAuthority, ReceiverGroups, ReceiverProtection,
        retriever::{BootCredentialExchange, BootCredentialRelease, RetrieverPipes, exchange},
    },
    startup::{AccountBuildKey, BuildIdentity, InstallationId, PendingStartupContext},
    startup_exchange::{
        ExchangeDeadline, StartupTranscript,
        unix::{ExactChild, LockedMemory, ReceiverPipes, anonymous_pipe, process_identity},
    },
};

#[derive(Default)]
struct Observations {
    authorized: usize,
    retrieved: usize,
    rechecks: usize,
    final_checks: usize,
}
#[derive(Clone, Copy)]
enum Fault {
    None,
    Protection,
    BeforeDelivery,
    FinalAuthority,
    WrongReady,
}

struct Protection(Fault);
impl ReceiverProtection for Protection {
    fn prepare(&self, _transcript: &StartupTranscript) -> Result<ReceiverGroups, NativeFailure> {
        if matches!(self.0, Fault::Protection) {
            Err(NativeFailure::ExecutionBindingUnproven)
        } else {
            ReceiverGroups::observed(vec![501, 20, 80])
        }
    }
}
struct ReleasePeer {
    observations: Arc<Mutex<Observations>>,
    fault: Fault,
}
impl BootCredentialExchange for ReleasePeer {
    fn authorize(
        &self,
        _transcript: &StartupTranscript,
    ) -> Result<Box<dyn BootCredentialRelease + '_>, NativeFailure> {
        self.observations.lock().unwrap().authorized += 1;
        Ok(Box::new(Release { peer: self }))
    }
}
struct Release<'a> {
    peer: &'a ReleasePeer,
}
impl BootCredentialRelease for Release<'_> {
    fn recheck(&self) -> Result<(), NativeFailure> {
        let mut observed = self.peer.observations.lock().unwrap();
        observed.rechecks += 1;
        if matches!(self.peer.fault, Fault::BeforeDelivery) && observed.retrieved == 1 {
            Err(NativeFailure::GenerationMismatch)
        } else {
            Ok(())
        }
    }
    fn retrieve(&self, destination: &mut LockedSecret32) -> Result<(), NativeFailure> {
        self.peer.observations.lock().unwrap().retrieved += 1;
        let mut synthetic = LockedMemory::<32>::allocate()?;
        getrandom::fill(synthetic.bytes_mut()).map_err(|_| NativeFailure::NativeUnavailable)?;
        destination.write(synthetic.bytes())
    }
}
struct FinalPeer {
    observations: Arc<Mutex<Observations>>,
    retriever: u32,
    fault: Fault,
}
impl ReceiverAuthority for FinalPeer {
    fn revalidate(
        &self,
        _transcript: &StartupTranscript,
        groups: &ReceiverGroups,
    ) -> Result<(), NativeFailure> {
        // Native final authority must not be called while the exact child still
        // has a wait reservation, including after a valid acknowledgement.
        assert!(process_identity(self.retriever).is_err());
        assert_eq!(groups.groups(), &[501, 20, 80]);
        self.observations.lock().unwrap().final_checks += 1;
        if matches!(self.fault, Fault::FinalAuthority) {
            Err(NativeFailure::GenerationMismatch)
        } else {
            Ok(())
        }
    }
}

fn context(retriever: u32) -> PendingStartupContext {
    PendingStartupContext {
        key: AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        primary_gid: 20,
        registration_generation: NonZeroU64::new(7).unwrap(),
        lifecycle_generation: NonZeroU64::new(8).unwrap(),
        data_generation: DataGenerationId::new(9).unwrap(),
        installation: InstallationId(NonZeroU64::new(10).unwrap()),
        server_id: [1; 16],
        account_binding_sha256: [2; 32],
        installation_binding: [3; 32],
        server_artifact_sha256: [4; 32],
        manifest_sha256: [5; 32],
        credential_binding_sha256: [6; 32],
        startup_nonce: [7; 32],
        original_process: process_identity(std::process::id()).unwrap(),
        retriever_process: process_identity(retriever).unwrap(),
        journal_request: None,
    }
}

fn startup(fault: Fault) -> (Result<(), NativeFailure>, Observations, usize) {
    // This child represents independently observed native process authority.
    // The peer thread owns only the injected authority adapter, not a root key.
    let mut owned = Some(
        Command::new("/bin/sh")
            .args(["-c", "read line; exit 0"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = owned.as_ref().unwrap().id();
    let mut retire = owned.as_mut().unwrap().stdin.take().unwrap();
    let context = context(pid);
    let child = ExactChild::take(&mut owned, context.retriever_process).unwrap();
    let transcript = StartupTranscript::from_context(&context).unwrap();
    let mut peer_context = context.clone();
    if matches!(fault, Fault::WrongReady) {
        peer_context.lifecycle_generation = NonZeroU64::new(99).unwrap();
    }
    let peer_transcript = StartupTranscript::from_context(&peer_context).unwrap();
    let (rr, rw) = anonymous_pipe().unwrap();
    let (dr, dw) = anonymous_pipe().unwrap();
    let (cr, cw) = anonymous_pipe().unwrap();
    let (ar, aw) = anonymous_pipe().unwrap();
    let (group_read, group_write) = anonymous_pipe().unwrap();
    let receiver = ReceiverPipes::take(rw, dr, cw, ar).unwrap();
    let retriever = RetrieverPipes::take(rr, dw, cr, aw).unwrap();
    let observations = Arc::new(Mutex::new(Observations::default()));
    let peer_observations = observations.clone();
    let deadline = ExchangeDeadline::start(Duration::from_secs(2)).unwrap();
    let peer = std::thread::spawn(move || {
        let result = ReceiverGroups::read_report(group_read, &peer_transcript, deadline).and_then(
            |groups| {
                assert_eq!(groups.groups(), &[501, 20, 80]);
                exchange(
                    &peer_transcript,
                    retriever,
                    &ReleasePeer {
                        observations: peer_observations,
                        fault,
                    },
                    deadline,
                )
            },
        );
        retire.write_all(b"retire\n").unwrap();
        result
    });
    let receiver =
        NativeReceiver::take(transcript, receiver, child, Some(group_write), deadline).unwrap();
    let authority = FinalPeer {
        observations: observations.clone(),
        retriever: pid,
        fault,
    };
    let mut listener_count = 0;
    let outcome =
        spaceterm_server::native_startup::admit_using(receiver, &Protection(fault), &authority)
            .map(|receipt| {
                assert!(receipt.secret().iter().any(|byte| *byte != 0));
                let listener =
                    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
                listener_count += 1;
                drop(listener);
            });
    let peer_result = peer.join().unwrap();
    if matches!(fault, Fault::None | Fault::FinalAuthority) {
        assert_eq!(peer_result, Ok(()));
    } else {
        assert!(peer_result.is_err());
    }
    assert!(process_identity(pid).is_err());
    drop(authority);
    let observations = Arc::try_unwrap(observations)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap();
    (outcome, observations, listener_count)
}

#[test]
fn admission_follows_exact_reaping_and_native_revalidation() {
    let (outcome, observed, listeners) = startup(Fault::None);
    assert_eq!(outcome, Ok(()));
    assert_eq!(
        (
            observed.authorized,
            observed.retrieved,
            observed.final_checks,
            listeners
        ),
        (1, 1, 1, 1)
    );
}

#[test]
fn protection_failure_precedes_credential_effects() {
    let (outcome, observed, listeners) = startup(Fault::Protection);
    assert_eq!(outcome, Err(NativeFailure::ExecutionBindingUnproven));
    assert_eq!(
        (
            observed.authorized,
            observed.retrieved,
            observed.final_checks,
            listeners
        ),
        (0, 0, 0, 0)
    );
}

#[test]
fn retired_generation_before_delivery_never_admits() {
    let (outcome, observed, listeners) = startup(Fault::BeforeDelivery);
    assert_eq!(outcome, Err(NativeFailure::CredentialUnavailable));
    assert_eq!(
        (observed.retrieved, observed.final_checks, listeners),
        (1, 0, 0)
    );
}

#[test]
fn startup_failure_creates_no_listener() {
    let (outcome, observed, listeners) = startup(Fault::FinalAuthority);
    assert_eq!(outcome, Err(NativeFailure::GenerationMismatch));
    assert_eq!((observed.final_checks, listeners), (1, 0));
}

#[test]
fn exact_ready_context_is_required_before_authorization() {
    let (outcome, observed, listeners) = startup(Fault::WrongReady);
    assert_eq!(outcome, Err(NativeFailure::CredentialUnavailable));
    assert_eq!(
        (observed.authorized, observed.retrieved, listeners),
        (0, 0, 0)
    );
}
