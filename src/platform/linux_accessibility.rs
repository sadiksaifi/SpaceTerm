//! Linux Terminal Accessibility. AT-SPI publication through GPUI's AccessKit integration arrives
//! with a later accessibility wave; until then each Pane owns no native accessibility resources.
use gpui::{Pixels, Window};

use super::terminal_accessibility::{
    TerminalAccessibilityAdapter, TerminalAccessibilityAdapterFactory,
    TerminalAccessibilityUpdate,
};
use crate::terminal::{AccessibilityNotifications, TerminalAccessibilityModel};

pub(super) struct LinuxTerminalAccessibilityAdapterFactory;

impl TerminalAccessibilityAdapterFactory for LinuxTerminalAccessibilityAdapterFactory {
    fn create(
        &self,
        _: &Window,
        _: TerminalAccessibilityModel,
        _: &crate::appearance::ResolvedFontDescriptor,
        _: Pixels,
    ) -> Box<dyn TerminalAccessibilityAdapter> {
        Box::new(LinuxTerminalAccessibility)
    }
}

struct LinuxTerminalAccessibility;

impl TerminalAccessibilityAdapter for LinuxTerminalAccessibility {
    fn set_hierarchy(&mut self, _: bool, _: usize) {}

    fn update(&mut self, _: TerminalAccessibilityUpdate<'_>) -> AccessibilityNotifications {
        AccessibilityNotifications::default()
    }
}
