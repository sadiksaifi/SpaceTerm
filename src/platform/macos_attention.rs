use std::sync::Mutex;

use cocoa::appkit::NSApplication;
use cocoa::base::{id, nil};
use cocoa::foundation::{NSInteger, NSString};
use objc::{class, msg_send, sel, sel_impl};

use crate::terminal::attention::{AttentionEffects, AttentionEvent};

#[cfg_attr(test, allow(dead_code))]
static DOCK_REQUEST: Mutex<Option<NSInteger>> = Mutex::new(None);

pub(crate) trait AttentionPlatform {
    fn audio_bell(&mut self);
    fn request_dock_attention(&mut self);
    fn cancel_dock_attention(&mut self);
    fn notify(&mut self, event: AttentionEvent, aggregate_count: u32);
}

pub(crate) fn apply_attention_effects(
    platform: &mut impl AttentionPlatform,
    effects: AttentionEffects,
) {
    if effects.audio_bell {
        platform.audio_bell();
    }
    if effects.request_dock_attention {
        platform.request_dock_attention();
    }
    if effects.cancel_dock_attention {
        platform.cancel_dock_attention();
    }
    if let Some(event) = effects.notification {
        platform.notify(event, effects.unread_count);
    }
}

#[derive(Default)]
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct MacosAttentionPlatform;

impl AttentionPlatform for MacosAttentionPlatform {
    fn audio_bell(&mut self) {
        unsafe extern "C" {
            #[cfg_attr(test, allow(dead_code))]
            fn NSBeep();
        }
        // SAFETY: AppKit's process-global bell takes no arguments and may be called on the UI thread.
        unsafe { NSBeep() };
    }

    fn request_dock_attention(&mut self) {
        // SAFETY: This runs from GPUI's main thread and sends documented NSApplication messages.
        unsafe {
            let application = NSApplication::sharedApplication(nil);
            let request: NSInteger = msg_send![application, requestUserAttention: 10_u64];
            *DOCK_REQUEST
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(request);
        }
    }

    fn cancel_dock_attention(&mut self) {
        let request = DOCK_REQUEST
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(request) = request {
            // SAFETY: The request identity came from NSApplication and cancellation stays on UI.
            unsafe {
                let application = NSApplication::sharedApplication(nil);
                let _: () = msg_send![application, cancelUserAttentionRequest: request];
            }
        }
    }

    fn notify(&mut self, event: AttentionEvent, aggregate_count: u32) {
        let body = match event {
            AttentionEvent::Bell => format!("Terminal requested attention ({aggregate_count})"),
            AttentionEvent::CommandFinished { exit_status, .. } => match exit_status {
                Some(0) => format!("Command finished ({aggregate_count})"),
                Some(_) => format!("Command finished with an error ({aggregate_count})"),
                None => format!("Command finished ({aggregate_count})"),
            },
        };
        // SAFETY: Deprecated NSUserNotification remains native-policy mediated on supported macOS,
        // and all objects/messages are confined to GPUI's main thread.
        unsafe {
            let notification: id = msg_send![class!(NSUserNotification), new];
            let title = NSString::alloc(nil).init_str("SpaceTerm");
            let body = NSString::alloc(nil).init_str(&body);
            let _: () = msg_send![notification, setTitle: title];
            let _: () = msg_send![notification, setInformativeText: body];
            let center: id = msg_send![
                class!(NSUserNotificationCenter),
                defaultUserNotificationCenter
            ];
            let _: () = msg_send![center, deliverNotification: notification];
            let _: () = msg_send![title, release];
            let _: () = msg_send![body, release];
            let _: () = msg_send![notification, release];
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[derive(Default)]
    struct FakePlatform {
        audio: usize,
        dock: usize,
        cancelled: usize,
        notifications: Vec<(AttentionEvent, u32)>,
    }

    impl AttentionPlatform for FakePlatform {
        fn audio_bell(&mut self) {
            self.audio += 1;
        }
        fn request_dock_attention(&mut self) {
            self.dock += 1;
        }
        fn cancel_dock_attention(&mut self) {
            self.cancelled += 1;
        }
        fn notify(&mut self, event: AttentionEvent, count: u32) {
            self.notifications.push((event, count));
        }
    }

    #[test]
    fn platform_effects_are_explicit_testable_and_content_redacted() {
        let event = AttentionEvent::CommandFinished {
            exit_status: Some(1),
            duration: Duration::from_secs(2),
        };
        let mut platform = FakePlatform::default();
        apply_attention_effects(
            &mut platform,
            AttentionEffects {
                audio_bell: true,
                request_dock_attention: true,
                cancel_dock_attention: true,
                notification: Some(event),
                unread_count: 3,
                ..AttentionEffects::default()
            },
        );

        assert_eq!(
            (platform.audio, platform.dock, platform.cancelled),
            (1, 1, 1)
        );
        assert_eq!(platform.notifications, vec![(event, 3)]);
    }
}
