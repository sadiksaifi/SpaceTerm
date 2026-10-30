//! The Command Line Tool: this build identity's launcher in the account's own `~/.local/bin`.
//!
//! Installing creates the link `install.sh` creates, through the same service code, so it needs
//! no administrator and asks for no password. It never replaces a file or link that another
//! program put at the launcher path. SpaceTerm runs no shell and edits no shell profile to learn
//! or change PATH, so it makes no claim about which command a shell finds. Reads and installs
//! block, so callers run them off the UI thread.
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

/// What the launcher path holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Launcher {
    Missing,
    /// SpaceTerm's link, naming a server build this account keeps.
    Installed,
    /// SpaceTerm's link, naming a server build that is gone. Installing replaces it.
    Stale,
    /// SpaceTerm's link, naming a kept server build other than the one this account's server
    /// uses, such as one an update couldn't repoint. Installing replaces it.
    Outdated,
    /// A link SpaceTerm didn't create. It is never replaced.
    OtherLink,
    /// A file, directory or other entry that is not a link. It is never replaced.
    OtherFile,
    /// SpaceTerm's link needs replacing, but another program's entry holds the temporary name
    /// replacement uses. That entry is never removed.
    Blocked,
}

impl Launcher {
    /// The one action each state offers: Install where nothing usable is there, and Reveal
    /// wherever a file is.
    pub(crate) fn action(self) -> ToolAction {
        match self {
            Self::Missing | Self::Stale | Self::Outdated => ToolAction::Install,
            Self::Installed | Self::OtherLink | Self::OtherFile | Self::Blocked => {
                ToolAction::Reveal
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolAction {
    Install,
    /// Shows the launcher in the file manager, or for [`Launcher::Blocked`] the entry that
    /// blocks it.
    Reveal,
}

/// Why a read, install or reveal didn't complete. Classifications only: no path, environment
/// value or native error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolFailure {
    /// Another server operation, such as an update, holds the service.
    Busy,
    /// This build includes no server package, and this account has no approved server build.
    NoServerPackage,
    /// The server package changed after SpaceTerm opened.
    RestartRequired,
    /// The server package or the approved build failed verification.
    Unverified,
    /// The launcher directory or server storage has permissions SpaceTerm doesn't accept.
    UnsafeStorage,
    /// The launcher directory or server storage couldn't be read or written.
    StorageUnavailable,
    /// Another program's file or link is at the launcher path, so nothing was installed.
    Occupied,
    /// Another program's entry holds the temporary name replacement uses, so nothing changed.
    Blocked,
    /// The file manager is missing, refused or didn't answer.
    RevealUnavailable,
}

/// The outcome of linking the launcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkOutcome {
    Linked,
    /// Another program's file or link was there, or arrived meanwhile, and stays.
    Kept,
    /// Another program's entry holds the temporary name, and stays.
    Blocked,
}

/// The file manager's answer to a reveal.
pub(crate) type Reveal = Pin<Box<dyn Future<Output = Result<(), ToolFailure>> + Send>>;

/// The account's launcher files.
pub(crate) trait LauncherFiles: Send + Sync {
    /// What the launcher path holds, read without following it.
    fn read(&self) -> Result<Launcher, ToolFailure>;
    /// Links the launcher as the installer does, deciding again what the path holds.
    fn link(&self) -> Result<LinkOutcome, ToolFailure>;
    /// The launcher's path.
    fn launcher(&self) -> PathBuf;
    /// The temporary name a replacement links from.
    fn temporary(&self) -> PathBuf;
}

/// The desktop's file manager.
pub(crate) trait FileViewer: Send + Sync {
    /// Asks it to show `path` selected in its folder. Called on the UI thread, which it never
    /// blocks; the returned future settles with the file manager's answer.
    fn reveal(&self, path: &Path) -> Reveal;
}

/// The host capabilities a [`CommandLineTool`] uses.
#[derive(Clone)]
pub(crate) struct CommandLineToolAdapters {
    pub(crate) files: Arc<dyn LauncherFiles>,
    pub(crate) viewer: Arc<dyn FileViewer>,
}

/// The launcher of one build identity, such as `spaceterm-development`.
pub(crate) struct CommandLineTool {
    name: &'static str,
    adapters: CommandLineToolAdapters,
}

impl CommandLineTool {
    pub(crate) fn new(name: &'static str, adapters: CommandLineToolAdapters) -> Self {
        Self { name, adapters }
    }

    /// The command people type, which is the launcher's file name.
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Reads what the launcher path holds. Blocks.
    pub(crate) fn status(&self) -> Result<Launcher, ToolFailure> {
        self.adapters.files.read()
    }

    /// Installs the launcher unless another program's file or link is there, and reports the
    /// launcher afterward. An installed launcher is left as it is. Blocks.
    pub(crate) fn install(&self) -> Result<Launcher, ToolFailure> {
        match self.adapters.files.read()? {
            Launcher::Installed => {}
            Launcher::OtherLink | Launcher::OtherFile => return Err(ToolFailure::Occupied),
            Launcher::Blocked => return Err(ToolFailure::Blocked),
            Launcher::Missing | Launcher::Stale | Launcher::Outdated => {
                match self.adapters.files.link()? {
                    LinkOutcome::Linked => {}
                    LinkOutcome::Kept => return Err(ToolFailure::Occupied),
                    LinkOutcome::Blocked => return Err(ToolFailure::Blocked),
                }
            }
        }
        self.status()
    }

    /// Shows `launcher`'s entry in the file manager: the entry blocking a blocked launcher, and
    /// the launcher otherwise. Only a launcher whose action is Reveal offers this.
    pub(crate) fn reveal(&self, launcher: Launcher) -> Reveal {
        let path = if launcher == Launcher::Blocked {
            self.adapters.files.temporary()
        } else {
            self.adapters.files.launcher()
        };
        self.adapters.viewer.reveal(&path)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(crate) const DIRECTORY: &str = "/home/someone/.local/bin";

    /// Launcher files whose contents the test sets and whose links it counts.
    pub(crate) struct ScriptedFiles {
        pub(crate) launcher: Mutex<Result<Launcher, ToolFailure>>,
        /// What linking returns. A successful link leaves the launcher installed; a kept one
        /// found another program's link there.
        pub(crate) link: Mutex<Result<LinkOutcome, ToolFailure>>,
        pub(crate) links: AtomicUsize,
        pub(crate) reads: AtomicUsize,
    }

    impl ScriptedFiles {
        pub(crate) fn new(launcher: Result<Launcher, ToolFailure>) -> Arc<Self> {
            Arc::new(Self {
                launcher: Mutex::new(launcher),
                link: Mutex::new(Ok(LinkOutcome::Linked)),
                links: AtomicUsize::new(0),
                reads: AtomicUsize::new(0),
            })
        }

        pub(crate) fn links(&self) -> usize {
            self.links.load(Ordering::SeqCst)
        }
    }

    impl LauncherFiles for ScriptedFiles {
        fn read(&self) -> Result<Launcher, ToolFailure> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            *self.launcher.lock().unwrap()
        }
        fn link(&self) -> Result<LinkOutcome, ToolFailure> {
            self.links.fetch_add(1, Ordering::SeqCst);
            let outcome = *self.link.lock().unwrap();
            match outcome {
                Ok(LinkOutcome::Linked) => *self.launcher.lock().unwrap() = Ok(Launcher::Installed),
                Ok(LinkOutcome::Kept) => *self.launcher.lock().unwrap() = Ok(Launcher::OtherLink),
                Ok(LinkOutcome::Blocked) => *self.launcher.lock().unwrap() = Ok(Launcher::Blocked),
                Err(_) => {}
            }
            outcome
        }
        fn launcher(&self) -> PathBuf {
            Path::new(DIRECTORY).join("spaceterm-development")
        }
        fn temporary(&self) -> PathBuf {
            Path::new(DIRECTORY).join(".launcher-spaceterm-development")
        }
    }

    /// A file manager that records what it was asked to show and answers as the test sets.
    #[derive(Default)]
    pub(crate) struct RecordingViewer {
        pub(crate) shown: Mutex<Vec<PathBuf>>,
        pub(crate) refuse: bool,
    }

    impl FileViewer for RecordingViewer {
        fn reveal(&self, path: &Path) -> Reveal {
            self.shown.lock().unwrap().push(path.to_owned());
            let answer = if self.refuse {
                Err(ToolFailure::RevealUnavailable)
            } else {
                Ok(())
            };
            Box::pin(std::future::ready(answer))
        }
    }

    pub(crate) fn tool(
        files: &Arc<ScriptedFiles>,
        viewer: &Arc<RecordingViewer>,
    ) -> CommandLineTool {
        CommandLineTool::new(
            "spaceterm-development",
            CommandLineToolAdapters {
                files: Arc::clone(files) as Arc<dyn LauncherFiles>,
                viewer: Arc::clone(viewer) as Arc<dyn FileViewer>,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn fixture(launcher: Launcher) -> (Arc<ScriptedFiles>, Arc<RecordingViewer>, CommandLineTool) {
        let files = ScriptedFiles::new(Ok(launcher));
        let viewer = Arc::new(RecordingViewer::default());
        let tool = tool(&files, &viewer);
        (files, viewer, tool)
    }

    #[test]
    fn installing_links_a_missing_stale_or_outdated_launcher_and_reports_it_installed() {
        for launcher in [Launcher::Missing, Launcher::Stale, Launcher::Outdated] {
            let (files, _, tool) = fixture(launcher);
            assert_eq!(tool.status().map(Launcher::action), Ok(ToolAction::Install));
            assert_eq!(tool.install(), Ok(Launcher::Installed));
            assert_eq!(files.links(), 1);
        }
    }

    #[test]
    fn another_programs_file_or_link_is_never_replaced() {
        for launcher in [Launcher::OtherLink, Launcher::OtherFile] {
            let (files, _, tool) = fixture(launcher);
            assert_eq!(tool.install(), Err(ToolFailure::Occupied));
            assert_eq!(files.links(), 0);
            assert_eq!(tool.status().map(Launcher::action), Ok(ToolAction::Reveal));
        }
        // A file that appears between the read and the link is kept by the link itself.
        let (files, _, tool) = fixture(Launcher::Missing);
        *files.link.lock().unwrap() = Ok(LinkOutcome::Kept);
        assert_eq!(tool.install(), Err(ToolFailure::Occupied));
    }

    #[test]
    fn an_installed_launcher_is_left_as_it_is() {
        let (files, _, tool) = fixture(Launcher::Installed);
        assert_eq!(tool.install(), Ok(Launcher::Installed));
        assert_eq!(files.links(), 0);
    }

    #[test]
    fn failures_pass_through_as_classifications() {
        let files = ScriptedFiles::new(Err(ToolFailure::UnsafeStorage));
        let viewer = Arc::new(RecordingViewer {
            refuse: true,
            ..RecordingViewer::default()
        });
        let tool = tool(&files, &viewer);
        assert_eq!(tool.status(), Err(ToolFailure::UnsafeStorage));
        assert_eq!(tool.install(), Err(ToolFailure::UnsafeStorage));
        assert_eq!(
            futures_lite::future::block_on(tool.reveal(Launcher::Installed)),
            Err(ToolFailure::RevealUnavailable)
        );
        *files.launcher.lock().unwrap() = Ok(Launcher::Missing);
        *files.link.lock().unwrap() = Err(ToolFailure::Busy);
        assert_eq!(tool.install(), Err(ToolFailure::Busy));
    }

    #[test]
    fn a_blocked_launcher_is_never_linked_and_reveals_the_entry_that_blocks_it() {
        let (files, viewer, tool) = fixture(Launcher::Outdated);
        *files.link.lock().unwrap() = Ok(LinkOutcome::Blocked);
        assert_eq!(tool.install(), Err(ToolFailure::Blocked));
        assert_eq!(tool.status(), Ok(Launcher::Blocked));
        assert_eq!(tool.install(), Err(ToolFailure::Blocked));
        assert_eq!(files.links(), 1);
        assert_eq!(
            futures_lite::future::block_on(tool.reveal(Launcher::Blocked)),
            Ok(())
        );
        assert_eq!(
            *viewer.shown.lock().unwrap(),
            [Path::new(DIRECTORY).join(".launcher-spaceterm-development")]
        );
    }

    #[test]
    fn reveal_shows_the_launcher_itself() {
        let (_, viewer, tool) = fixture(Launcher::Installed);
        assert_eq!(
            futures_lite::future::block_on(tool.reveal(Launcher::Installed)),
            Ok(())
        );
        assert_eq!(
            *viewer.shown.lock().unwrap(),
            [Path::new(DIRECTORY).join("spaceterm-development")]
        );
    }
}
