//! Linux keyboard layout facts. GPUI does not yet report the active layout's shift pairs, so the
//! US English pairs stand in until it does; spellings for other layouts resolve as written.
use super::keyboard_layout::{KeyboardLayout, KeyboardLayoutAdapter, KeyboardLayoutUnavailable};

#[derive(Debug)]
pub(super) struct LinuxKeyboardLayout;

impl KeyboardLayoutAdapter for LinuxKeyboardLayout {
    fn snapshot(&self) -> Result<KeyboardLayout, KeyboardLayoutUnavailable> {
        Ok(KeyboardLayout::us_english())
    }
}
