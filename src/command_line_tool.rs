//! The Command Line Tool: this build identity's launcher in the account's own `~/.local/bin`.
//!
//! Installing creates the link `install.sh` creates, through the same service code, so it needs
//! no administrator and asks for no password. It never replaces a file or link that another
//! program put at the launcher path, and it never edits a shell profile: a launcher directory
//! that new shells don't search is reported instead. Reads and installs block, so callers run
//! them off the UI thread.
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// What the launcher path holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Launcher {
    Missing,
    /// SpaceTerm's link, naming a server build this account keeps.
    Installed,
    /// SpaceTerm's link, naming a server build that is gone. Installing replaces it.
    Stale,
    /// A link SpaceTerm didn't create. It is never replaced.
    OtherLink,
    /// A file, directory or other entry that is not a link. It is never replaced.
    OtherFile,
}

/// Whether a new login shell searches the launcher's directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchPath {
    Included,
    Excluded,
    /// The shell's PATH couldn't be learned.
    Unknown,
}

/// The launcher and whether shells find it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ToolStatus {
    pub(crate) launcher: Launcher,
    pub(crate) search_path: SearchPath,
}

impl ToolStatus {
    /// The one action this status offers: Install where nothing usable is there, and Reveal
    /// wherever a file is.
    pub(crate) fn action(self) -> ToolAction {
        match self.launcher {
            Launcher::Missing | Launcher::Stale => ToolAction::Install,
            Launcher::Installed | Launcher::OtherLink | Launcher::OtherFile => ToolAction::Reveal,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolAction {
    Install,
    /// Shows the launcher in the file manager.
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
    /// The file manager couldn't show the launcher.
    RevealUnavailable,
}

/// The outcome of linking the launcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkOutcome {
    Linked,
    /// Another program's file or link was there and stays.
    Kept,
}

/// The account's launcher files.
pub(crate) trait LauncherFiles: Send + Sync {
    /// What the launcher path holds, read without following it.
    fn read(&self) -> Result<Launcher, ToolFailure>;
    /// Links the launcher as the installer does, deciding again what the path holds.
    fn link(&self) -> Result<LinkOutcome, ToolFailure>;
    /// The launcher's path.
    fn launcher(&self) -> PathBuf;
    /// The directory holding the launcher, in the form PATH entries are compared with.
    fn directory(&self) -> PathBuf;
}

/// The directories a new login shell searches for commands.
pub(crate) trait ShellSearchPath: Send + Sync {
    /// In search order, in the form [`LauncherFiles::directory`] uses. `None` when the shell's
    /// PATH can't be learned.
    fn directories(&self) -> Option<Vec<PathBuf>>;
}

/// The desktop's file manager.
pub(crate) trait FileViewer: Send + Sync {
    /// Shows `path` selected in its folder. Called on the UI thread; never waits for the viewer.
    fn reveal(&self, path: &Path) -> Result<(), ToolFailure>;
}

/// The host capabilities one [`CommandLineTool`] uses.
#[derive(Clone)]
pub(crate) struct CommandLineToolAdapters {
    pub(crate) files: Arc<dyn LauncherFiles>,
    pub(crate) search_path: Arc<dyn ShellSearchPath>,
    pub(crate) viewer: Arc<dyn FileViewer>,
}

/// The launcher of one build identity, such as `spaceterm-development`.
pub(crate) struct CommandLineTool {
    name: &'static str,
    adapters: CommandLineToolAdapters,
    /// Learned once: SpaceTerm never changes the shell's PATH.
    search_path: OnceLock<SearchPath>,
}

impl CommandLineTool {
    pub(crate) fn new(name: &'static str, adapters: CommandLineToolAdapters) -> Self {
        Self {
            name,
            adapters,
            search_path: OnceLock::new(),
        }
    }

    /// The command people type, which is the launcher's file name.
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Reads the launcher and whether shells search its directory. Blocks.
    pub(crate) fn status(&self) -> Result<ToolStatus, ToolFailure> {
        let launcher = self.adapters.files.read()?;
        Ok(ToolStatus {
            launcher,
            search_path: self.search_path(),
        })
    }

    /// Installs the launcher unless another program's file or link is there, and reports the
    /// status afterward. An installed launcher is left as it is. Blocks.
    pub(crate) fn install(&self) -> Result<ToolStatus, ToolFailure> {
        match self.adapters.files.read()? {
            Launcher::Installed => {}
            Launcher::OtherLink | Launcher::OtherFile => return Err(ToolFailure::Occupied),
            Launcher::Missing | Launcher::Stale => {
                if self.adapters.files.link()? == LinkOutcome::Kept {
                    return Err(ToolFailure::Occupied);
                }
            }
        }
        self.status()
    }

    /// Shows the launcher in the file manager. Only a status whose action is Reveal offers this.
    pub(crate) fn reveal(&self) -> Result<(), ToolFailure> {
        self.adapters.viewer.reveal(&self.adapters.files.launcher())
    }

    fn search_path(&self) -> SearchPath {
        *self.search_path.get_or_init(|| {
            let directory = self.adapters.files.directory();
            match self.adapters.search_path.directories() {
                Some(directories) if directories.contains(&directory) => SearchPath::Included,
                Some(_) => SearchPath::Excluded,
                None => SearchPath::Unknown,
            }
        })
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
    }

    impl ScriptedFiles {
        pub(crate) fn new(launcher: Result<Launcher, ToolFailure>) -> Arc<Self> {
            Arc::new(Self {
                launcher: Mutex::new(launcher),
                link: Mutex::new(Ok(LinkOutcome::Linked)),
                links: AtomicUsize::new(0),
            })
        }

        pub(crate) fn links(&self) -> usize {
            self.links.load(Ordering::SeqCst)
        }
    }

    impl LauncherFiles for ScriptedFiles {
        fn read(&self) -> Result<Launcher, ToolFailure> {
            *self.launcher.lock().unwrap()
        }
        fn link(&self) -> Result<LinkOutcome, ToolFailure> {
            self.links.fetch_add(1, Ordering::SeqCst);
            let outcome = *self.link.lock().unwrap();
            match outcome {
                Ok(LinkOutcome::Linked) => *self.launcher.lock().unwrap() = Ok(Launcher::Installed),
                Ok(LinkOutcome::Kept) => *self.launcher.lock().unwrap() = Ok(Launcher::OtherLink),
                Err(_) => {}
            }
            outcome
        }
        fn launcher(&self) -> PathBuf {
            Path::new(DIRECTORY).join("spaceterm-development")
        }
        fn directory(&self) -> PathBuf {
            PathBuf::from(DIRECTORY)
        }
    }

    /// A shell whose PATH the test sets, counting how often it is asked.
    pub(crate) struct ScriptedShell {
        pub(crate) directories: Option<Vec<PathBuf>>,
        pub(crate) probes: AtomicUsize,
    }

    impl ScriptedShell {
        pub(crate) fn new(directories: Option<&[&str]>) -> Arc<Self> {
            Arc::new(Self {
                directories: directories
                    .map(|directories| directories.iter().map(PathBuf::from).collect()),
                probes: AtomicUsize::new(0),
            })
        }
    }

    impl ShellSearchPath for ScriptedShell {
        fn directories(&self) -> Option<Vec<PathBuf>> {
            self.probes.fetch_add(1, Ordering::SeqCst);
            self.directories.clone()
        }
    }

    /// A file manager that records what it showed, or refuses.
    #[derive(Default)]
    pub(crate) struct RecordingViewer {
        pub(crate) shown: Mutex<Vec<PathBuf>>,
        pub(crate) refuse: bool,
    }

    impl FileViewer for RecordingViewer {
        fn reveal(&self, path: &Path) -> Result<(), ToolFailure> {
            if self.refuse {
                return Err(ToolFailure::RevealUnavailable);
            }
            self.shown.lock().unwrap().push(path.to_owned());
            Ok(())
        }
    }

    pub(crate) fn tool(
        files: &Arc<ScriptedFiles>,
        shell: &Arc<ScriptedShell>,
        viewer: &Arc<RecordingViewer>,
    ) -> CommandLineTool {
        CommandLineTool::new(
            "spaceterm-development",
            CommandLineToolAdapters {
                files: Arc::clone(files) as Arc<dyn LauncherFiles>,
                search_path: Arc::clone(shell) as Arc<dyn ShellSearchPath>,
                viewer: Arc::clone(viewer) as Arc<dyn FileViewer>,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::sync::atomic::Ordering;

    fn fixture(
        launcher: Launcher,
        path: Option<&[&str]>,
    ) -> (
        Arc<ScriptedFiles>,
        Arc<ScriptedShell>,
        Arc<RecordingViewer>,
        CommandLineTool,
    ) {
        let files = ScriptedFiles::new(Ok(launcher));
        let shell = ScriptedShell::new(path);
        let viewer = Arc::new(RecordingViewer::default());
        let tool = tool(&files, &shell, &viewer);
        (files, shell, viewer, tool)
    }

    #[test]
    fn installing_links_a_missing_or_stale_launcher_and_reports_it_installed() {
        for launcher in [Launcher::Missing, Launcher::Stale] {
            let (files, _, _, tool) = fixture(launcher, Some(&["/usr/bin", DIRECTORY]));
            assert_eq!(
                tool.status().map(ToolStatus::action),
                Ok(ToolAction::Install)
            );
            assert_eq!(
                tool.install(),
                Ok(ToolStatus {
                    launcher: Launcher::Installed,
                    search_path: SearchPath::Included,
                })
            );
            assert_eq!(files.links(), 1);
        }
    }

    #[test]
    fn another_programs_file_or_link_is_never_replaced() {
        for launcher in [Launcher::OtherLink, Launcher::OtherFile] {
            let (files, _, _, tool) = fixture(launcher, None);
            assert_eq!(tool.install(), Err(ToolFailure::Occupied));
            assert_eq!(files.links(), 0);
            assert_eq!(
                tool.status().map(ToolStatus::action),
                Ok(ToolAction::Reveal)
            );
        }
        // A file that appears between the read and the link is kept by the link itself.
        let (files, _, _, tool) = fixture(Launcher::Missing, None);
        *files.link.lock().unwrap() = Ok(LinkOutcome::Kept);
        assert_eq!(tool.install(), Err(ToolFailure::Occupied));
    }

    #[test]
    fn an_installed_launcher_is_left_as_it_is() {
        let (files, _, _, tool) = fixture(Launcher::Installed, None);
        assert_eq!(
            tool.install().map(|status| status.launcher),
            Ok(Launcher::Installed)
        );
        assert_eq!(files.links(), 0);
    }

    #[test]
    fn the_shells_path_decides_whether_the_launcher_is_found() {
        let excluded = fixture(
            Launcher::Installed,
            Some(&["/usr/bin", "/home/someone/.local"]),
        );
        assert_eq!(
            excluded.3.status().map(|status| status.search_path),
            Ok(SearchPath::Excluded)
        );
        let unknown = fixture(Launcher::Installed, None);
        assert_eq!(
            unknown.3.status().map(|status| status.search_path),
            Ok(SearchPath::Unknown)
        );
        let (_, shell, _, tool) = fixture(Launcher::Missing, Some(&[DIRECTORY]));
        tool.status().unwrap();
        tool.install().unwrap();
        assert_eq!(shell.probes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failures_pass_through_as_classifications() {
        let files = ScriptedFiles::new(Err(ToolFailure::UnsafeStorage));
        let shell = ScriptedShell::new(None);
        let viewer = Arc::new(RecordingViewer {
            refuse: true,
            ..RecordingViewer::default()
        });
        let tool = tool(&files, &shell, &viewer);
        assert_eq!(tool.status(), Err(ToolFailure::UnsafeStorage));
        assert_eq!(tool.install(), Err(ToolFailure::UnsafeStorage));
        assert_eq!(tool.reveal(), Err(ToolFailure::RevealUnavailable));
        *files.launcher.lock().unwrap() = Ok(Launcher::Missing);
        *files.link.lock().unwrap() = Err(ToolFailure::Busy);
        assert_eq!(tool.install(), Err(ToolFailure::Busy));
    }

    #[test]
    fn reveal_shows_the_launcher_itself() {
        let (_, _, viewer, tool) = fixture(Launcher::Installed, None);
        tool.reveal().unwrap();
        assert_eq!(
            *viewer.shown.lock().unwrap(),
            [Path::new(DIRECTORY).join("spaceterm-development")]
        );
    }
}
