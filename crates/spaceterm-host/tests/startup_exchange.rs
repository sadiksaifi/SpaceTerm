//! Tests against the exported host exchange, with injected nonauthorizing peers.

use spaceterm_host::native_deployment::contracts::NativeFailure;
use spaceterm_host::startup_exchange;
use startup_exchange::{FrameKind, StartupTranscript};

#[test]
fn confirmation_binds_every_startup_field() {
    let ready = literal_ready();
    let transcript = StartupTranscript::recognize_ready(&ready).unwrap();
    let confirmation = transcript.prefix(FrameKind::Confirmation);
    for offset in [
        16, 20, 24, 32, 40, 48, 56, 64, 80, 112, 144, 176, 208, 240, 272, 280, 288, 296, 304, 312,
        320,
    ] {
        let mut altered = confirmation;
        altered[offset] ^= 1;
        assert!(
            transcript
                .recognize_prefix(FrameKind::Confirmation, &altered)
                .is_err()
        );
    }
}

fn literal_ready() -> [u8; 352] {
    let mut bytes = [0; 352];
    bytes[..4].copy_from_slice(b"STN2");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[8..12].copy_from_slice(&352u32.to_be_bytes());
    for (offset, value) in [(16, 501u32), (20, 20), (24, 3), (28, 63)] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (offset, value) in [
        (32, 7u64),
        (40, 8),
        (48, 9),
        (56, 10),
        (272, 111),
        (280, 1000),
        (296, 112),
        (304, 1001),
    ] {
        bytes[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    bytes[288..292].copy_from_slice(&123u32.to_be_bytes());
    bytes[312..316].copy_from_slice(&456u32.to_be_bytes());
    for (i, byte) in bytes[64..272].iter_mut().enumerate() {
        *byte = (i % 251 + 1) as u8;
    }
    bytes
}

use startup_exchange::unix::{
    ExactChild, LockedMemory, PipeEndpoint, ReceiverPipes, anonymous_pipe, process_identity,
    receive,
};
use startup_exchange::{ExchangeDeadline, ProcessIdentity};
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn abi_two_rejects_reserved_values_sizes_and_fixture_magic() {
    for offset in [0, 4, 5, 6, 7, 8, 12, 28, 292, 316, 336, 351] {
        let mut bytes = literal_ready();
        bytes[offset] ^= 128;
        assert!(
            StartupTranscript::recognize_ready(&bytes).is_err(),
            "offset {offset}"
        );
    }
    for length in [0, 48, 192, 351] {
        assert!(StartupTranscript::recognize_ready(&literal_ready()[..length]).is_err());
    }
    assert!(StartupTranscript::recognize_ready(&[0; 353]).is_err());
}

#[test]
fn abi_two_rejects_invalid_identity_generation_digest_and_process_bounds() {
    for (offset, size) in [
        (16, 4),
        (24, 4),
        (32, 8),
        (40, 8),
        (48, 8),
        (56, 8),
        (64, 16),
        (80, 32),
        (112, 32),
        (144, 32),
        (176, 32),
        (208, 32),
        (240, 32),
        (272, 8),
        (296, 8),
    ] {
        let mut bytes = literal_ready();
        bytes[offset..offset + size].fill(0);
        assert!(
            StartupTranscript::recognize_ready(&bytes).is_err(),
            "offset {offset}"
        );
    }
    let mut bytes = literal_ready();
    bytes[312..316].copy_from_slice(&1_000_000u32.to_be_bytes());
    assert!(StartupTranscript::recognize_ready(&bytes).is_err());
    bytes = literal_ready();
    bytes[272..280].copy_from_slice(&(i32::MAX as u64 + 1).to_be_bytes());
    assert!(StartupTranscript::recognize_ready(&bytes).is_err());
}

#[test]
fn acknowledgement_and_confirmation_are_distinct_and_constant_time_checked() {
    let transcript = StartupTranscript::recognize_ready(&literal_ready()).unwrap();
    let mut secret = LockedMemory::<32>::allocate().unwrap();
    secret.bytes_mut().fill(7);
    let macs = transcript
        .confirmation_macs(secret.bytes(), &[11; 32])
        .unwrap();
    assert_ne!(macs.confirmation(), macs.acknowledgement());
    assert_eq!(macs.verify_confirmation(macs.confirmation()), Ok(()));
    assert_eq!(macs.verify_acknowledgement(macs.acknowledgement()), Ok(()));
    assert_eq!(
        macs.verify_confirmation(macs.acknowledgement()),
        Err(NativeFailure::CredentialUnavailable)
    );
    assert!(macs.verify_acknowledgement(&[0; 31]).is_err());
    let changed = transcript
        .confirmation_macs(secret.bytes(), &[12; 32])
        .unwrap();
    assert_ne!(changed.confirmation(), macs.confirmation());
    assert_eq!(format!("{macs:?}"), "ConfirmationMacs([redacted])");
    assert_eq!(format!("{transcript:?}"), "StartupTranscript([redacted])");
    assert_eq!(format!("{secret:?}"), "LockedMemory([redacted])");
}

#[test]
fn confirmation_covers_every_context_byte_and_optional_journal() {
    let secret = [7; 32];
    let challenge = [11; 32];
    let original = StartupTranscript::recognize_ready(&literal_ready()).unwrap();
    let expected = original.confirmation_macs(&secret, &challenge).unwrap();
    for range in [
        16..20,
        20..24,
        24..28,
        32..40,
        40..48,
        48..56,
        56..64,
        64..80,
        80..112,
        112..144,
        144..176,
        176..208,
        208..240,
        240..272,
        272..292,
        296..316,
        320..336,
    ] {
        for offset in range {
            let mut bytes = literal_ready();
            bytes[offset] ^= 1;
            if let Ok(changed) = StartupTranscript::recognize_ready(&bytes) {
                let value = changed.confirmation_macs(&secret, &challenge).unwrap();
                assert_ne!(
                    expected.confirmation(),
                    value.confirmation(),
                    "offset {offset}"
                );
            }
        }
    }
}

#[test]
fn secret_delivery_fragmentation_preserves_absolute_deadline() {
    let (read, write) = anonymous_pipe().unwrap();
    let deadline = ExchangeDeadline::start(Duration::from_millis(35)).unwrap();
    let writer = std::thread::spawn(move || {
        let raw = write.as_raw_fd();
        let mut signals = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut signals);
            libc::sigaddset(&mut signals, libc::SIGPIPE);
            libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut());
        }
        for byte in [1u8, 2, 3] {
            unsafe {
                libc::write(raw, (&byte as *const u8).cast(), 1);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let mut endpoint = PipeEndpoint::take(read, false).unwrap();
    let mut frame = LockedMemory::<4>::allocate().unwrap();
    let start = Instant::now();
    assert_eq!(
        endpoint.read_frame(&mut frame, 3, deadline),
        Err(NativeFailure::TimedOut)
    );
    assert!(start.elapsed() < Duration::from_millis(100));
    // The fixture blocks SIGPIPE on its own writer thread during deadline cleanup.
    writer.join().unwrap();
}

#[test]
fn frames_require_eof_and_reject_trailing_byte_and_wrong_direction() {
    let (read, write) = anonymous_pipe().unwrap();
    let mut endpoint = PipeEndpoint::take(read, false).unwrap();
    let mut writer = PipeEndpoint::take(write, true).unwrap();
    let deadline = ExchangeDeadline::start(Duration::from_millis(100)).unwrap();
    writer.write_frame(&[1, 2, 3, 4], deadline).unwrap();
    let mut frame = LockedMemory::<4>::allocate().unwrap();
    assert_eq!(
        endpoint.read_frame(&mut frame, 3, deadline),
        Err(NativeFailure::InvalidRecord)
    );
    let (read, write) = anonymous_pipe().unwrap();
    assert!(PipeEndpoint::take(read, true).is_err());
    drop(write);
}

#[test]
fn full_frame_without_eof_times_out() {
    let (read, write) = anonymous_pipe().unwrap();
    unsafe {
        libc::write(write.as_raw_fd(), b"abc".as_ptr().cast(), 3);
    }
    let mut endpoint = PipeEndpoint::take(read, false).unwrap();
    let mut frame = LockedMemory::<4>::allocate().unwrap();
    assert_eq!(
        endpoint.read_frame(
            &mut frame,
            3,
            ExchangeDeadline::start(Duration::from_millis(20)).unwrap()
        ),
        Err(NativeFailure::TimedOut)
    );
}

#[test]
fn reader_death_does_not_change_sigpipe_disposition_or_kill_receiver() {
    let (read, write) = anonymous_pipe().unwrap();
    drop(read);
    let mut endpoint = PipeEndpoint::take(write, true).unwrap();
    assert_eq!(
        endpoint.write_frame(
            &[1],
            ExchangeDeadline::start(Duration::from_millis(20)).unwrap()
        ),
        Err(NativeFailure::CredentialUnavailable)
    );
}

#[test]
fn exchange_descriptors_are_cloexec_before_any_subsequent_child() {
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;

    let (rr, rw) = anonymous_pipe().unwrap();
    let (dr, dw) = anonymous_pipe().unwrap();
    let (cr, cw) = anonymous_pipe().unwrap();
    let (ar, aw) = anonymous_pipe().unwrap();
    let inherited: Vec<OwnedFd> = [rw, dr, cw, ar]
        .into_iter()
        .map(|fd| {
            let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 128) };
            assert!(raw >= 128);
            unsafe { OwnedFd::from_raw_fd(raw) }
        })
        .collect();
    let raw: Vec<_> = inherited.iter().map(AsRawFd::as_raw_fd).collect();
    // Model the one intended server exec: these four are deliberately inherited.
    for fd in &raw {
        assert_eq!(unsafe { libc::fcntl(*fd, libc::F_SETFD, 0) }, 0);
    }
    let mut inherited = inherited.into_iter();
    let pipes = ReceiverPipes::take(
        inherited.next().unwrap(),
        inherited.next().unwrap(),
        inherited.next().unwrap(),
        inherited.next().unwrap(),
    )
    .unwrap();
    let flags_restored = raw
        .iter()
        .all(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
    let mut helper = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "live_descriptor_helper",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env(
            "ST_TEST_DESCRIPTORS",
            raw.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = [0; 6];
    let ready_read = helper
        .stderr
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .is_ok();
    let alive_before = helper.try_wait().unwrap().is_none();
    drop(pipes);
    // Both writers must yield EOF while the subsequent exec is still alive.
    let outcomes: Vec<_> = [rr, cr]
        .into_iter()
        .map(|fd| {
            let mut reader = PipeEndpoint::take(fd, false).unwrap();
            let mut bytes = LockedMemory::<2>::allocate().unwrap();
            reader.read_frame(
                &mut bytes,
                1,
                ExchangeDeadline::start(Duration::from_millis(300)).unwrap(),
            )
        })
        .collect();
    let alive_after = helper.try_wait().unwrap().is_none();
    let _ = helper.stdin.take().unwrap().write_all(b"x");
    let status = helper.wait().unwrap();
    drop((dw, aw));
    // All assertions follow exact helper retirement, including mutation RED.
    assert!(ready_read && &ready == b"ready\n");
    assert!(flags_restored && alive_before && alive_after && status.success());
    assert_eq!(outcomes, vec![Err(NativeFailure::CredentialUnavailable); 2]);
}

#[test]
#[ignore = "executed in a bounded live helper by the descriptor regression"]
fn live_descriptor_helper() {
    use std::io::{Read, Write};
    unsafe {
        libc::alarm(3);
    }
    let clean = std::env::var("ST_TEST_DESCRIPTORS")
        .unwrap()
        .split(',')
        .all(|fd| {
            let fd: i32 = fd.parse().unwrap();
            (unsafe { libc::fcntl(fd, libc::F_GETFD) }) == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::EBADF)
        });
    std::io::stderr()
        .write_all(if clean { b"ready\n" } else { b"leak!\n" })
        .unwrap();
    std::io::stdin().read_exact(&mut [0; 1]).unwrap();
    assert!(clean);
}

#[test]
fn locked_buffer_wipes_while_still_owned_and_locked() {
    let mut memory = LockedMemory::<417>::allocate().unwrap();
    memory.bytes_mut().fill(171);
    memory.wipe();
    assert!(memory.bytes().iter().all(|&byte| byte == 0));
}

#[test]
fn exchange_budget_rejects_zero_and_unbounded_waits() {
    assert!(ExchangeDeadline::start(Duration::ZERO).is_err());
    assert!(ExchangeDeadline::start(Duration::from_secs(3)).is_err());
}

#[test]
fn exact_child_rejects_pid_reuse_and_nonchild() {
    let child = helper_child("0.05");
    let identity = process_identity(child.id()).unwrap();
    let changed = ProcessIdentity {
        start_microseconds: (identity.start_microseconds + 1) % 1_000_000,
        ..identity
    };
    let mut owned = Some(child);
    assert!(ExactChild::take(&mut owned, changed).is_err());
    assert!(ExactChild::take(&mut owned, process_identity(std::process::id()).unwrap()).is_err());
    owned.as_mut().unwrap().wait().unwrap();
}

#[derive(Clone, Copy)]
enum Peer {
    Exact,
    WrongAck,
    DiesBeforeAck,
    Timeout,
    LateRetirement(ExchangeDeadline),
}

fn helper_child(delay: &str) -> std::process::Child {
    Command::new("/bin/sleep")
        .arg(delay)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn receiver_case(peer: Peer) -> Result<bool, NativeFailure> {
    let mut child = Some(helper_child("0.05"));
    let identity = process_identity(child.as_ref().unwrap().id()).unwrap();
    let retained = ExactChild::take(&mut child, identity).unwrap();
    let mut ready = literal_ready();
    let original = process_identity(std::process::id()).unwrap();
    ready[272..280].copy_from_slice(&(original.pid as u64).to_be_bytes());
    ready[280..288].copy_from_slice(&original.start_seconds.to_be_bytes());
    ready[288..292].copy_from_slice(&original.start_microseconds.to_be_bytes());
    ready[296..304].copy_from_slice(&(identity.pid as u64).to_be_bytes());
    ready[304..312].copy_from_slice(&identity.start_seconds.to_be_bytes());
    ready[312..316].copy_from_slice(&identity.start_microseconds.to_be_bytes());
    let transcript = StartupTranscript::recognize_ready(&ready).unwrap();
    let (rr, rw) = anonymous_pipe().unwrap();
    let (dr, dw) = anonymous_pipe().unwrap();
    let (cr, cw) = anonymous_pipe().unwrap();
    let (ar, aw) = anonymous_pipe().unwrap();
    let pipes = ReceiverPipes::take(rw, dr, cw, ar).unwrap();
    let thread = std::thread::spawn(move || fake_retriever(ready, rr, dw, cr, aw, peer));
    let budget = if matches!(peer, Peer::Timeout) {
        25
    } else {
        400
    };
    let result = receive(
        &transcript,
        pipes,
        retained,
        ExchangeDeadline::start(Duration::from_millis(budget)).unwrap(),
    )
    .map(|receipt| receipt.secret().iter().all(|&byte| byte == 7));
    thread.join().unwrap();
    drop(child); // The exact child token, not std::process::Child, performed waitpid.
    result
}

fn fake_retriever(
    ready: [u8; 352],
    rr: OwnedFd,
    dw: OwnedFd,
    cr: OwnedFd,
    aw: OwnedFd,
    peer: Peer,
) {
    let deadline = ExchangeDeadline::start(Duration::from_millis(600)).unwrap();
    let transcript = StartupTranscript::recognize_ready(&ready).unwrap();
    let mut reader = PipeEndpoint::take(rr, false).unwrap();
    let mut writer = PipeEndpoint::take(dw, true).unwrap();
    let mut confirmation = PipeEndpoint::take(cr, false).unwrap();
    let mut acknowledgement = PipeEndpoint::take(aw, true).unwrap();
    let mut frame = LockedMemory::<417>::allocate().unwrap();
    reader.read_frame(&mut frame, 352, deadline).unwrap();
    transcript
        .recognize_prefix(FrameKind::Ready, &frame.bytes()[..352])
        .unwrap();
    if matches!(peer, Peer::Timeout) {
        std::thread::sleep(Duration::from_millis(80));
        return;
    }
    frame.wipe();
    frame.bytes_mut()[..352].copy_from_slice(&transcript.prefix(FrameKind::Delivery));
    frame.bytes_mut()[352..384].fill(11);
    frame.bytes_mut()[384..416].fill(7);
    writer.write_frame(&frame.bytes()[..416], deadline).unwrap();
    frame.wipe();
    confirmation.read_frame(&mut frame, 416, deadline).unwrap();
    transcript
        .recognize_prefix(FrameKind::Confirmation, &frame.bytes()[..352])
        .unwrap();
    let mut secret = LockedMemory::<32>::allocate().unwrap();
    secret.bytes_mut().fill(7);
    let macs = transcript
        .confirmation_macs(secret.bytes(), &[11; 32])
        .unwrap();
    macs.verify_confirmation(&frame.bytes()[384..416]).unwrap();
    if matches!(peer, Peer::DiesBeforeAck) {
        return;
    }
    frame.wipe();
    frame.bytes_mut()[..352].copy_from_slice(&transcript.prefix(FrameKind::Acknowledgement));
    frame.bytes_mut()[352..384].fill(11);
    frame.bytes_mut()[384..416].copy_from_slice(macs.acknowledgement());
    if matches!(peer, Peer::WrongAck) {
        frame.bytes_mut()[415] ^= 1;
    }
    if let Peer::LateRetirement(exchange_deadline) = peer {
        // Acknowledgement still fits, but the independently held child cannot
        // retire before the original deadline. A fresh reap budget would admit it.
        std::thread::sleep(
            exchange_deadline
                .remaining()
                .unwrap()
                .saturating_sub(Duration::from_millis(100)),
        );
        exchange_deadline.remaining().unwrap();
    }
    acknowledgement
        .write_frame(&frame.bytes()[..416], deadline)
        .unwrap();
}

#[test]
fn retriever_retirement_cannot_extend_the_absolute_exchange_deadline() {
    use std::io::Write;

    let mut child = Some(
        Command::new("/bin/sh")
            .args(["-c", "read line; exit 0"])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut retirement = child.as_mut().unwrap().stdin.take().unwrap();
    let identity = process_identity(child.as_ref().unwrap().id()).unwrap();
    let exact = ExactChild::take(&mut child, identity).unwrap();
    let original = process_identity(std::process::id()).unwrap();
    let mut ready = literal_ready();
    for (offset, process) in [(272, original), (296, identity)] {
        ready[offset..offset + 8].copy_from_slice(&u64::from(process.pid).to_be_bytes());
        ready[offset + 8..offset + 16].copy_from_slice(&process.start_seconds.to_be_bytes());
        ready[offset + 16..offset + 20].copy_from_slice(&process.start_microseconds.to_be_bytes());
    }
    let transcript = StartupTranscript::recognize_ready(&ready).unwrap();
    let (rr, rw) = anonymous_pipe().unwrap();
    let (dr, dw) = anonymous_pipe().unwrap();
    let (cr, cw) = anonymous_pipe().unwrap();
    let (ar, aw) = anonymous_pipe().unwrap();
    let pipes = ReceiverPipes::take(rw, dr, cw, ar).unwrap();
    let deadline = ExchangeDeadline::start(Duration::from_millis(500)).unwrap();
    let peer = std::thread::spawn(move || {
        fake_retriever(ready, rr, dw, cr, aw, Peer::LateRetirement(deadline));
        std::thread::sleep(deadline.remaining().unwrap_or_default() + Duration::from_millis(30));
        assert_eq!(deadline.remaining(), Err(NativeFailure::TimedOut));
        retirement.write_all(b"exit\n").unwrap();
    });

    let outcome = receive(&transcript, pipes, exact, deadline).map(|_| ());
    peer.join().unwrap();
    assert_eq!(outcome, Err(NativeFailure::TimedOut));
    // The separate failure-cleanup allowance must still consume the exact child.
    assert_eq!(
        unsafe { libc::waitpid(identity.pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn receipt_requires_acknowledgement_and_exact_reaping() {
    assert_eq!(receiver_case(Peer::Exact), Ok(true));
}
#[test]
fn confirmation_failure_prevents_readiness() {
    assert_eq!(
        receiver_case(Peer::WrongAck),
        Err(NativeFailure::CredentialUnavailable)
    );
}
#[test]
fn retriever_death_before_ack_prevents_readiness() {
    assert_eq!(
        receiver_case(Peer::DiesBeforeAck),
        Err(NativeFailure::CredentialUnavailable)
    );
}
#[test]
fn exchange_timeout_wipes_and_retires_owned_processes() {
    assert_eq!(receiver_case(Peer::Timeout), Err(NativeFailure::TimedOut));
}
#[test]
fn unresolved_reaping_overrides_success_and_cancellation() {
    let mut child = Some(helper_child("0.35"));
    let identity = process_identity(child.as_ref().unwrap().id()).unwrap();
    let mut exact = ExactChild::take(&mut child, identity).unwrap();
    assert_eq!(
        exact.reap(ExchangeDeadline::start(Duration::from_millis(10)).unwrap()),
        Err(NativeFailure::CleanupUnresolved)
    );
    assert_eq!(
        exact.reap(ExchangeDeadline::start(Duration::from_millis(500)).unwrap()),
        Ok(true)
    );
}

#[test]
fn failed_frame_read_wipes_destination_and_closes_endpoint() {
    let (read, write) = anonymous_pipe().unwrap();
    unsafe {
        libc::write(write.as_raw_fd(), b"ab".as_ptr().cast(), 2);
    }
    drop(write);
    let mut endpoint = PipeEndpoint::take(read, false).unwrap();
    let mut frame = LockedMemory::<4>::allocate().unwrap();
    frame.bytes_mut().fill(99);
    // This checks malformed EOF cleanup, not scheduler latency. Deadline
    // expiration has separate tests; keep this operation within the ABI budget.
    let outcome = endpoint.read_frame(
        &mut frame,
        3,
        ExchangeDeadline::start(Duration::from_secs(2)).unwrap(),
    );
    assert_eq!(outcome, Err(NativeFailure::CredentialUnavailable));
    assert!(frame.bytes().iter().all(|&byte| byte == 0));
    assert_eq!(
        endpoint.read_frame(
            &mut frame,
            3,
            ExchangeDeadline::start(Duration::from_millis(20)).unwrap()
        ),
        Err(NativeFailure::UnsafeNativeState)
    );
}

#[test]
fn confirmation_matches_independent_hkdf_hmac_sha256_vector() {
    let transcript = StartupTranscript::recognize_ready(&literal_ready()).unwrap();
    let macs = transcript.confirmation_macs(&[7; 32], &[11; 32]).unwrap();
    let confirmation = [
        0x7f, 0x10, 0x54, 0xc5, 0x81, 0x90, 0xbb, 0x19, 0x6c, 0x07, 0xf2, 0x39, 0x51, 0x9b, 0x9e,
        0x59, 0x4c, 0x9e, 0x92, 0x3e, 0x58, 0xbb, 0x1b, 0xed, 0xb8, 0x5b, 0xa5, 0x1b, 0x02, 0x77,
        0x4d, 0xd2,
    ];
    let acknowledgement = [
        0xaf, 0x7b, 0x36, 0x50, 0x07, 0x5f, 0xd5, 0x35, 0xc4, 0xee, 0x60, 0xb0, 0xec, 0x66, 0x08,
        0x00, 0xbd, 0xa5, 0xa2, 0x1c, 0xb2, 0xb7, 0xa7, 0x43, 0x3d, 0x3a, 0x28, 0xcf, 0xf2, 0xa6,
        0x5a, 0x2b,
    ];
    assert_eq!(macs.confirmation(), &confirmation);
    assert_eq!(macs.acknowledgement(), &acknowledgement);
}

#[test]
fn successful_reaping_permanently_invalidates_exact_child() {
    let mut child = Some(helper_child("0.03"));
    let identity = process_identity(child.as_ref().unwrap().id()).unwrap();
    let mut exact = ExactChild::take(&mut child, identity).unwrap();
    assert!(child.is_none());
    assert!(ExactChild::take(&mut child, identity).is_err());
    assert_eq!(
        exact.reap(ExchangeDeadline::start(Duration::from_millis(300)).unwrap()),
        Ok(true)
    );
    assert_eq!(
        exact.reap(ExchangeDeadline::start(Duration::from_millis(20)).unwrap()),
        Err(NativeFailure::UnsafeNativeState)
    );
    drop(child);
}

#[test]
fn receiver_rejects_a_transcript_for_another_startup_before_readiness() {
    let mut owned = Some(helper_child("0.03"));
    let retriever = process_identity(owned.as_ref().unwrap().id()).unwrap();
    let exact = ExactChild::take(&mut owned, retriever).unwrap();
    let original = process_identity(std::process::id()).unwrap();
    let mut ready = literal_ready();
    for (offset, process) in [(272, original), (296, retriever)] {
        ready[offset..offset + 8].copy_from_slice(&u64::from(process.pid).to_be_bytes());
        ready[offset + 8..offset + 16].copy_from_slice(&process.start_seconds.to_be_bytes());
        ready[offset + 16..offset + 20].copy_from_slice(&process.start_microseconds.to_be_bytes());
    }
    ready[280..288].copy_from_slice(&(original.start_seconds + 1).to_be_bytes());
    let transcript = StartupTranscript::recognize_ready(&ready).unwrap();
    let (rr, rw) = anonymous_pipe().unwrap();
    let (dr, dw) = anonymous_pipe().unwrap();
    let (cr, cw) = anonymous_pipe().unwrap();
    let (ar, aw) = anonymous_pipe().unwrap();
    drop((dw, aw));
    let result = receive(
        &transcript,
        ReceiverPipes::take(rw, dr, cw, ar).unwrap(),
        exact,
        ExchangeDeadline::start(Duration::from_millis(300)).unwrap(),
    )
    .map(|_| ());
    drop((rr, cr));
    assert_eq!(result, Err(NativeFailure::ProcessIdentityMismatch));
}

#[path = "native_handoff/cleanup_exit.rs"]
mod cleanup_exit;
