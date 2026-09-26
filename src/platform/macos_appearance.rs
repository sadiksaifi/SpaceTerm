#![allow(deprecated)]
use std::{ffi::c_void, sync::OnceLock};

use cocoa::base::{id, nil};
use cocoa::foundation::{NSInteger, NSString};
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

use super::appearance::{
    AccessibilityDisplayOptions, AppearancePlatform, SystemAppearanceObservation,
    SystemAppearanceSubscription, WindowBackdrop,
};
use crate::appearance::Appearance;

const APPEARANCE_NOTIFICATION: &str = "AppleInterfaceThemeChangedNotification";
const APPEARANCE_OBSERVER_CLASS: &str = "SpaceTermDistributedAppearanceObserver";
const APPEARANCE_SENDER_IVAR: &str = "spaceTermAppearanceSender";
const SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY: NSInteger = 4;
const ACCESSIBILITY_FRAMEWORK: &[u8] =
    b"/System/Library/Frameworks/Accessibility.framework/Accessibility\0";
const SHOW_BORDERS_GETTER: &[u8] = b"AXShowBordersEnabled\0";
const SHOW_BORDERS_NOTIFICATION: &[u8] = b"AXShowBordersEnabledStatusDidChangeNotification\0";

type ShowBordersGetter = unsafe extern "C" fn() -> objc::runtime::BOOL;

#[derive(Clone, Copy)]
struct ShowBordersSymbols {
    _framework: usize,
    getter: ShowBordersGetter,
    notification: Option<usize>,
}

static SHOW_BORDERS_SYMBOLS: OnceLock<Option<ShowBordersSymbols>> = OnceLock::new();

fn show_borders_symbols() -> Option<ShowBordersSymbols> {
    *SHOW_BORDERS_SYMBOLS.get_or_init(|| {
        // SAFETY: the path names Apple's public Accessibility framework. The retained handle keeps
        // every resolved symbol and the exported notification object valid for the process.
        unsafe {
            let framework = libc::dlopen(
                ACCESSIBILITY_FRAMEWORK.as_ptr().cast(),
                libc::RTLD_LAZY | libc::RTLD_LOCAL,
            );
            if framework.is_null() {
                return None;
            }
            let getter = libc::dlsym(framework, SHOW_BORDERS_GETTER.as_ptr().cast());
            if getter.is_null() {
                let _ = libc::dlclose(framework);
                return None;
            }
            let notification = libc::dlsym(framework, SHOW_BORDERS_NOTIFICATION.as_ptr().cast());
            let notification = if notification.is_null() {
                None
            } else {
                let name = *notification.cast::<id>();
                (name != nil).then_some(name as usize)
            };
            Some(ShowBordersSymbols {
                _framework: framework as usize,
                getter: std::mem::transmute::<*mut c_void, ShowBordersGetter>(getter),
                notification,
            })
        }
    })
}

fn show_borders_enabled(increase_contrast: bool) -> bool {
    let native = show_borders_symbols().map(|symbols| {
        // SAFETY: symbol loading verifies the public C function is present and retains its
        // framework. The function has no arguments and returns Objective-C BOOL.
        unsafe { (symbols.getter)() != objc::runtime::NO }
    });
    resolve_show_borders(native, increase_contrast)
}

const fn resolve_show_borders(native: Option<bool>, increase_contrast: bool) -> bool {
    match native {
        Some(shown) => shown,
        None => increase_contrast,
    }
}

fn show_borders_changed_notification() -> Option<id> {
    show_borders_symbols()?
        .notification
        .map(|notification| notification as id)
}

pub(crate) struct MacosAppearancePlatform;

impl AppearancePlatform for MacosAppearancePlatform {
    fn prefers_reduced_motion(&self) -> bool {
        // SAFETY: display accessibility preferences are queried on the AppKit thread.
        unsafe {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let reduce: objc::runtime::BOOL =
                msg_send![workspace, accessibilityDisplayShouldReduceMotion];
            reduce != objc::runtime::NO
        }
    }
    fn supports_native_window_transparency(&self) -> bool {
        true
    }
    fn accessibility_display_options(&self) -> AccessibilityDisplayOptions {
        // SAFETY: display accessibility preferences are queried on the AppKit thread.
        unsafe {
            let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
            let reduce: objc::runtime::BOOL =
                msg_send![workspace, accessibilityDisplayShouldReduceTransparency];
            let contrast: objc::runtime::BOOL =
                msg_send![workspace, accessibilityDisplayShouldIncreaseContrast];
            let differentiate: objc::runtime::BOOL = msg_send![
                workspace,
                accessibilityDisplayShouldDifferentiateWithoutColor
            ];
            AccessibilityDisplayOptions {
                reduce_transparency: reduce != objc::runtime::NO,
                increase_contrast: contrast != objc::runtime::NO,
                show_borders: show_borders_enabled(contrast != objc::runtime::NO),
                differentiate_without_color: differentiate != objc::runtime::NO,
            }
        }
    }
    fn apply_window_backdrop(&self, window: &gpui::Window, backdrop: WindowBackdrop) {
        super::macos_window_backdrop::apply(window, backdrop);
    }
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

