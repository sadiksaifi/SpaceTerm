//! The retriever's exit is not a receipt for its independently guarded scope.

use super::*;
use spaceterm_host::native_receiver::{
    NativeReceiver, ReceiverAuthority, ReceiverGroups, ReceiverProtection,
};
use std::{cell::Cell, io::Write};

#[derive(Clone, Copy)]
enum FailureAt {
    Exchange,
    Protection,
    EndpointClaim,
}

#[derive(Default)]
struct FailedProtection(Cell<usize>);
impl ReceiverProtection for FailedProtection {
    fn prepare(&self, _transcript: &StartupTranscript) -> Result<ReceiverGroups, NativeFailure> {
        self.0.set(self.0.get() + 1);
        Err(NativeFailure::ExecutionBindingUnproven)
    }
}

#[derive(Default)]
struct FinalAuthority(Cell<usize>);
impl ReceiverAuthority for FinalAuthority {
    fn revalidate(
        &self,
        _transcript: &StartupTranscript,
        _groups: &ReceiverGroups,
    ) -> Result<(), NativeFailure> {
        self.0.set(self.0.get() + 1);
        Err(NativeFailure::CustodyUnavailable)
    }
}

#[test]
fn retriever_cleanup_uncertainty_survives_exit_before_guard_retirement() {
    assert_cleanup_precedence(FailureAt::Exchange);
}

#[test]
fn receiver_protection_failure_preserves_retriever_cleanup_uncertainty() {
    assert_cleanup_precedence(FailureAt::Protection);
}

#[test]
fn receiver_endpoint_claim_failure_preserves_retriever_cleanup_uncertainty() {
    assert_cleanup_precedence(FailureAt::EndpointClaim);
}

fn assert_cleanup_precedence(failure_at: FailureAt) {
    for (inherited, command) in [
        (false, "read line; exit 33"),
        (true, "read line; exit 33"),
        (false, "read line; kill -KILL $$"),
        (true, "read line; kill -KILL $$"),
    ] {
        // Failed tool retirement reports exit 33; unexpected signal death also
        // supplies no scope retirement proof. The guard remains alive in both cases.
        let mut guard = Command::new("/bin/sh")
            .args(["-c", "read line"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut owned = Some(
            Command::new("/bin/sh")
                .args(["-c", command])
                .env_clear()
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let identity = process_identity(owned.as_ref().unwrap().id()).unwrap();
        let mut exit = owned.as_mut().unwrap().stdin.take().unwrap();
        let exact = if inherited {
            // The fixture alone owns the fork/wait reservation. No other waiter
            // touches the retained Child before or after this ownership transfer.
            drop(owned.take());
            unsafe { ExactChild::inherit(identity) }.unwrap()
        } else {
            ExactChild::take(&mut owned, identity).unwrap()
        };
        let mut ready = literal_ready();
        let original = process_identity(std::process::id()).unwrap();
        for (offset, process) in [(272, original), (296, identity)] {
            ready[offset..offset + 8].copy_from_slice(&u64::from(process.pid).to_be_bytes());
            ready[offset + 8..offset + 16].copy_from_slice(&process.start_seconds.to_be_bytes());
            ready[offset + 16..offset + 20]
                .copy_from_slice(&process.start_microseconds.to_be_bytes());
        }
        let transcript = StartupTranscript::recognize_ready(&ready).unwrap();
        let (rr, rw) = anonymous_pipe().unwrap();
        let (dr, dw) = anonymous_pipe().unwrap();
        let (cr, cw) = anonymous_pipe().unwrap();
        let (ar, aw) = anonymous_pipe().unwrap();
        let deadline = ExchangeDeadline::start(Duration::from_secs(2)).unwrap();
        let peer = std::thread::spawn(move || {
            let mut input = PipeEndpoint::take(rr, false).unwrap();
            let mut frame = LockedMemory::<353>::allocate().unwrap();
            // Setup failures must close readiness without emitting any bytes.
            let expected = if matches!(failure_at, FailureAt::Exchange) {
                352
            } else {
                0
            };
            input.read_frame(&mut frame, expected, deadline).unwrap();
            exit.write_all(b"exit\n").unwrap();
            drop((dw, cr, aw));
        });
        let pipes = ReceiverPipes::take(rw, dr, cw, ar).unwrap();
        let protection = FailedProtection::default();
        let authority = FinalAuthority::default();
        let outcome = match failure_at {
            FailureAt::Exchange => receive(&transcript, pipes, exact, deadline).map(|_| ()),
            FailureAt::Protection | FailureAt::EndpointClaim => {
                let (group_read, group_write) = anonymous_pipe().unwrap();
                let report = if matches!(failure_at, FailureAt::EndpointClaim) {
                    drop(group_write);
                    group_read // Wrong direction rejects at endpoint claim.
                } else {
                    drop(group_read);
                    group_write
                };
                NativeReceiver::take(transcript, pipes, exact, Some(report), deadline)
                    .and_then(|receiver| receiver.complete(&protection, &authority))
                    .map(|_| ())
            }
        };
        peer.join().unwrap();
        let guard_alive = guard.try_wait().unwrap().is_none();
        guard.stdin.take().unwrap().write_all(b"retire\n").unwrap();
        assert!(guard.wait().unwrap().success());
        assert!(guard_alive);
        assert_eq!(
            protection.0.get(),
            usize::from(matches!(failure_at, FailureAt::Protection))
        );
        assert_eq!(authority.0.get(), 0);
        assert_eq!(outcome, Err(NativeFailure::CleanupUnresolved));
        assert_eq!(
            unsafe { libc::waitpid(identity.pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
