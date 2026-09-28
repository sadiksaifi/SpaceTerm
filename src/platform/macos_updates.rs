use crate::updates::{UpdateAdapter, UpdateError, UpdateEvent};

#[derive(Default)]
pub(crate) struct MacosUpdates {
    #[cfg(spaceterm_sparkle)]
    session: std::cell::RefCell<Option<native::Session>>,
}

impl MacosUpdates {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

#[cfg(not(spaceterm_sparkle))]
impl UpdateAdapter for MacosUpdates {
    fn start(&self, _: async_channel::Sender<UpdateEvent>) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn check(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn download(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
    fn cancel(&self) {}
    fn install(&self) -> Result<(), UpdateError> {
        Err(UpdateError::Unavailable)
    }
}

#[cfg(spaceterm_sparkle)]
mod native {
    use super::*;
    use std::ffi::{CStr, c_char, c_void};
    use std::ptr::NonNull;

    unsafe extern "C" {
        fn spt_updater_create(
            context: *mut c_void,
            callback: unsafe extern "C" fn(*mut c_void, u32, *const c_char, u64, u64),
            validator: unsafe extern "C" fn(
                *const c_char,
                *const c_char,
                *const c_char,
                bool,
            ) -> bool,
        ) -> *mut c_void;
        fn spt_updater_check(session: *mut c_void) -> bool;
        fn spt_updater_download(session: *mut c_void) -> bool;
        fn spt_updater_cancel(session: *mut c_void);
        fn spt_updater_install(session: *mut c_void) -> bool;
        fn spt_updater_destroy(session: *mut c_void);
        fn spt_updater_finish_on_quit(session: *mut c_void) -> bool;
        fn spt_updater_read_history(session: *mut c_void, bytes: *mut u8, capacity: u64) -> u64;
        fn spt_updater_write_history(session: *mut c_void, bytes: *const u8, length: u64);
    }

    pub(super) struct Session {
        pointer: NonNull<c_void>,
        // The boxed callback context outlives the native session and is released only after detach.
        _events: Box<async_channel::Sender<UpdateEvent>>,
    }

    unsafe extern "C" fn event(
        context: *mut c_void,
        kind: u32,
        version: *const c_char,
        first: u64,
        second: u64,
    ) {
        // No panic or native string may escape the FFI boundary. The bridge bounds version data.
        let _ = std::panic::catch_unwind(|| {
            // SAFETY: create retains this pointer until destroy detaches the native callback.
            let events = unsafe { &*context.cast::<async_channel::Sender<UpdateEvent>>() };
            let event = match kind {
                1 if !version.is_null() => {
                    // SAFETY: the bridge passes a live NSString UTF-8 string for this callback.
                    let Ok(version) = (unsafe { CStr::from_ptr(version) }).to_str() else {
                        return;
                    };
                    if crate::updates::stable_version(version).is_none() {
                        return;
                    }
                    UpdateEvent::Available(version.to_owned())
                }
                2 => UpdateEvent::UpToDate,
                3 => UpdateEvent::Downloading {
                    received: first,
                    total: second,
                },
                4 => UpdateEvent::Verifying,
                5 => UpdateEvent::Ready,
                6 => UpdateEvent::Installing,
                7 => UpdateEvent::Failed(match first {
                    2 => UpdateError::Download,
                    3 => UpdateError::Verification,
                    4 => UpdateError::Installation,
                    5 => UpdateError::ReadOnly,
                    _ => UpdateError::Check,
                }),
                8 => UpdateEvent::Finished,
                9 => UpdateEvent::ReleaseMetadata {
                    published_at: first,
                    prepared: second != 0,
                },
                _ => return,
            };
            let _ = events.try_send(event);
        });
    }

    unsafe extern "C" fn validate(
        version: *const c_char,
        display: *const c_char,
        url: *const c_char,
        informational: bool,
    ) -> bool {
        std::panic::catch_unwind(|| {
            if version.is_null() || display.is_null() || url.is_null() {
                return false;
            }
            // SAFETY: all strings are bounded and borrowed from live NSStrings in this callback.
            let values = unsafe {
                [
                    CStr::from_ptr(version),
                    CStr::from_ptr(display),
                    CStr::from_ptr(url),
                ]
            };
            let [Ok(version), Ok(display), Ok(url)] = values.map(CStr::to_str) else {
                return false;
            };
            crate::updates::allows_release(version, display, url, informational)
        })
        .unwrap_or(false)
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: the adapter is !Send and is owned by the main-thread application composition.
            unsafe { spt_updater_destroy(self.pointer.as_ptr()) };
        }
    }

    impl UpdateAdapter for MacosUpdates {
        fn start(&self, sender: async_channel::Sender<UpdateEvent>) -> Result<(), UpdateError> {
            if cfg!(any(
                feature = "development-app",
                feature = "appearance-exerciser"
            )) || objc2::MainThreadMarker::new().is_none()
                || self.session.borrow().is_some()
            {
                return Err(UpdateError::Unavailable);
            }
            let mut events = Box::new(sender);
            // SAFETY: the retained box remains stable until the session is destroyed.
            let pointer = unsafe { spt_updater_create((&raw mut *events).cast(), event, validate) };
            let pointer = NonNull::new(pointer).ok_or(UpdateError::Unavailable)?;
            *self.session.borrow_mut() = Some(Session {
                pointer,
                _events: events,
            });
            Ok(())
        }
        fn check(&self) -> Result<(), UpdateError> {
            self.command(spt_updater_check, UpdateError::Check)
        }
        fn download(&self) -> Result<(), UpdateError> {
            self.command(spt_updater_download, UpdateError::Download)
        }
        fn install(&self) -> Result<(), UpdateError> {
            self.command(spt_updater_install, UpdateError::Installation)
        }
        fn finish_on_quit(&self) -> Result<(), UpdateError> {
            self.command(spt_updater_finish_on_quit, UpdateError::Installation)
        }
        fn load_history(&self) -> crate::updates::policy::UpdateHistory {
            let session = self.session.borrow();
            let Some(session) = session.as_ref() else {
                return Default::default();
            };
            let mut bytes = [0u8; 4096];
            // SAFETY: retained main-thread session; buffer capacity is passed exactly.
            let length = unsafe {
                spt_updater_read_history(
                    session.pointer.as_ptr(),
                    bytes.as_mut_ptr(),
                    bytes.len() as u64,
                )
            } as usize;
            bytes
                .get(..length)
                .and_then(|bytes| serde_json::from_slice(bytes).ok())
                .unwrap_or_default()
        }
        fn save_history(&self, history: &crate::updates::policy::UpdateHistory) {
            let session = self.session.borrow();
            if let (Some(session), Ok(bytes)) = (session.as_ref(), serde_json::to_vec(history)) {
                // SAFETY: the bridge copies bounded bytes synchronously on the main thread.
                unsafe {
                    spt_updater_write_history(
                        session.pointer.as_ptr(),
                        bytes.as_ptr(),
                        bytes.len() as u64,
                    )
                };
            }
        }
        fn cancel(&self) {
            if let Some(session) = self.session.borrow().as_ref() {
                // SAFETY: session is retained and every adapter call runs on the main thread.
                unsafe { spt_updater_cancel(session.pointer.as_ptr()) };
            }
        }
    }

    impl MacosUpdates {
        fn command(
            &self,
            operation: unsafe extern "C" fn(*mut c_void) -> bool,
            error: UpdateError,
        ) -> Result<(), UpdateError> {
            let session = self.session.borrow();
            let session = session.as_ref().ok_or(UpdateError::Unavailable)?;
            // SAFETY: the native session is retained and this adapter is main-thread owned.
            if unsafe { operation(session.pointer.as_ptr()) } {
                Ok(())
            } else {
                Err(error)
            }
        }
    }
}
