use std::marker::PhantomData;
use std::rc::Rc;

use cocoa::base::{BOOL, YES, id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use objc::{class, msg_send, sel, sel_impl};

use super::permission_recovery::{UrlLaunchError, UrlLauncher};
#[derive(Default)]
pub(crate) struct NsWorkspaceUrlLauncher {
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
