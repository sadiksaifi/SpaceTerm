//! Active XKB layout facts reported by the GPUI window backend.
use super::keyboard_layout::{KeyboardLayout, KeyboardLayoutAdapter, KeyboardLayoutUnavailable};

#[derive(Debug)]
pub(super) struct LinuxKeyboardLayout;

impl KeyboardLayoutAdapter for LinuxKeyboardLayout {
    fn snapshot(
        &self,
        platform: &dyn gpui::PlatformKeyboardLayout,
    ) -> Result<KeyboardLayout, KeyboardLayoutUnavailable> {
        let mut layout = KeyboardLayout::default();
        // Wayland may not have delivered its first keymap at installation. The first keymap
        // callback refreshes both dispatch and labels, including when the name is unchanged.
        for (base, shifted) in platform.shift_pairs().unwrap_or_default() {
            for command in [false, true] {
                layout.insert(command, base, shifted);
            }
        }
        Ok(layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Layout(Vec<(gpui::SharedString, gpui::SharedString)>);
    impl gpui::PlatformKeyboardLayout for Layout {
        fn id(&self) -> &str {
            "layout"
        }
        fn name(&self) -> &str {
            "Layout"
        }
        fn shift_pairs(&self) -> Option<&[(gpui::SharedString, gpui::SharedString)]> {
            Some(&self.0)
        }
    }

    #[test]
    fn linux_snapshot_waits_for_platform_pairs_and_copies_both_dispatch_layers() {
        let adapter = LinuxKeyboardLayout;
        assert_eq!(
            adapter
                .snapshot(&crate::platform::keyboard_layout::testing::UnknownLayout)
                .unwrap(),
            KeyboardLayout::default()
        );
        let layout = adapter
            .snapshot(&Layout(vec![
                ("3".into(), "§".into()),
                (",".into(), ";".into()),
            ]))
            .unwrap();
        for command in [false, true] {
            assert_eq!(layout.shifted(command, "3"), Some("§"));
            assert_eq!(layout.shifted(command, ","), Some(";"));
        }
        assert_eq!(layout.shifted(false, "1"), None);
    }
}
