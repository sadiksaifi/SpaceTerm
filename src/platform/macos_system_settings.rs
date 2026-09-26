use std::marker::PhantomData;
use std::rc::Rc;

use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSString, NSURL};

use super::permission_recovery::{UrlLaunchError, UrlLauncher};
#[derive(Default)]
pub(crate) struct NsWorkspaceUrlLauncher {
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl UrlLauncher for NsWorkspaceUrlLauncher {
    fn open_url(&self, uri: &'static str) -> Result<(), UrlLaunchError> {
        if MainThreadMarker::new().is_none() {
            return Err(UrlLaunchError::OffMainThread);
        }
        let url =
            NSURL::URLWithString(&NSString::from_str(uri)).ok_or(UrlLaunchError::Unavailable)?;
        NSWorkspace::sharedWorkspace()
            .openURL(&url)
            .then_some(())
            .ok_or(UrlLaunchError::Rejected)
    }
}
