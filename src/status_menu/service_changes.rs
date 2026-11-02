//! Learns that the local service changed without a link to its server: a stopped, started,
//! crashed, installed or reconfigured server changes the service's directories or ends the
//! server's process. The status item reads the service after each such change, and never on a
//! timer, so it stays current in a menu-only client and beside a server that refuses admission.
use std::path::{Path, PathBuf};

/// What woke a [`Watch`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fired {
    /// An entry was added, removed or renamed in the directory at this index, or it went away.
    Directory(usize),
    /// The watched process exited.
    Process,
}

/// A platform's blocking watch over some directories' entries and one process's exit.
pub(crate) trait Watch {
    /// Blocks until something watched changes. Returns `None` when watching failed.
    fn wait(&mut self) -> Option<Fired>;
}

/// Starts a platform watch over `directories` and, when given, the exit of `process`. A process
/// that already exited is not watched.
pub(crate) type StartWatch = fn(&[PathBuf], Option<u32>) -> Option<Box<dyn Watch>>;

/// Calls `changed` after each change to the service at `directories` or to its server's process,
/// on a thread that blocks in the platform watch between changes. `process` reads the running
/// server's process. The thread ends when `changed` returns `false` or watching fails.
pub(crate) fn follow(
    start: StartWatch,
    directories: Vec<PathBuf>,
    process: impl Fn() -> Option<u32> + Send + 'static,
    changed: impl Fn() -> bool + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("server-service-changes".into())
        .spawn(move || run(start, &directories, &process, &changed))
        .map(drop)
}

fn run(
    start: StartWatch,
    directories: &[PathBuf],
    process: &dyn Fn() -> Option<u32>,
    changed: &dyn Fn() -> bool,
) {
    let mut first = true;
    // A process the watch saw exit is not watched again while the service manager still names it.
    let mut exited = None;
    loop {
        // Watch before reporting, so a change after the read the report causes wakes the watch.
        let watched: Vec<Watched> = directories
            .iter()
            .filter_map(|path| Watched::of(path))
            .collect();
        let paths: Vec<PathBuf> = watched.iter().map(|watched| watched.path()).collect();
        let server = process().filter(|server| Some(*server) != exited);
        let Some(mut watch) = start(&paths, server) else {
            return;
        };
        if !first && !changed() {
            return;
        }
        first = false;
        loop {
            match watch.wait() {
                None => return,
                Some(Fired::Process) => {
                    exited = server;
                    break;
                }
                // A missing directory's parent changes for unrelated entries too. Only the
                // directory appearing is a change to the service.
                Some(Fired::Directory(index)) => match watched.get(index) {
                    Some(Watched::Parent { target, .. }) if !target.is_dir() => continue,
                    _ => break,
                },
            }
        }
    }
}

/// How one service directory is watched: itself while it exists, otherwise its parent until it
/// appears.
enum Watched {
    Directory(PathBuf),
    Parent { parent: PathBuf, target: PathBuf },
}

impl Watched {
    /// `None` when neither the directory nor its parent exists, so there is nothing to watch.
    fn of(target: &Path) -> Option<Self> {
        if target.is_dir() {
            return Some(Self::Directory(target.to_path_buf()));
        }
        let parent = target.parent().filter(|parent| parent.is_dir())?;
        Some(Self::Parent {
            parent: parent.to_path_buf(),
            target: target.to_path_buf(),
        })
    }

    fn path(&self) -> PathBuf {
        match self {
            Self::Directory(path) | Self::Parent { parent: path, .. } => path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    /// Long enough for a real watch to report a change; a missing report fails within it.
    const PROMPT: Duration = Duration::from_secs(5);
    /// How long the test waits to show that something caused no report.
    const QUIET: Duration = Duration::from_millis(300);

    struct Fixture {
        root: PathBuf,
        data: PathBuf,
        runtime: PathBuf,
        server: Arc<Mutex<Option<u32>>>,
        reports: mpsc::Receiver<()>,
    }

    fn fixture() -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-service-changes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data = root.join("data/server");
        std::fs::create_dir_all(&data).unwrap();
        let runtime = root.join("runtime");
        let server = Arc::new(Mutex::new(None));
        let (report, reports) = mpsc::channel();
        let process = {
            let server = Arc::clone(&server);
            move || *server.lock().unwrap()
        };
        follow(
            crate::platform::service_watch,
            vec![data.clone(), runtime.clone()],
            process,
            move || report.send(()).is_ok(),
        )
        .unwrap();
        std::thread::sleep(QUIET);
        Fixture {
            root,
            data,
            runtime,
            server,
            reports,
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Fixture {
        fn reported(&self, what: &str) {
            self.reports
                .recv_timeout(PROMPT)
                .unwrap_or_else(|_| panic!("{what} is reported"));
            std::thread::sleep(QUIET);
            while self.reports.try_recv().is_ok() {}
        }

        fn quiet(&self, what: &str) {
            assert!(
                self.reports.recv_timeout(QUIET).is_err(),
                "{what} is not reported"
            );
        }
    }

    /// A server another client or the service manager starts and stops is reported with no link
    /// to it: its runtime directory appearing, its endpoint appearing and going away, its
    /// process ending, and a policy write in its data directory. Nothing else is reported.
    #[test]
    fn external_starts_stops_and_exits_are_reported_without_a_link() {
        let fixture = fixture();
        fixture.quiet("an idle service");

        std::fs::write(fixture.runtime.with_file_name("unrelated"), b"").unwrap();
        fixture.quiet("an unrelated entry beside the missing runtime directory");

        std::fs::create_dir(&fixture.runtime).unwrap();
        fixture.reported("the runtime directory appearing");

        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        *fixture.server.lock().unwrap() = Some(child.id());
        std::fs::write(fixture.runtime.join("s"), b"").unwrap();
        fixture.reported("a started server's endpoint");
        fixture.quiet("a running server");

        child.kill().unwrap();
        child.wait().unwrap();
        fixture.reported("the server's process ending");
        *fixture.server.lock().unwrap() = None;

        std::fs::remove_file(fixture.runtime.join("s")).unwrap();
        fixture.reported("a stopped server's endpoint going away");

        std::fs::write(fixture.data.join("pending"), b"{}").unwrap();
        std::fs::rename(
            fixture.data.join("pending"),
            fixture.data.join("service.json"),
        )
        .unwrap();
        fixture.reported("a policy write");
        fixture.quiet("an unchanged service");
    }
}
