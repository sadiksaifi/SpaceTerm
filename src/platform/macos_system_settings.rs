use std::marker::PhantomData;
use std::rc::Rc;

use cocoa::base::{BOOL, YES, id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use objc::{class, msg_send, sel, sel_impl};

const PREFERRED_FILES_AND_FOLDERS_SETTINGS_URI: &str = "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders";
const FALLBACK_FILES_AND_FOLDERS_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SystemSettingsOpenError {
    OffMainThread,
    PlatformUnavailable,
    PlatformRejected,
}

pub(crate) trait SystemSettingsOpener {
    fn open_files_and_folders(&self) -> Result<(), SystemSettingsOpenError>;
}

#[derive(Default)]
pub(crate) struct MacosSystemSettingsOpener {
    launcher: NsWorkspaceUrlLauncher,
}

impl SystemSettingsOpener for MacosSystemSettingsOpener {
    fn open_files_and_folders(&self) -> Result<(), SystemSettingsOpenError> {
        open_files_and_folders(&self.launcher)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UrlLaunchError {
    OffMainThread,
    Unavailable,
    Rejected,
}

trait UrlLauncher {
    fn open_url(&self, uri: &'static str) -> Result<(), UrlLaunchError>;
}

fn open_files_and_folders(launcher: &impl UrlLauncher) -> Result<(), SystemSettingsOpenError> {
    match launcher.open_url(PREFERRED_FILES_AND_FOLDERS_SETTINGS_URI) {
        Ok(()) => Ok(()),
        Err(UrlLaunchError::Rejected) => launcher
            .open_url(FALLBACK_FILES_AND_FOLDERS_SETTINGS_URI)
            .map_err(SystemSettingsOpenError::from),
        Err(error @ (UrlLaunchError::OffMainThread | UrlLaunchError::Unavailable)) => {
            Err(SystemSettingsOpenError::from(error))
        }
    }
}

impl From<UrlLaunchError> for SystemSettingsOpenError {
    fn from(error: UrlLaunchError) -> Self {
        match error {
            UrlLaunchError::OffMainThread => Self::OffMainThread,
            UrlLaunchError::Unavailable => Self::PlatformUnavailable,
            UrlLaunchError::Rejected => Self::PlatformRejected,
        }
    }
}

#[derive(Default)]
struct NsWorkspaceUrlLauncher {
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl UrlLauncher for NsWorkspaceUrlLauncher {
    fn open_url(&self, uri: &'static str) -> Result<(), UrlLaunchError> {
        if !main_thread() {
            return Err(UrlLaunchError::OffMainThread);
        }

        // SAFETY: The main-thread check confines these AppKit objects to AppKit's thread. The
        // NSString and NSURL are used synchronously before the autorelease pool drains, and
        // NSWorkspace owns the shared workspace returned by `sharedWorkspace`.
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let result = (|| {
                let string = NSString::alloc(nil).init_str(uri).autorelease();
                let url: id = msg_send![class!(NSURL), URLWithString: string];
                if url == nil {
                    return Err(UrlLaunchError::Unavailable);
                }

                let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
                if workspace == nil {
                    return Err(UrlLaunchError::Unavailable);
                }

                let opened: BOOL = msg_send![workspace, openURL: url];
                if opened == YES {
                    Ok(())
                } else {
                    Err(UrlLaunchError::Rejected)
                }
            })();
            pool.drain();
            result
        }
    }
}

fn main_thread() -> bool {
    // SAFETY: `NSThread.isMainThread` is a process query with no object lifetime transfer.
    unsafe {
        let is_main: BOOL = msg_send![class!(NSThread), isMainThread];
        is_main == YES
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;

    struct RecordingUrlLauncher {
        results: RefCell<VecDeque<Result<(), UrlLaunchError>>>,
        opened_uris: RefCell<Vec<&'static str>>,
    }

    impl RecordingUrlLauncher {
        fn new(results: impl IntoIterator<Item = Result<(), UrlLaunchError>>) -> Self {
            Self {
                results: RefCell::new(results.into_iter().collect()),
                opened_uris: RefCell::new(Vec::new()),
            }
        }
    }

    impl UrlLauncher for RecordingUrlLauncher {
        fn open_url(&self, uri: &'static str) -> Result<(), UrlLaunchError> {
            self.opened_uris.borrow_mut().push(uri);
            self.results
                .borrow_mut()
                .pop_front()
                .unwrap_or(Err(UrlLaunchError::Rejected))
        }
    }

    #[test]
    fn preferred_success_does_not_launch_fallback() {
        let launcher = RecordingUrlLauncher::new([Ok(())]);

        let result = open_files_and_folders(&launcher);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (Ok(()), vec![PREFERRED_FILES_AND_FOLDERS_SETTINGS_URI])
        );
    }

    #[test]
    fn preferred_rejection_launches_successful_fallback() {
        let launcher = RecordingUrlLauncher::new([Err(UrlLaunchError::Rejected), Ok(())]);

        let result = open_files_and_folders(&launcher);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (
                Ok(()),
                vec![
                    PREFERRED_FILES_AND_FOLDERS_SETTINGS_URI,
                    FALLBACK_FILES_AND_FOLDERS_SETTINGS_URI,
                ]
            )
        );
    }

    #[test]
    fn both_rejected_return_content_free_platform_error() {
        let launcher = RecordingUrlLauncher::new([
            Err(UrlLaunchError::Rejected),
            Err(UrlLaunchError::Rejected),
        ]);

        let result = open_files_and_folders(&launcher);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (
                Err(SystemSettingsOpenError::PlatformRejected),
                vec![
                    PREFERRED_FILES_AND_FOLDERS_SETTINGS_URI,
                    FALLBACK_FILES_AND_FOLDERS_SETTINGS_URI,
                ]
            )
        );
    }

    #[test]
    fn non_appkit_failure_does_not_launch_fallback() {
        let launcher = RecordingUrlLauncher::new([Err(UrlLaunchError::OffMainThread)]);

        let result = open_files_and_folders(&launcher);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (
                Err(SystemSettingsOpenError::OffMainThread),
                vec![PREFERRED_FILES_AND_FOLDERS_SETTINGS_URI]
            )
        );
    }

    #[test]
    fn platform_unavailability_does_not_launch_fallback() {
        let launcher = RecordingUrlLauncher::new([Err(UrlLaunchError::Unavailable)]);

        let result = open_files_and_folders(&launcher);

        assert_eq!(
            (result, launcher.opened_uris.into_inner()),
            (
                Err(SystemSettingsOpenError::PlatformUnavailable),
                vec![PREFERRED_FILES_AND_FOLDERS_SETTINGS_URI]
            )
        );
    }

    #[test]
    fn files_and_folders_uri_constants_are_exact() {
        assert_eq!(
            (
                PREFERRED_FILES_AND_FOLDERS_SETTINGS_URI,
                FALLBACK_FILES_AND_FOLDERS_SETTINGS_URI,
            ),
            (
                "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_FilesAndFolders",
                "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders",
            )
        );
    }

    #[test]
    fn error_identifiers_carry_no_content() {
        assert_eq!(
            [
                SystemSettingsOpenError::OffMainThread,
                SystemSettingsOpenError::PlatformUnavailable,
                SystemSettingsOpenError::PlatformRejected,
            ]
            .map(|error| format!("{error:?}")),
            [
                "OffMainThread".to_owned(),
                "PlatformUnavailable".to_owned(),
                "PlatformRejected".to_owned(),
            ]
        );
    }
}
