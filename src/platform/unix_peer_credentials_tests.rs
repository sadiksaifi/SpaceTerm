//! A separately executed peer owns the listener, so caller credentials cannot pass authentication.

use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const CHILD_DIRECTORY: &str = "SPACETERM_TEST_PEER_DIRECTORY";
const CHILD_TEST: &str = "platform::unix_peer_credentials_tests::peer_listener_child";
const TIMEOUT: Duration = Duration::from_secs(10);
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

pub(super) struct ChildListener {
    child: Child,
    directory: PathBuf,
    socket: PathBuf,
}

impl ChildListener {
    pub(super) fn new() -> Self {
        let directory = std::fs::canonicalize("/tmp").unwrap().join(format!(
            "st-peer-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--ignored"])
            .env(CHILD_DIRECTORY, &directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut listener = Self {
            child,
            socket: directory.join("peer.sock"),
            directory,
        };
        let deadline = Instant::now() + TIMEOUT;
        while !listener.directory.join("ready").exists() {
            assert!(
                listener.child.try_wait().unwrap().is_none(),
                "peer exited before listening"
            );
            assert!(Instant::now() < deadline, "peer did not start listening");
            std::thread::sleep(Duration::from_millis(5));
        }
        listener
    }

    pub(super) fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub(super) fn process(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn finish(mut self) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "peer received unexpected request bytes");
                return;
            }
            assert!(Instant::now() < deadline, "peer did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for ChildListener {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
#[ignore = "child-process fixture invoked by the peer authentication tests"]
fn peer_listener_child() {
    let directory =
        PathBuf::from(std::env::var_os(CHILD_DIRECTORY).expect("peer fixture directory"));
    let listener = UnixListener::bind(directory.join("peer.sock")).unwrap();
    std::fs::write(directory.join("ready"), []).unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    let mut request = Vec::new();
    stream.read_to_end(&mut request).unwrap();
    assert!(
        request.is_empty(),
        "authentication must precede request writes"
    );
}
