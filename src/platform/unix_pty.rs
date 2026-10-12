pub(crate) use spaceterm_host::pty::unix_pty::UnixNativePtyAdapterFactory;

#[cfg(all(test, feature = "native-tests"))]
use std::sync::Mutex;

// Real PTY cases fork nested test executables and login shells while asserting subsecond process-
// group cleanup. Serial execution keeps those OS-backed cases independent of runner scheduling.
#[cfg(all(test, feature = "native-tests"))]
static REAL_PTY_TEST_LOCK: Mutex<()> = Mutex::new(());
#[cfg(all(test, feature = "native-tests"))]
pub(crate) fn lock_real_pty_test() -> std::sync::MutexGuard<'static, ()> {
    REAL_PTY_TEST_LOCK
        .lock()
        .expect("a previous real PTY test panicked while it owned OS process resources")
}

#[cfg(all(test, feature = "native-tests"))]
pub(crate) fn isolate_real_pty_test(test_name: &str) -> bool {
    const CHILD_TEST: &str = "SPACETERM_ISOLATED_REAL_PTY_TEST";
    if std::env::var(CHILD_TEST).as_deref() == Ok(test_name) {
        eprintln!("SPACETERM_PTY_TEST_ENTERED={test_name}");
        return false;
    }

    // portable-pty forks before exec. The parallel native test process also runs Foundation
    // code, which can leave a forked child with corrupt os_once state. Run the PTY case in a
    // fresh test process before that process starts unrelated test threads.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--test-threads=1", "--nocapture"])
        .env(CHILD_TEST, test_name)
        .output()
        .expect("failed to start isolated real PTY test");
    assert!(
        output.status.success(),
        "isolated real PTY test failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .any(|line| line == format!("SPACETERM_PTY_TEST_ENTERED={test_name}")),
        "isolated real PTY test body did not execute: {test_name}"
    );
    true
}

#[cfg(all(test, feature = "native-tests"))]
pub(crate) fn test_factory() -> UnixNativePtyAdapterFactory {
    #[cfg(target_os = "macos")]
    let host = super::macos_pty_host::MacosPtyHost;
    #[cfg(target_os = "linux")]
    let host = super::linux_pty_host::LinuxPtyHost;
    UnixNativePtyAdapterFactory::new(std::sync::Arc::new(host))
}
