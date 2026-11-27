//! The Command Line Tool: this build identity's launcher in the account's own `~/.local/bin`,
//! on this computer or on a paired Machine, whose SpaceTerm Core links it over its service lane.
//!
//! Installing creates the link `install.sh` creates, through the same service code, so it needs
//! no administrator and asks for no password. It never replaces a file or link that another
//! program put at the launcher path. SpaceTerm runs no shell and edits no shell profile to learn
//! or change PATH, so it makes no claim about which command a shell finds. Removing it removes
//! only SpaceTerm's own link. Reads, installs and removals block, so callers run them off the
//! UI thread.
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
    /// The actions each state offers: Install where nothing usable is there, and Repair or
    /// Uninstall for SpaceTerm's working link. Another program's entry, and a link blocked by
    /// one, offer none, because SpaceTerm never touches that entry.
    pub(crate) fn actions(self) -> &'static [ToolAction] {
        match self {
            Self::Missing | Self::Stale | Self::Outdated => &[ToolAction::Install],
            Self::Installed => &[ToolAction::Repair, ToolAction::Uninstall],
            Self::OtherLink | Self::OtherFile | Self::Blocked => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolAction {
    /// Reads the launcher again. Offered where SpaceTerm reads it only when asked, as on a
    /// paired Machine.
    Check,
    Install,
    /// Links SpaceTerm's launcher again, as Install does.
    Repair,
    /// Removes SpaceTerm's launcher.
    Uninstall,
}

/// Why a read, install or removal didn't complete. Classifications only: no path, environment
/// value or native error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolFailure {
    /// Another server operation, such as an update, holds the service.
    Busy,
    /// This build includes no server package, and this account has no approved server build.
    NoCorePackage,
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
    /// SpaceTerm couldn't reach the paired Machine's service lane.
    Unreachable,
    /// The paired Machine didn't accept this device.
    Refused,
    /// The paired Machine's SpaceTerm Core is too old to link the launcher over its lane.
    Unsupported,
    /// The paired Machine refused or answered in a way SpaceTerm doesn't read.
    Failed,
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

/// The account's launcher files.
pub(crate) trait LauncherFiles: Send + Sync {
    /// What the launcher path holds, read without following it.
    fn read(&self) -> Result<Launcher, ToolFailure>;
    /// Links the launcher as the installer does, deciding again what the path holds.
    fn link(&self) -> Result<LinkOutcome, ToolFailure>;
    /// Removes SpaceTerm's own link, deciding again what the path holds. Another program's
    /// entry, or nothing there, is left as it is.
    fn unlink(&self) -> Result<(), ToolFailure>;
}

/// The launcher of one build identity, such as `spaceterm-development`.
pub(crate) struct CommandLineTool {
    name: &'static str,
    files: Arc<dyn LauncherFiles>,
}

impl CommandLineTool {
    pub(crate) fn new(name: &'static str, files: Arc<dyn LauncherFiles>) -> Self {
        Self { name, files }
    }

    /// The command people type, which is the launcher's file name.
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Reads what the launcher path holds. Blocks.
    pub(crate) fn status(&self) -> Result<Launcher, ToolFailure> {
        self.files.read()
    }

    /// Links the launcher unless another program's file or link is there, and reports the
    /// launcher afterward. Install and Repair both run this; an installed launcher is linked
    /// again. Blocks.
    pub(crate) fn install(&self) -> Result<Launcher, ToolFailure> {
        match self.files.read()? {
            Launcher::OtherLink | Launcher::OtherFile => return Err(ToolFailure::Occupied),
            Launcher::Blocked => return Err(ToolFailure::Blocked),
            Launcher::Missing | Launcher::Stale | Launcher::Outdated | Launcher::Installed => {
                match self.files.link()? {
                    LinkOutcome::Linked => {}
                    LinkOutcome::Kept => return Err(ToolFailure::Occupied),
                    LinkOutcome::Blocked => return Err(ToolFailure::Blocked),
                }
            }
        }
        self.status()
    }

    /// Removes SpaceTerm's launcher and reports the launcher afterward, which another program's
    /// entry may still hold. Blocks.
    pub(crate) fn uninstall(&self) -> Result<Launcher, ToolFailure> {
        self.files.unlink()?;
        self.status()
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Launcher files whose contents the test sets and whose links and removals it counts.
    pub(crate) struct ScriptedFiles {
        pub(crate) launcher: Mutex<Result<Launcher, ToolFailure>>,
        /// What linking returns. A successful link leaves the launcher installed; a kept one
        /// found another program's link there.
        pub(crate) link: Mutex<Result<LinkOutcome, ToolFailure>>,
        /// What removing returns. A successful removal leaves nothing where SpaceTerm's link
        /// was, as the service does, and leaves another program's entry in place.
        pub(crate) unlink: Mutex<Result<(), ToolFailure>>,
        pub(crate) links: AtomicUsize,
        pub(crate) unlinks: AtomicUsize,
        pub(crate) reads: AtomicUsize,
    }

    impl ScriptedFiles {
        pub(crate) fn new(launcher: Result<Launcher, ToolFailure>) -> Arc<Self> {
            Arc::new(Self {
                launcher: Mutex::new(launcher),
                link: Mutex::new(Ok(LinkOutcome::Linked)),
                unlink: Mutex::new(Ok(())),
                links: AtomicUsize::new(0),
                unlinks: AtomicUsize::new(0),
                reads: AtomicUsize::new(0),
            })
        }

        pub(crate) fn links(&self) -> usize {
            self.links.load(Ordering::SeqCst)
        }

        pub(crate) fn unlinks(&self) -> usize {
            self.unlinks.load(Ordering::SeqCst)
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
        fn unlink(&self) -> Result<(), ToolFailure> {
            self.unlinks.fetch_add(1, Ordering::SeqCst);
            let outcome = *self.unlink.lock().unwrap();
            if outcome.is_ok() {
                let mut launcher = self.launcher.lock().unwrap();
                if !matches!(*launcher, Ok(Launcher::OtherLink | Launcher::OtherFile)) {
                    *launcher = Ok(Launcher::Missing);
                }
            }
            outcome
        }
    }

    pub(crate) fn tool(files: &Arc<ScriptedFiles>) -> CommandLineTool {
        CommandLineTool::new(
            "spaceterm-development",
            Arc::clone(files) as Arc<dyn LauncherFiles>,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn fixture(launcher: Launcher) -> (Arc<ScriptedFiles>, CommandLineTool) {
        let files = ScriptedFiles::new(Ok(launcher));
        let tool = tool(&files);
        (files, tool)
    }

    #[test]
    fn installing_links_a_missing_stale_or_outdated_launcher_and_reports_it_installed() {
        for launcher in [Launcher::Missing, Launcher::Stale, Launcher::Outdated] {
            let (files, tool) = fixture(launcher);
            assert_eq!(
                tool.status().map(Launcher::actions),
                Ok(&[ToolAction::Install][..])
            );
            assert_eq!(tool.install(), Ok(Launcher::Installed));
            assert_eq!(files.links(), 1);
        }
    }

    #[test]
    fn repairing_an_installed_launcher_links_it_again() {
        let (files, tool) = fixture(Launcher::Installed);
        assert_eq!(
            tool.status().map(Launcher::actions),
            Ok(&[ToolAction::Repair, ToolAction::Uninstall][..])
        );
        assert_eq!(tool.install(), Ok(Launcher::Installed));
        assert_eq!(files.links(), 1);
    }

    #[test]
    fn uninstalling_removes_the_launcher_and_reports_it_missing() {
        let (files, tool) = fixture(Launcher::Installed);
        assert_eq!(tool.uninstall(), Ok(Launcher::Missing));
        assert_eq!(files.unlinks(), 1);
        assert_eq!(
            tool.status().map(Launcher::actions),
            Ok(&[ToolAction::Install][..])
        );
    }

    #[test]
    fn another_programs_file_or_link_is_never_replaced_and_offers_nothing() {
        for launcher in [Launcher::OtherLink, Launcher::OtherFile] {
            let (files, tool) = fixture(launcher);
            assert_eq!(tool.install(), Err(ToolFailure::Occupied));
            assert_eq!(files.links(), 0);
            assert_eq!(tool.status().map(Launcher::actions), Ok(&[][..]));
        }
        // A file that appears between the read and the link is kept by the link itself.
        let (files, tool) = fixture(Launcher::Missing);
        *files.link.lock().unwrap() = Ok(LinkOutcome::Kept);
        assert_eq!(tool.install(), Err(ToolFailure::Occupied));
    }

    #[test]
    fn failures_pass_through_as_classifications() {
        let files = ScriptedFiles::new(Err(ToolFailure::UnsafeStorage));
        let tool = tool(&files);
        assert_eq!(tool.status(), Err(ToolFailure::UnsafeStorage));
        assert_eq!(tool.install(), Err(ToolFailure::UnsafeStorage));
        *files.launcher.lock().unwrap() = Ok(Launcher::Missing);
        *files.link.lock().unwrap() = Err(ToolFailure::Busy);
        assert_eq!(tool.install(), Err(ToolFailure::Busy));
        *files.launcher.lock().unwrap() = Ok(Launcher::Installed);
        *files.unlink.lock().unwrap() = Err(ToolFailure::Busy);
        assert_eq!(tool.uninstall(), Err(ToolFailure::Busy));
        assert_eq!(tool.status(), Ok(Launcher::Installed));
    }

    #[test]
    fn a_blocked_launcher_is_never_linked_and_offers_nothing() {
        let (files, tool) = fixture(Launcher::Outdated);
        *files.link.lock().unwrap() = Ok(LinkOutcome::Blocked);
        assert_eq!(tool.install(), Err(ToolFailure::Blocked));
        assert_eq!(tool.status(), Ok(Launcher::Blocked));
        assert_eq!(tool.install(), Err(ToolFailure::Blocked));
        assert_eq!(files.links(), 1);
        assert_eq!(tool.status().map(Launcher::actions), Ok(&[][..]));
    }
}
