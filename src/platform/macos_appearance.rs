use std::{ffi::c_void, sync::OnceLock};

use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSAppearance, NSApplication, NSWorkspace};
use objc2_foundation::{
    NSDistributedNotificationCenter, NSNotification, NSNotificationCenter,
    NSNotificationSuspensionBehavior, NSObject, NSObjectProtocol, NSString, NSUserDefaults,
};

use super::appearance::{
    AccessibilityDisplayOptions, AppearancePlatform, SystemAppearanceObservation,
    SystemAppearanceSubscription, WindowBackdrop,
};
use crate::appearance::Appearance;

const APPEARANCE_NOTIFICATION: &str = "AppleInterfaceThemeChangedNotification";
const ACCESSIBILITY_FRAMEWORK: &[u8] =
    b"/System/Library/Frameworks/Accessibility.framework/Accessibility\0";
const SHOW_BORDERS_GETTER: &[u8] = b"AXShowBordersEnabled\0";
const SHOW_BORDERS_NOTIFICATION: &[u8] = b"AXShowBordersEnabledStatusDidChangeNotification\0";

type ShowBordersGetter = unsafe extern "C" fn() -> Bool;

#[derive(Clone, Copy)]
struct ShowBordersSymbols {
    _framework: usize,
    getter: ShowBordersGetter,
    notification: Option<usize>,
}

static SHOW_BORDERS_SYMBOLS: OnceLock<Option<ShowBordersSymbols>> = OnceLock::new();

fn show_borders_symbols() -> Option<ShowBordersSymbols> {
    *SHOW_BORDERS_SYMBOLS.get_or_init(|| {
        // SAFETY: The retained public framework keeps its function and notification symbols live.
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
                let name = *notification.cast::<*mut NSString>();
                (!name.is_null()).then_some(name as usize)
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
        // SAFETY: Symbol loading verified the public C function and retained its framework.
        unsafe { (symbols.getter)().as_bool() }
    });
    resolve_show_borders(native, increase_contrast)
}

const fn resolve_show_borders(native: Option<bool>, increase_contrast: bool) -> bool {
    match native {
        Some(shown) => shown,
        None => increase_contrast,
    }
}

fn show_borders_changed_notification() -> Option<Retained<NSString>> {
    let pointer = show_borders_symbols()?.notification? as *mut NSString;
    // SAFETY: The retained Accessibility framework owns this NSString for the process lifetime.
    unsafe { Retained::retain(pointer) }
}

pub(crate) struct MacosAppearancePlatform;

impl MacosAppearancePlatform {
    fn observe_with_marker(&self, mtm: MainThreadMarker) -> Option<SystemAppearanceObservation> {
        observe_distributed(
            NSDistributedNotificationCenter::defaultCenter(),
            APPEARANCE_NOTIFICATION,
            mtm,
        )
    }

    fn apply_native_appearance_with_marker(&self, appearance: Appearance, mtm: MainThreadMarker) {
        let name = NSString::from_str(match appearance {
            Appearance::Light => "NSAppearanceNameAqua",
            Appearance::Dark => "NSAppearanceNameDarkAqua",
        });
        if let Some(selected) = NSAppearance::appearanceNamed(&name) {
            NSApplication::sharedApplication(mtm).setAppearance(Some(&selected));
        }
    }
}

impl AppearancePlatform for MacosAppearancePlatform {
    fn prefers_reduced_motion(&self) -> bool {
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    fn supports_native_window_transparency(&self) -> bool {
        true
    }

    fn accessibility_display_options(&self) -> AccessibilityDisplayOptions {
        let workspace = NSWorkspace::sharedWorkspace();
        let contrast = workspace.accessibilityDisplayShouldIncreaseContrast();
        AccessibilityDisplayOptions {
            reduce_transparency: workspace.accessibilityDisplayShouldReduceTransparency(),
            increase_contrast: contrast,
            show_borders: show_borders_enabled(contrast),
            differentiate_without_color: workspace
                .accessibilityDisplayShouldDifferentiateWithoutColor(),
        }
    }

    fn apply_window_backdrop(&self, window: &gpui::Window, backdrop: WindowBackdrop) {
        super::macos_window_backdrop::apply(window, backdrop);
    }

    fn system_appearance(&self) -> Option<Appearance> {
        let defaults = NSUserDefaults::standardUserDefaults();
        let value = defaults.stringForKey(&NSString::from_str("AppleInterfaceStyle"));
        Some(if value.as_deref() == Some(&*NSString::from_str("Dark")) {
            Appearance::Dark
        } else {
            Appearance::Light
        })
    }

    fn observe(&self) -> Option<SystemAppearanceObservation> {
        self.observe_with_marker(MainThreadMarker::new()?)
    }

    fn apply_native_appearance(&self, appearance: Appearance) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        self.apply_native_appearance_with_marker(appearance, mtm);
    }
}

struct AppearanceObserverIvars {
    sender: async_channel::Sender<()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and define_class! drops the sender ivar.
    #[unsafe(super(NSObject))]
    #[name = "SpaceTermDistributedAppearanceObserver"]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppearanceObserverIvars]
    struct AppearanceObserver;

    impl AppearanceObserver {
        #[unsafe(method(appearanceChanged:))]
        fn appearance_changed(&self, _notification: &NSNotification) {
            let _ = self.ivars().sender.try_send(());
        }
    }