    // Accessibility changes use the workspace's local notification center. Both observations
    // share the same coalescing wakeup and are removed with the application owner.
    let workspace: id = unsafe { msg_send![class!(NSWorkspace), sharedWorkspace] };
    let accessibility_center: id = unsafe { msg_send![workspace, notificationCenter] };
    let accessibility_center: id = unsafe { msg_send![accessibility_center, retain] };
    let accessibility_name = unsafe {
        NSString::alloc(nil).init_str("NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification")
    };
    unsafe {
        let _: () = msg_send![accessibility_center,
            addObserver: observer selector: sel!(appearanceChanged:) name: accessibility_name object: nil
        ];
        let _: () = msg_send![accessibility_name, release];
    }
    let show_borders = unsafe { observe_show_borders(observer) };

    Some(SystemAppearanceObservation {
        changed,
        subscription: Box::new(MacosAppearanceSubscription {
            center,
            observer,
            name,
            accessibility_center,
            show_borders,
        }),
    })
}

struct NativeNotificationRegistration {
    center: id,
    name: id,
}

/// SAFETY: called on the AppKit thread with a live selector observer.
unsafe fn observe_show_borders(observer: id) -> Option<NativeNotificationRegistration> {
    let name = show_borders_changed_notification()?;
    let center: id = unsafe { msg_send![class!(NSNotificationCenter), defaultCenter] };
    if center == nil {
        return None;
    }
    let center: id = unsafe { msg_send![center, retain] };
    unsafe {
        let _: () = msg_send![center,
            addObserver: observer selector: sel!(appearanceChanged:) name: name object: nil
        ];
    }
    Some(NativeNotificationRegistration { center, name })
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
    accessibility_center: id,
    show_borders: Option<NativeNotificationRegistration>,
}

impl SystemAppearanceSubscription for MacosAppearanceSubscription {}

impl Drop for MacosAppearanceSubscription {
    fn drop(&mut self) {
        // SAFETY: NSDistributedNotificationCenter does not retain selector observers. Remove the
        // observer while every registration argument is still alive, then release owned objects.
        unsafe {
            if let Some(registration) = self.show_borders.as_ref() {
                let _: () = msg_send![registration.center,
                    removeObserver: self.observer name: registration.name object: nil
                ];
                let _: () = msg_send![registration.center, release];
            }
            let _: () = msg_send![self.accessibility_center, removeObserver: self.observer];
            let _: () = msg_send![self.accessibility_center, release];
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

    #[test]
    fn show_borders_prefers_the_independent_fact_and_falls_back_to_legacy_contrast() {
        assert!(!resolve_show_borders(Some(false), true));
        assert!(resolve_show_borders(Some(true), false));
        assert!(resolve_show_borders(None, true));
        assert!(!resolve_show_borders(None, false));
    }

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

    #[gpui::test]
    fn native_observer_coalesces_show_borders_notifications_and_removes_registration(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|_| {
            let Some(name) = show_borders_changed_notification() else {
                return;
            };
            let observation = MacosAppearancePlatform
                .observe()
                .expect("native appearance observation should be available");
            let SystemAppearanceObservation {
                changed,
                subscription,
            } = observation;
            // SAFETY: the public notification name and the default center are live for the process,
            // and notification delivery is synchronous on this AppKit thread.
            unsafe {
                let center: id = msg_send![class!(NSNotificationCenter), defaultCenter];
                for _ in 0..32 {
                    let _: () = msg_send![center, postNotificationName: name object: nil];
                }
            }
            assert_eq!(changed.len(), 1);
            changed
                .try_recv()
                .expect("coalesced Show Borders wakeup should be available");

            drop(subscription);
            assert!(changed.is_closed());
        });
    }
}
