//! The Settings Document file as other programs see it.
//!
//! Reading and writing the file stay with the settings storage.

use std::{
    any::Any,
    path::{Path, PathBuf},
};

use gpui::{App, SharedString};

/// Watching could not start, so outside changes go unnoticed until the next launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("the settings file cannot be watched")]
pub(crate) struct SettingsFileWatchError;

/// Reports possible changes until it is dropped.
pub(crate) struct SettingsFileWatch(#[allow(dead_code, reason = "held for its drop")] Box<dyn Any>);

impl SettingsFileWatch {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self(Box::new(()))
    }
}

/// Operating-System effects on the Settings Document file.
pub(crate) trait SettingsFileAccess {
    /// Where the file lives, spelled for a person to read. It is never logged.
    fn location(&self) -> SharedString;

    /// Opens the file in the program the Operating System assigns to it.
    fn open(&self, cx: &mut App);

    /// Calls `changed` from any thread whenever the file may have changed.
    ///
    /// Watching needs the directory that holds the file, so it fails until that directory exists.
    fn watch(
        &self,
        changed: Box<dyn Fn() + Send + Sync>,
    ) -> Result<SettingsFileWatch, SettingsFileWatchError>;
}

/// The settings file at its semantic location, opened and watched through the Operating System.
pub(crate) struct SystemSettingsFile {
    path: PathBuf,
    location: SharedString,
}

impl SystemSettingsFile {
    /// `home` shortens the shown location; the file itself is addressed by `path`.
    pub(crate) fn new(path: PathBuf, home: &Path) -> Self {
        let location = match path.strip_prefix(home) {
            Ok(relative) => format!("~/{}", relative.display()),
            Err(_) => path.display().to_string(),
        };
        Self {
            path,
            location: location.into(),
        }
    }
}

impl SettingsFileAccess for SystemSettingsFile {
    fn location(&self) -> SharedString {
        self.location.clone()
    }

    fn open(&self, cx: &mut App) {
        cx.open_with_system(&self.path);
    }

    fn watch(
        &self,
        changed: Box<dyn Fn() + Send + Sync>,
    ) -> Result<SettingsFileWatch, SettingsFileWatchError> {
        use notify::Watcher as _;

        let directory = self.path.parent().ok_or(SettingsFileWatchError)?;
        let name = self
            .path
            .file_name()
            .ok_or(SettingsFileWatchError)?
            .to_owned();
        // Editors often save by replacing the file, which ends a watch on the file itself, so the
        // watch covers its directory and filters by name.
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
                Ok(event) => {
                    if event
                        .paths
                        .iter()
                        .any(|path| path.file_name() == Some(name.as_os_str()))
                    {
                        changed();
                    }
                }
                // A lost or overflowed event may hide a change, so it counts as one.
                Err(_) => changed(),
            })
            .map_err(|_| SettingsFileWatchError)?;
        watcher
            .watch(directory, notify::RecursiveMode::NonRecursive)
            .map_err(|_| SettingsFileWatchError)?;
        Ok(SettingsFileWatch(Box::new(watcher)))
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::Arc,
    };

    use super::*;

    /// A settings file whose changes the test announces and whose opening it records.
    #[derive(Default)]
    pub(crate) struct RecordingSettingsFile {
        pub(crate) opened: Cell<usize>,
        pub(crate) watchable: Cell<bool>,
        changed: RefCell<Option<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl RecordingSettingsFile {
        pub(crate) fn watchable() -> Rc<Self> {
            let file = Rc::new(Self::default());
            file.watchable.set(true);
            file
        }

        pub(crate) fn announce_change(&self) {
            let changed = self.changed.borrow().clone();
            changed.expect("the file is watched")();
        }

        pub(crate) fn is_watched(&self) -> bool {
            self.changed.borrow().is_some()
        }
    }

    impl SettingsFileAccess for RecordingSettingsFile {
        fn location(&self) -> SharedString {
            "~/.config/spaceterm/settings.json".into()
        }

        fn open(&self, _: &mut App) {
            self.opened.set(self.opened.get() + 1);
        }

        fn watch(
            &self,
            changed: Box<dyn Fn() + Send + Sync>,
        ) -> Result<SettingsFileWatch, SettingsFileWatchError> {
            if !self.watchable.get() {
                return Err(SettingsFileWatchError);
            }
            *self.changed.borrow_mut() = Some(Arc::from(changed));
            Ok(SettingsFileWatch::for_test())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_location_shortens_the_home_directory() {
        let file = SystemSettingsFile::new(
            PathBuf::from("/Users/person/.config/spaceterm/settings.json"),
            Path::new("/Users/person"),
        );
        assert_eq!(
            file.location().as_ref(),
            "~/.config/spaceterm/settings.json"
        );

        let elsewhere = SystemSettingsFile::new(
            PathBuf::from("/etc/spaceterm/settings.json"),
            Path::new("/Users/person"),
        );
        assert_eq!(
            elsewhere.location().as_ref(),
            "/etc/spaceterm/settings.json"
        );
    }

    #[test]
    fn watching_reports_a_change_to_the_file_and_ignores_its_neighbors() {
        let directory = std::env::temp_dir().join(format!(
            "spaceterm-settings-file-watch-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("fixture directory");
        let path = directory.join("settings.json");
        std::fs::write(&path, b"{}").expect("fixture file");
        let file = SystemSettingsFile::new(path.clone(), Path::new("/nonexistent-home"));
        let (sender, receiver) = std::sync::mpsc::channel();
        let _watch = file
            .watch(Box::new(move || {
                let _ = sender.send(());
            }))
            .expect("the directory exists");
        // FSEvents may report writes that happened just before the stream started.
        std::thread::sleep(std::time::Duration::from_millis(200));
        while receiver.try_recv().is_ok() {}

        std::fs::write(directory.join("settings.json.bak"), b"{}").expect("neighbor");
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_err()
        );

        std::fs::write(&path, b"{\"changed\":true}").expect("change");
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok()
        );
        let settle = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match receiver.recv_timeout(std::time::Duration::from_millis(200)) {
                    Ok(()) => assert!(std::time::Instant::now() < deadline),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(error) => panic!("watch disconnected: {error}"),
                }
            }
        };
        settle();
        let replacement = directory.join("settings.json.tmp");
        std::fs::write(&replacement, b"{\"replaced\":true}").expect("replacement");
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(500))
                .is_err()
        );
        std::fs::rename(&replacement, &path).expect("atomic replacement");
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok(),
            "atomic replacement must be observed"
        );
        settle();
        std::fs::write(&path, b"{\"after_replacement\":true}").expect("subsequent change");
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok(),
            "the watch must survive replacement"
        );
        drop(_watch);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn watching_needs_the_directory() {
        let file = SystemSettingsFile::new(
            std::env::temp_dir().join("spaceterm-missing-settings-directory/settings.json"),
            Path::new("/nonexistent-home"),
        );
        assert!(file.watch(Box::new(|| {})).is_err());
    }
}
