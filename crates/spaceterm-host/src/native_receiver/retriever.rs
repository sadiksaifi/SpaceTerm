//! Privileged half of STN2. Native proof and live release fencing precede every
//! credential effect; wire declarations alone never authorize retrieval.

use std::os::fd::OwnedFd;

use crate::{
    boot_credentials::{BootCredentialAdapter, CredentialReleasePermit, LockedSecret32},
    native_deployment::contracts::NativeFailure,
    startup::{NativeStartupAuthority, PendingStartup, PostExecProof},
    startup_exchange::{
        ExchangeDeadline, FRAME_BYTES, FrameKind, PREFIX_BYTES, StartupTranscript,
        unix::{LockedMemory, PipeEndpoint},
    },
};

/// Retriever-owned endpoints. The launcher closes every other writer before exec.
pub struct RetrieverPipes {
    ready: PipeEndpoint,
    delivery: PipeEndpoint,
    confirmation: PipeEndpoint,
    acknowledgement: PipeEndpoint,
}
impl RetrieverPipes {
    /// Takes the exact anonymous endpoints, restoring CLOEXEC before any tool.
    pub fn take(
        ready: OwnedFd,
        delivery: OwnedFd,
        confirmation: OwnedFd,
        acknowledgement: OwnedFd,
    ) -> Result<Self, NativeFailure> {
        Ok(Self {
            ready: PipeEndpoint::take(ready, false)?,
            delivery: PipeEndpoint::take(delivery, true)?,
            confirmation: PipeEndpoint::take(confirmation, false)?,
            acknowledgement: PipeEndpoint::take(acknowledgement, true)?,
        })
    }
}

/// Live serialized native release fence. The native adapter owns registration,
/// execution, credential binding and Stop/release serialization through delivery.
pub trait BootCredentialRelease {
    /// Rechecks Running intent, current generations and retained native execution.
    fn recheck(&self) -> Result<(), NativeFailure>;
    /// Retrieves into already locked owned memory, without plaintext fallback.
    fn retrieve(&self, destination: &mut LockedSecret32) -> Result<(), NativeFailure>;
}

/// Release authority boundary, also used by explicitly injected native test peers.
/// There is no production adapter that accepts readiness bits as authority.
pub trait BootCredentialExchange {
    /// Independently authorizes this exact startup after its Ready frame matches.
    fn authorize(
        &self,
        transcript: &StartupTranscript,
    ) -> Result<Box<dyn BootCredentialRelease + '_>, NativeFailure>;
}

/// Production native composition retains the issuer's pending and post-exec
/// proof. The lifetime keeps both alive through retrieval and delivery.
pub struct NativeCredentialExchange<'a> {
    /// Retained startup authority, not decoded context facts.
    pub pending: &'a PendingStartup,
    /// Independently observed post-exec proof with retained execution custody.
    pub proof: &'a PostExecProof,
    /// Retained native registration and serialized release authority.
    pub authority: &'a dyn NativeStartupAuthority,
    /// Fixed platform credential adapter selected by trusted composition.
    pub credentials: &'a dyn BootCredentialAdapter,
}
impl BootCredentialExchange for NativeCredentialExchange<'_> {
    fn authorize(
        &self,
        transcript: &StartupTranscript,
    ) -> Result<Box<dyn BootCredentialRelease + '_>, NativeFailure> {
        let expected = StartupTranscript::from_context(self.pending.context())?;
        expected.recognize_prefix(FrameKind::Ready, &transcript.prefix(FrameKind::Ready))?;
        self.proof.recheck(self.pending.context())?;
        let permit = self.authority.authorize_release(self.pending, self.proof)?;
        let release = NativeRelease {
            exchange: self,
            permit,
        };
        release.recheck()?;
        Ok(Box::new(release))
    }
}
struct NativeRelease<'a, 'b> {
    exchange: &'a NativeCredentialExchange<'b>,
    permit: CredentialReleasePermit,
}
impl BootCredentialRelease for NativeRelease<'_, '_> {
    fn recheck(&self) -> Result<(), NativeFailure> {
        self.exchange
            .proof
            .recheck(self.exchange.pending.context())?;
        self.exchange.authority.recheck_release(&self.permit)
    }
    fn retrieve(&self, destination: &mut LockedSecret32) -> Result<(), NativeFailure> {
        self.recheck()?;
        self.exchange
            .credentials
            .retrieve_for_startup(&self.permit, destination)
    }
}

/// Completes the retriever transcript with a live release fence. Call only in a
/// short-lived retriever after installing an independent native lifetime guard.
/// All transient secret, challenge, frame and MAC storage is locked and wiped;
/// endpoints and the release fence are consumed on every return path.
pub fn exchange(
    transcript: &StartupTranscript,
    mut pipes: RetrieverPipes,
    authority: &dyn BootCredentialExchange,
    deadline: ExchangeDeadline,
) -> Result<(), NativeFailure> {
    let mut frame = LockedMemory::<417>::allocate()?;
    pipes.ready.read_frame(&mut frame, PREFIX_BYTES, deadline)?;
    transcript.recognize_prefix(FrameKind::Ready, &frame.bytes()[..PREFIX_BYTES])?;
    frame.wipe();
    let release = authority.authorize(transcript)?;
    let mut secret = LockedSecret32::new()?;
    release.recheck()?;
    deadline.remaining()?;
    release.retrieve(&mut secret)?;
    deadline.remaining()?;
    let mut challenge = LockedMemory::<32>::allocate()?;
    getrandom::fill(challenge.bytes_mut()).map_err(|_| NativeFailure::NativeUnavailable)?;
    let macs = transcript.confirmation_macs(secret.bytes(), challenge.bytes())?;
    frame.bytes_mut()[..PREFIX_BYTES].copy_from_slice(&transcript.prefix(FrameKind::Delivery));
    frame.bytes_mut()[352..384].copy_from_slice(challenge.bytes());
    frame.bytes_mut()[384..416].copy_from_slice(secret.bytes());
    // The native fence excludes Stop/update publication while this write occurs.
    release.recheck()?;
    pipes
        .delivery
        .write_frame(&frame.bytes()[..FRAME_BYTES], deadline)?;
    frame.wipe();
    secret.wipe();
    pipes
        .confirmation
        .read_frame(&mut frame, FRAME_BYTES, deadline)?;
    transcript.recognize_prefix(FrameKind::Confirmation, &frame.bytes()[..PREFIX_BYTES])?;
    if frame.bytes()[352..384] != challenge.bytes()[..] {
        return Err(NativeFailure::CredentialUnavailable);
    }
    macs.verify_confirmation(&frame.bytes()[384..416])?;
    frame.wipe();
    frame.bytes_mut()[..PREFIX_BYTES]
        .copy_from_slice(&transcript.prefix(FrameKind::Acknowledgement));
    frame.bytes_mut()[352..384].copy_from_slice(challenge.bytes());
    frame.bytes_mut()[384..416].copy_from_slice(macs.acknowledgement());
    release.recheck()?;
    pipes
        .acknowledgement
        .write_frame(&frame.bytes()[..FRAME_BYTES], deadline)?;
    deadline.remaining()?;
    Ok(())
}
