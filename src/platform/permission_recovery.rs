#![cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only desktops with a permission recovery route compose this policy"
    )
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PermissionRecoveryError {
    OffMainThread,
    PlatformUnavailable,
    PlatformRejected,
}

pub(crate) trait PermissionRecoveryOpener {
    fn open(&self) -> Result<(), PermissionRecoveryError>;
}

pub(crate) struct PermissionRecovery {
    launcher: Box<dyn UrlLauncher>,
    uri: &'static str,
}

impl PermissionRecoveryOpener for PermissionRecovery {
    fn open(&self) -> Result<(), PermissionRecoveryError> {
        self.launcher
            .open_url(self.uri)
            .map_err(PermissionRecoveryError::from)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UrlLaunchError {
    OffMainThread,
    Unavailable,
    Rejected,
}

pub(crate) trait UrlLauncher {
    fn open_url(&self, uri: &'static str) -> Result<(), UrlLaunchError>;
}

impl From<UrlLaunchError> for PermissionRecoveryError {
    fn from(error: UrlLaunchError) -> Self {
        match error {
            UrlLaunchError::OffMainThread => Self::OffMainThread,
            UrlLaunchError::Unavailable => Self::PlatformUnavailable,
            UrlLaunchError::Rejected => Self::PlatformRejected,
        }
    }
}

impl PermissionRecovery {
    pub(crate) fn new(launcher: Box<dyn UrlLauncher>, uri: &'static str) -> Self {
        Self { launcher, uri }
    }
}
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    const SETTINGS_URI: &str = "x-spaceterm-test:settings";

    struct RecordingUrlLauncher {
        result: Result<(), UrlLaunchError>,
        opened_uris: Rc<RefCell<Vec<&'static str>>>,
    }

    impl UrlLauncher for RecordingUrlLauncher {
        fn open_url(&self, uri: &'static str) -> Result<(), UrlLaunchError> {
            self.opened_uris.borrow_mut().push(uri);
            self.result
        }
    }

    #[test]
    fn open_launches_one_url_and_maps_its_failure() {
        for (result, expected) in [
            (Ok(()), Ok(())),
            (
                Err(UrlLaunchError::Rejected),
                Err(PermissionRecoveryError::PlatformRejected),
            ),
            (
                Err(UrlLaunchError::OffMainThread),
                Err(PermissionRecoveryError::OffMainThread),
            ),
            (
                Err(UrlLaunchError::Unavailable),
                Err(PermissionRecoveryError::PlatformUnavailable),
            ),
        ] {
            let opened_uris = Rc::new(RefCell::new(Vec::new()));
            let recovery = PermissionRecovery::new(
                Box::new(RecordingUrlLauncher {
                    result,
                    opened_uris: Rc::clone(&opened_uris),
                }),
                SETTINGS_URI,
            );

            assert_eq!(recovery.open(), expected);
            assert_eq!(*opened_uris.borrow(), [SETTINGS_URI]);
        }
    }

    #[test]
    fn error_identifiers_carry_no_content() {
        assert_eq!(
            [
                PermissionRecoveryError::OffMainThread,
                PermissionRecoveryError::PlatformUnavailable,
                PermissionRecoveryError::PlatformRejected,
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
