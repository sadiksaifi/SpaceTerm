use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSUserInterfaceLayoutDirection};
use spaceterm_ui::TextDirection;

/// Resolves the logical direction AppKit selected for the current application locale.
pub(crate) fn current_text_direction() -> TextDirection {
    let Some(mtm) = MainThreadMarker::new() else {
        return TextDirection::LeftToRight;
    };
    let native_direction = NSApplication::sharedApplication(mtm).userInterfaceLayoutDirection();
    text_direction_from_native(native_direction)
}

const fn text_direction_from_native(
    native_direction: NSUserInterfaceLayoutDirection,
) -> TextDirection {
    if native_direction.0 == NSUserInterfaceLayoutDirection::RightToLeft.0 {
        TextDirection::RightToLeft
    } else {
        TextDirection::LeftToRight
    }
}

pub(super) struct ApplicationLocale;
impl super::locale::LocaleDirection for ApplicationLocale {
    fn text_direction(&self) -> TextDirection {
        current_text_direction()
    }
}

#[cfg(all(test, feature = "macos-native-tests"))]
mod tests {
    use super::*;

    #[test]
    fn native_layout_direction_maps_to_bounded_locale_behavior() {
        assert_eq!(
            (
                text_direction_from_native(NSUserInterfaceLayoutDirection::LeftToRight),
                text_direction_from_native(NSUserInterfaceLayoutDirection::RightToLeft),
            ),
            (TextDirection::LeftToRight, TextDirection::RightToLeft)
        );
    }
}
