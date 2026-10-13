use spaceterm_host::bootstrap::{
    command::{MAX_PROBE_BYTES, read_only_probe_script},
    process::{
        BootstrapProbeFailure, BootstrapProbeFuture, BootstrapProbeLease, BootstrapProbeTransport,
        read_only_preflight,
    },
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
struct Lease(Arc<AtomicBool>);
impl BootstrapProbeLease for Lease {
    fn account_uid(&self) -> u32 {
        1000
    }
    fn revalidate(&self) -> Result<(), BootstrapProbeFailure> {
        if self.0.load(Ordering::Acquire) {
            Err(BootstrapProbeFailure::RouteChanged)
        } else {
            Ok(())
        }
    }
}
struct ChangedProbe(Arc<AtomicBool>);
impl BootstrapProbeTransport for ChangedProbe {
    fn run_read_only<'a>(
        &'a self,
        _: &'a dyn BootstrapProbeLease,
        script: &'static [u8],
        maximum: usize,
    ) -> BootstrapProbeFuture<'a, Vec<u8>> {
        Box::pin(async move {
            assert_eq!(script, read_only_probe_script());
            assert_eq!(maximum, MAX_PROBE_BYTES);
            self.0.store(true, Ordering::Release);
            Ok(Vec::new())
        })
    }
}
#[tokio::test]
async fn probe_changed_route_discards_completion_before_parsing() {
    let changed = Arc::new(AtomicBool::new(false));
    let ssh = Lease(changed.clone());
    assert_eq!(
        read_only_preflight(&ChangedProbe(changed), &ssh)
            .await
            .err()
            .unwrap(),
        BootstrapProbeFailure::RouteChanged
    );
}
#[cfg(unix)]
struct OwnedGroup {
    child: std::process::Child,
    reaped: Arc<AtomicBool>,
}
#[cfg(unix)]
impl Drop for OwnedGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        self.child.wait().unwrap();
        self.reaped.store(true, Ordering::Release);
    }
}
#[cfg(unix)]
struct PendingProbe {
    reaped: Arc<AtomicBool>,
    started: tokio::sync::Notify,
}
#[cfg(unix)]
impl BootstrapProbeTransport for PendingProbe {
    fn run_read_only<'a>(
        &'a self,
        _: &'a dyn BootstrapProbeLease,
        _: &'static [u8],
        _: usize,
    ) -> BootstrapProbeFuture<'a, Vec<u8>> {
        Box::pin(async move {
            use std::os::unix::process::CommandExt;
            let child = std::process::Command::new("/bin/sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap();
            let _owned = OwnedGroup {
                child,
                reaped: self.reaped.clone(),
            };
            self.started.notify_one();
            std::future::pending().await
        })
    }
}
#[cfg(unix)]
#[tokio::test]
async fn bootstrap_cancellation_reaps_owned_ssh_process_group() {
    // The uncontrolled OpenSSH process edge is represented by a disposable unprivileged group.
    let reaped = Arc::new(AtomicBool::new(false));
    let transport = Arc::new(PendingProbe {
        reaped: reaped.clone(),
        started: tokio::sync::Notify::new(),
    });
    let worker = transport.clone();
    let ssh = Lease(Arc::new(AtomicBool::new(false)));
    let task = tokio::spawn(async move { read_only_preflight(&*worker, &ssh).await });
    transport.started.notified().await;
    task.abort();
    assert!(task.await.err().unwrap().is_cancelled());
    assert!(reaped.load(Ordering::Acquire));
}
