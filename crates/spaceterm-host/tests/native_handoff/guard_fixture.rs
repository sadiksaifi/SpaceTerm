//! Standalone single-threaded peer. Do not run a post-fork Rust retriever from
//! the multithreaded libtest harness. No credential or privileged operation runs.

use spaceterm_host::{
    native_deployment::contracts::NativeFailure,
    native_receiver::guard::run_guarded_retriever,
    startup_exchange::{
        ExchangeDeadline,
        unix::{ExactChild, LockedMemory, PipeEndpoint, anonymous_pipe, process_identity},
    },
};
use std::{
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn fork() -> i32;
    fn write(fd: i32, data: *const u8, length: usize) -> isize;
    fn read(fd: i32, data: *mut u8, length: usize) -> isize;
    fn _exit(status: i32) -> !;
}

fn guarded(timeout: bool) -> bool {
    let (lifetime_read, writer) = anonymous_pipe().unwrap();
    let (start_read, start_write) = anonymous_pipe().unwrap();
    let start = Instant::now();
    let pid = unsafe { fork() };
    if pid < 0 {
        return false;
    }
    if pid == 0 {
        drop(lifetime_read);
        drop(start_write);
        let mut byte = 0;
        if unsafe { read(start_read.as_raw_fd(), &mut byte, 1) } != 1 {
            unsafe { _exit(1) }
        }
        drop(start_read);
        unsafe {
            run_guarded_retriever(|_| {
                if !timeout {
                    return Ok(());
                }
                // The fixed worker retains this writer throughout its blocked life.
                // EOF proves that both blocked processes closed their descriptors.
                let worker = fork();
                if worker < 0 {
                    return Err(NativeFailure::NativeUnavailable);
                }
                if worker == 0 {
                    std::thread::sleep(Duration::from_secs(60));
                    _exit(1);
                }
                if write(writer.as_raw_fd(), b"w".as_ptr(), 1) != 1 {
                    return Err(NativeFailure::IoUnavailable);
                }
                std::thread::sleep(Duration::from_secs(60));
                Err(NativeFailure::TimedOut)
            })
        }
    }
    drop(writer);
    drop(start_read);
    let identity = match process_identity(pid as u32) {
        Ok(identity) => identity,
        Err(_) => return false,
    };
    // This standalone main owns the fork reservation and installs no other waiter.
    let mut child = unsafe { ExactChild::inherit(identity) }.unwrap();
    assert_eq!(
        unsafe { write(start_write.as_raw_fd(), b"s".as_ptr(), 1) },
        1
    );
    drop(start_write);
    let waited = child.reap(ExchangeDeadline::start(Duration::from_secs(2)).unwrap());
    // A timeout never renews admission. Only this test's cleanup wait may extend.
    let waited = if waited.is_err() {
        child.reap(ExchangeDeadline::start(Duration::from_secs(2)).unwrap())
    } else {
        waited
    };
    let mut frame = LockedMemory::<2>::allocate().unwrap();
    let eof = PipeEndpoint::take(lifetime_read, false)
        .unwrap()
        .read_frame(
            &mut frame,
            if timeout { 1 } else { 0 },
            ExchangeDeadline::start(Duration::from_secs(1)).unwrap(),
        );
    if timeout {
        waited == Ok(false)
            && eof == Ok(())
            && frame.bytes()[0] == b'w'
            && start.elapsed() >= Duration::from_millis(1500)
            && start.elapsed() < Duration::from_secs(4)
    } else {
        waited == Ok(true) && eof == Ok(())
    }
}
fn main() {
    let cases = [
        ("retriever-guard-exact-retirement", guarded(false)),
        ("retriever-timeout-retires-fixed-worker", guarded(true)),
    ];
    for (case, passed) in cases {
        println!(
            "{{\"schema\":1,\"scope\":\"startup-exchange-composition\",\"case\":\"{case}\",\"result\":\"{}\"}}",
            if passed { "pass" } else { "red" }
        );
    }
    std::process::exit(i32::from(cases.iter().any(|(_, passed)| !passed)));
}