    unsafe impl NSObjectProtocol for AppearanceObserver {}
);

impl AppearanceObserver {
    fn new(mtm: MainThreadMarker, sender: async_channel::Sender<()>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppearanceObserverIvars { sender });
        // SAFETY: NSObject's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

fn observe_distributed(
    center: Retained<NSDistributedNotificationCenter>,
    notification_name: &str,
    mtm: MainThreadMarker,
) -> Option<SystemAppearanceObservation> {
    let (sender, changed) = async_channel::bounded(1);
    let observer = AppearanceObserver::new(mtm, sender);
    let name = NSString::from_str(notification_name);

    // SAFETY: The selector belongs to this retained observer. Immediate delivery preserves
    // changes while another application is active. The subscription removes the registration.
    unsafe {
        center.addObserver_selector_name_object_suspensionBehavior(
            &observer,
            sel!(appearanceChanged:),
            Some(&name),
            None,
            NSNotificationSuspensionBehavior::DeliverImmediately,
        );
    }

    let accessibility_center = NSWorkspace::sharedWorkspace().notificationCenter();
    let accessibility_name =
        NSString::from_str("NSWorkspaceAccessibilityDisplayOptionsDidChangeNotification");
    // SAFETY: The selector belongs to the retained observer and the subscription removes it.
    unsafe {
        accessibility_center.addObserver_selector_name_object(
            &observer,
            sel!(appearanceChanged:),
            Some(&accessibility_name),
            None,
        );
    }
    let show_borders = observe_show_borders(&observer);

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
    center: Retained<NSNotificationCenter>,
    name: Retained<NSString>,
}

fn observe_show_borders(observer: &AppearanceObserver) -> Option<NativeNotificationRegistration> {
    let name = show_borders_changed_notification()?;
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: The selector belongs to the live observer and the subscription removes it.
    unsafe {
        center.addObserver_selector_name_object(
            observer,
            sel!(appearanceChanged:),
            Some(&name),
            None,
        );
    }
    Some(NativeNotificationRegistration { center, name })
}

struct MacosAppearanceSubscription {
    center: Retained<NSDistributedNotificationCenter>,
    observer: Retained<AppearanceObserver>,
    name: Retained<NSString>,
    accessibility_center: Retained<NSNotificationCenter>,
    show_borders: Option<NativeNotificationRegistration>,
}

impl SystemAppearanceSubscription for MacosAppearanceSubscription {}

impl Drop for MacosAppearanceSubscription {
    fn drop(&mut self) {
        // SAFETY: The observer and names remain alive until all registrations are removed.
        unsafe {
            if let Some(registration) = &self.show_borders {
                registration.center.removeObserver_name_object(
                    &self.observer,
                    Some(&registration.name),
                    None,
                );
            }
            self.accessibility_center.removeObserver(&self.observer);
            self.center
                .removeObserver_name_object(&self.observer, Some(&self.name), None);
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
            let mtm = super::super::native_test_marker();
            let application = NSApplication::sharedApplication(mtm);
            let previous = application.appearance();
            for appearance in [Appearance::Light, Appearance::Dark] {
                platform.apply_native_appearance_with_marker(appearance, mtm);
                let effective = application.appearance().unwrap();
                let expected = NSString::from_str(match appearance {
                    Appearance::Light => "NSAppearanceNameAqua",
                    Appearance::Dark => "NSAppearanceNameDarkAqua",
                });
                assert_eq!(&*effective.name(), &*expected);
                assert_eq!(platform.system_appearance(), system);
            }
            let observation = platform
                .observe_with_marker(mtm)
                .expect("native appearance observation should be available");
            drop(observation);
            application.setAppearance(previous.as_deref());
        });
    }

    #[gpui::test]
    fn native_observer_coalesces_wakeups_and_closes_with_its_owner(cx: &mut gpui::TestAppContext) {
        cx.update(|_| {
            let (sender, changed) = async_channel::bounded(1);
            let observer = AppearanceObserver::new(super::super::native_test_marker(), sender);
            // SAFETY: The test supplies an immutable name and no source object.
            let notification = unsafe {
                NSNotification::notificationWithName_object(
                    &NSString::from_str("SpaceTermTest"),
                    None,
                )
            };
            // SAFETY: This selector belongs to the live observer and receives a valid notification.
            unsafe {
                for _ in 0..32 {
                    let _: () = msg_send![&*observer, appearanceChanged: &*notification];
                }
            }
            assert_eq!(changed.len(), 1);
            changed
                .try_recv()
                .expect("coalesced appearance wakeup should be available");
            // SAFETY: This selector belongs to the live observer and receives a valid notification.
            let _: () = unsafe { msg_send![&*observer, appearanceChanged: &*notification] };
            changed
                .try_recv()
                .expect("observer should remain reusable after a wakeup");
            drop(observer);
            assert!(changed.is_closed());
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
                .observe_with_marker(super::super::native_test_marker())
                .expect("native appearance observation should be available");
            let SystemAppearanceObservation {
                changed,
                subscription,
            } = observation;
            let center = NSNotificationCenter::defaultCenter();
            // SAFETY: This public name is retained and delivery is synchronous on the AppKit thread.
            unsafe {
                for _ in 0..32 {
                    center.postNotificationName_object(&name, None);
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
