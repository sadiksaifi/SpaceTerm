use gpui::{App, Global, Task};
use objc2::{MainThreadMarker, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSString};

use super::desktop_events::DesktopEventAdapter;

pub(crate) struct MacosTextInputFocus;

impl DesktopEventAdapter for MacosTextInputFocus {
    fn install(&self, cx: &mut App) {
        let center = NSNotificationCenter::defaultCenter();
        let (wake, receiver) = async_channel::bounded(1);
        let mut observers = Vec::new();
        for name in [
            "NSApplicationDidBecomeActiveNotification",
            "NSApplicationDidResignActiveNotification",
            "NSWindowDidBecomeMainNotification",
            "NSWindowDidResignMainNotification",
            "NSWindowDidMiniaturizeNotification",
            "NSWindowDidDeminiaturizeNotification",
        ] {
            let wake = wake.clone();
            let callback = block2::RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| {
                // Coalesce native notifications and avoid re-entering GPUI from AppKit.
                let _ = wake.try_send(());
            });
            // SAFETY: AppKit posts these application and window notifications on the main
            // thread. The retained observer tokens are removed before the delivery task drops.
            let observer = unsafe {
                center.addObserverForName_object_queue_usingBlock(
                    Some(&NSString::from_str(name)),
                    None,
                    None,
                    &callback,
                )
            };
            observers.push(observer);
        }
        let task = cx.spawn(async move |cx| {
            while receiver.recv().await.is_ok() {
                cx.update(refresh);
            }
        });
        cx.set_global(NativeFocusObserver {
            center,
            observers,
            _task: task,
        });
        refresh(cx);
    }
}

fn refresh(cx: &mut App) {
    let owner = MainThreadMarker::new().and_then(|mtm| {
        let application = NSApplication::sharedApplication(mtm);
        let main = application.mainWindow()?;
        (application.isActive() && main.isVisible() && !main.isMiniaturized())
            .then(|| cx.active_window())
            .flatten()
    });
    spaceterm_ui::TextInputFocus::set(owner, cx);
}

struct NativeFocusObserver {
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    _task: Task<()>,
}

impl Global for NativeFocusObserver {}

impl Drop for NativeFocusObserver {
    fn drop(&mut self) {
        for observer in self.observers.drain(..) {
            // SAFETY: Each token was returned by this notification center during registration.
            unsafe { self.center.removeObserver(observer.as_ref()) };
        }
    }
}
