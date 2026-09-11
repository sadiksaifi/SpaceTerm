use std::ffi::c_void;

use cocoa::base::{id, nil};
use cocoa::foundation::{NSInteger, NSString};
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

use super::appearance::{
    AppearancePlatform, SystemAppearanceObservation, SystemAppearanceSubscription,
};
use crate::appearance::Appearance;

const APPEARANCE_NOTIFICATION: &str = "AppleInterfaceThemeChangedNotification";
const APPEARANCE_OBSERVER_CLASS: &str = "SpaceTermDistributedAppearanceObserver";
const APPEARANCE_SENDER_IVAR: &str = "spaceTermAppearanceSender";
const SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY: NSInteger = 4;

pub(crate) struct MacosAppearancePlatform;

impl AppearancePlatform for MacosAppearancePlatform {
    fn system_appearance(&self) -> Option<Appearance> {
        // SAFETY: the preference is read on the AppKit thread. It is the system preference,
        // independent of NSApplication's effective appearance, which this Adapter may force.
        unsafe {
            let defaults: id = msg_send![class!(NSUserDefaults), standardUserDefaults];
            if defaults == nil {
                return None;
            }
            let key = NSString::alloc(nil).init_str("AppleInterfaceStyle");
            let value: id = msg_send![defaults, stringForKey: key];
            let _: () = msg_send![key, release];
            if value == nil {
                return Some(Appearance::Light);
            }
            let dark = NSString::alloc(nil).init_str("Dark");
            let is_dark: objc::runtime::BOOL = msg_send![value, isEqualToString: dark];
            let _: () = msg_send![dark, release];
            Some(if is_dark != objc::runtime::NO {
                Appearance::Dark
            } else {
                Appearance::Light
            })
        }
    }

    fn observe(&self) -> Option<SystemAppearanceObservation> {
        // SAFETY: registration and teardown run on the AppKit thread. The selector observer is
        // retained by the subscription because NSDistributedNotificationCenter does not retain
        // selector observers.
        unsafe {
            let center: id = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
            observe_distributed(center, APPEARANCE_NOTIFICATION)
        }
    }

    fn apply_native_appearance(&self, appearance: Appearance) {
        // SAFETY: NSApplication owns the selected appearance after this synchronous setter.
        unsafe {
            let application: id = msg_send![class!(NSApplication), sharedApplication];
            let name = NSString::alloc(nil).init_str(match appearance {
                Appearance::Light => "NSAppearanceNameAqua",
                Appearance::Dark => "NSAppearanceNameDarkAqua",
            });
            let selected: id = msg_send![class!(NSAppearance), appearanceNamed: name];
            let _: () = msg_send![name, release];
            if selected != nil {
                let _: () = msg_send![application, setAppearance: selected];
            }
        }
    }
}

unsafe fn observe_distributed(
    center: id,
    notification_name: &str,
) -> Option<SystemAppearanceObservation> {
    if center == nil {
        return None;
    }

    let (sender, changed) = async_channel::bounded(1);
    let observer = unsafe { new_appearance_observer(sender)? };
    let name = unsafe { NSString::alloc(nil).init_str(notification_name) };
    if name == nil {
        unsafe {
            let _: () = msg_send![observer, release];
        }
        return None;
    }
    let center: id = unsafe { msg_send![center, retain] };

    // NSApplication suspends ordinary distributed notification delivery while inactive. System
    // appearance changes happen while another application is active, so this observer must opt
    // into immediate delivery instead of inheriting the default coalescing suspension behavior.
    unsafe {
        let _: () = msg_send![center,
            addObserver: observer
            selector: sel!(appearanceChanged:)
            name: name
            object: nil
            suspensionBehavior: SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY
        ];
    }

    Some(SystemAppearanceObservation {
        changed,
        subscription: Box::new(MacosAppearanceSubscription {
            center,
            observer,
            name,
        }),
    })
}

unsafe fn new_appearance_observer(sender: async_channel::Sender<()>) -> Option<id> {
    let class = appearance_observer_class()?;
    let observer: id = unsafe { msg_send![class, new] };
    if observer == nil {
        return None;
    }
    unsafe {
        (*observer).set_ivar(
            APPEARANCE_SENDER_IVAR,
            Box::into_raw(Box::new(sender)).cast::<c_void>(),
        );
    }
    Some(observer)
}

fn appearance_observer_class() -> Option<&'static Class> {
    if let Some(class) = Class::get(APPEARANCE_OBSERVER_CLASS) {
        return Some(class);
    }
    let mut declaration = ClassDecl::new(APPEARANCE_OBSERVER_CLASS, class!(NSObject))?;
    declaration.add_ivar::<*mut c_void>(APPEARANCE_SENDER_IVAR);
    // SAFETY: both functions match the Objective-C method encodings for their selectors.
    unsafe {
        declaration.add_method(
            sel!(appearanceChanged:),
            appearance_changed as extern "C" fn(&Object, Sel, id),
        );
        declaration.add_method(
            sel!(dealloc),
            dealloc_appearance_observer as extern "C" fn(&Object, Sel),
        );
    }
    Some(declaration.register())
}

extern "C" fn appearance_changed(this: &Object, _: Sel, _: id) {
    // SAFETY: new_appearance_observer installs exactly one boxed sender before registering this
    // selector. The bounded channel coalesces bursts without inspecting notification contents.
    unsafe {
        let sender: *mut c_void = *this.get_ivar(APPEARANCE_SENDER_IVAR);
        if !sender.is_null() {
            let _ = (&*sender.cast::<async_channel::Sender<()>>()).try_send(());
        }
    }
}

extern "C" fn dealloc_appearance_observer(this: &Object, _: Sel) {
    // SAFETY: the observer owns exactly one boxed sender, installed before it is registered.
    unsafe {
        let sender: *mut c_void = *this.get_ivar(APPEARANCE_SENDER_IVAR);
        if !sender.is_null() {
            drop(Box::from_raw(sender.cast::<async_channel::Sender<()>>()));
        }
        let _: () = msg_send![super(this, class!(NSObject)), dealloc];
    }
}

struct MacosAppearanceSubscription {
    center: id,
    observer: id,
    name: id,
}

impl SystemAppearanceSubscription for MacosAppearanceSubscription {}

impl Drop for MacosAppearanceSubscription {
    fn drop(&mut self) {
        // SAFETY: NSDistributedNotificationCenter does not retain selector observers. Remove the
        // observer while every registration argument is still alive, then release owned objects.
        unsafe {
            let _: () = msg_send![self.center,
                removeObserver: self.observer name: self.name object: nil
            ];
            let _: () = msg_send![self.observer, release];
            let _: () = msg_send![self.name, release];
            let _: () = msg_send![self.center, release];
        }
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;

    #[gpui::test]
    fn forcing_native_chrome_does_not_change_the_system_preference(cx: &mut gpui::TestAppContext) {
        cx.update(|_| {
            let platform = MacosAppearancePlatform;
            let system = platform.system_appearance();
            // SAFETY: the test retains and restores this process's NSApplication appearance;
            // it never writes the user's system preference.
            unsafe {
                let application: id = msg_send![class!(NSApplication), sharedApplication];
                let previous: id = msg_send![application, appearance];
                if previous != nil {
                    let _: id = msg_send![previous, retain];
                }
                for appearance in [Appearance::Light, Appearance::Dark] {
                    platform.apply_native_appearance(appearance);
                    let effective: id = msg_send![application, appearance];
                    assert_ne!(effective, nil);
                    let expected = NSString::alloc(nil).init_str(match appearance {
                        Appearance::Light => "NSAppearanceNameAqua",
                        Appearance::Dark => "NSAppearanceNameDarkAqua",
                    });
                    let name: id = msg_send![effective, name];
                    let matches: objc::runtime::BOOL = msg_send![name, isEqualToString: expected];
                    let _: () = msg_send![expected, release];
                    assert_ne!(matches, objc::runtime::NO);
                    assert_eq!(platform.system_appearance(), system);
                }
                let observation = platform
                    .observe()
                    .expect("native appearance observation should be available");
                drop(observation);
                let _: () = msg_send![application, setAppearance: previous];
                if previous != nil {
                    let _: () = msg_send![previous, release];
                }
            }
        });
    }

    #[gpui::test]
    fn native_observer_coalesces_wakeups_and_closes_with_its_owner(cx: &mut gpui::TestAppContext) {
        cx.update(|_| {
            let (sender, changed) = async_channel::bounded(1);
            // SAFETY: the test owns the observer and directly exercises its content-free callback
            // before releasing it on the AppKit thread.
            unsafe {
                let observer = new_appearance_observer(sender)
                    .expect("native appearance observer should be available");
                for _ in 0..32 {
                    appearance_changed(&*observer, sel!(appearanceChanged:), nil);
                }
                assert_eq!(changed.len(), 1);
                changed
                    .try_recv()
                    .expect("coalesced appearance wakeup should be available");
                appearance_changed(&*observer, sel!(appearanceChanged:), nil);
                changed
                    .try_recv()
                    .expect("observer should remain reusable after a wakeup");

                let _: () = msg_send![observer, release];
                assert!(changed.is_closed());
            }
        });
    }
}
