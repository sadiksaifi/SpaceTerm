//! Translate the active layout into GPUI's shortcut alphabet on the application thread.

use std::ffi::c_void;
use std::ptr::NonNull;

use super::keyboard_layout::{KeyboardLayout, KeyboardLayoutAdapter, KeyboardLayoutUnavailable};

#[derive(Debug)]
pub(crate) struct MacosKeyboardLayout;

impl KeyboardLayoutAdapter for MacosKeyboardLayout {
    fn snapshot(
        &self,
        _: &dyn gpui::PlatformKeyboardLayout,
    ) -> Result<KeyboardLayout, KeyboardLayoutUnavailable> {
        // TIS returns the backing keyboard layout even when an input method is selected.
        let source = Source(
            NonNull::new(unsafe { TISCopyCurrentKeyboardLayoutInputSource() })
                .ok_or(KeyboardLayoutUnavailable)?,
        );
        snapshot(&source)
    }
}

struct Source(NonNull<c_void>);
impl Drop for Source {
    fn drop(&mut self) {
        // Owned by a TIS Copy/Create call; properties remain borrowed until this release.
        unsafe { CFRelease(self.0.as_ptr()) };
    }
}

fn snapshot(source: &Source) -> Result<KeyboardLayout, KeyboardLayoutUnavailable> {
    // The source owns the CFData and its bytes throughout translation.
    let data =
        unsafe { TISGetInputSourceProperty(source.0.as_ptr(), kTISPropertyUnicodeKeyLayoutData) };
    if data.is_null() {
        return Err(KeyboardLayoutUnavailable);
    }
    let bytes = unsafe { CFDataGetBytePtr(data) };
    if bytes.is_null() {
        return Err(KeyboardLayoutUnavailable);
    }
    let keyboard_type = u32::from(unsafe { LMGetKbdType() });
    let translate = |code, modifiers| translate(bytes.cast(), code, modifiers, keyboard_type);
    let base_a = translate(0, 0)?;
    let command_a = translate(0, 1)?;
    let always_command = !base_a.is_ascii() && command_a.is_ascii();
    let mut layout = KeyboardLayout::default();
    for code in 0..128 {
        let (Ok(base), Ok(shifted), Ok(command_base), Ok(command_shifted)) = (
            translate(code, 0),
            translate(code, 2),
            translate(code, 1),
            translate(code, 3),
        ) else {
            continue;
        };
        for command in [false, true] {
            // Match GPUI's Command-layout handling, including Norwegian and Ukrainian.
            let (base, shifted) = if command || always_command {
                let shifted = if command_shifted != command_base {
                    command_shifted.clone()
                } else if command_base.to_ascii_uppercase() != command_base {
                    command_base.to_ascii_uppercase()
                } else {
                    shifted.clone()
                };
                (&command_base, shifted)
            } else {
                (&base, shifted.clone())
            };
            layout.insert(command, base, &shifted);
        }
    }
    Ok(layout)
}

fn translate(
    layout: *const c_void,
    code: u16,
    modifiers: u32,
    keyboard_type: u32,
) -> Result<String, KeyboardLayoutUnavailable> {
    let mut dead_state = 0;
    let mut buffer = [0_u16; 255];
    let mut length = 0;
    // Carbon modifier bits are shifted right by eight: Command = 1, Shift = 2.
    // A following Space obtains the standalone accent for dead keys, as GPUI does.
    for key in [code, 49] {
        let status = unsafe {
            UCKeyTranslate(
                layout,
                key,
                0,
                modifiers,
                keyboard_type,
                0,
                &mut dead_state,
                buffer.len(),
                &mut length,
                buffer.as_mut_ptr(),
            )
        };
        if status != 0 || length > buffer.len() {
            return Err(KeyboardLayoutUnavailable);
        }
        if dead_state == 0 {
            break;
        }
    }
    String::from_utf16(&buffer[..length]).map_err(|_| KeyboardLayoutUnavailable)
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, property: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u16;
    fn UCKeyTranslate(
        layout: *const c_void,
        code: u16,
        action: u16,
        modifiers: u32,
        keyboard_type: u32,
        options: u32,
        dead_state: *mut u32,
        capacity: usize,
        length: *mut usize,
        buffer: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFRelease(value: *const c_void);
}

#[cfg(all(test, feature = "native-tests"))]
pub(crate) mod tests {
    use super::*;
    use crate::keybindings::{Command, KeybindingPreferences, Shortcut, SystemReservation};
    use std::rc::Rc;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCreateInputSourceList(properties: *const c_void, include_all: u8) -> *mut c_void;
        static kTISPropertyInputSourceID: *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *mut c_void;
        fn CFRetain(value: *const c_void) -> *const c_void;
        fn CFStringGetCString(
            string: *const c_void,
            buffer: *mut u8,
            capacity: isize,
            encoding: u32,
        ) -> u8;
    }

    fn layout(id: &str) -> KeyboardLayout {
        assert!(objc2::MainThreadMarker::new().is_some());
        let sources =
            Source(NonNull::new(unsafe { TISCreateInputSourceList(std::ptr::null(), 1) }).unwrap());
        for index in 0..unsafe { CFArrayGetCount(sources.0.as_ptr()) } {
            let source = unsafe { CFArrayGetValueAtIndex(sources.0.as_ptr(), index) };
            let source_id = unsafe { TISGetInputSourceProperty(source, kTISPropertyInputSourceID) };
            let mut buffer = [0_u8; 256];
            if !source_id.is_null()
                && unsafe {
                    CFStringGetCString(
                        source_id,
                        buffer.as_mut_ptr(),
                        buffer.len() as isize,
                        0x08000100,
                    )
                } != 0
                && std::ffi::CStr::from_bytes_until_nul(&buffer)
                    .unwrap()
                    .to_bytes()
                    == id.as_bytes()
            {
                unsafe { CFRetain(source) };
                return snapshot(&Source(NonNull::new(source).unwrap())).unwrap();
            }
        }
        panic!("required built-in keyboard layout unavailable: {id}");
    }

    pub(crate) fn native_layouts_resolve_dispatch_and_system_reservations() {
        for (id, source, expected) in [
            ("com.apple.keylayout.US", "shift-cmd-7", "cmd-&"),
            ("com.apple.keylayout.Norwegian", "shift-cmd-7", "cmd-/"),
            ("com.apple.keylayout.German", "shift-cmd-ö", "cmd-Ö"),
            ("com.apple.keylayout.French", "shift-cmd-3", "cmd-3"),
        ] {
            let layout = layout(id);
            assert_eq!(
                Shortcut::parse(source)
                    .unwrap()
                    .resolve(&layout)
                    .to_string(),
                expected,
                "{id}"
            );
        }
        let german = layout("com.apple.keylayout.German");
        let mut profile = crate::desktop_profile::default_keymap::profile(
            Rc::new(german),
            super::super::macos_reserved_shortcuts::shortcuts(),
        )
        .unwrap();
        profile
            .refresh_layout(&crate::platform::keyboard_layout::testing::UnknownLayout)
            .unwrap();
        for source in ["shift-cmd-3", "cmd-§"] {
            let preferences: KeybindingPreferences =
                serde_json::from_value(serde_json::json!({"new_workspace": source})).unwrap();
            assert_eq!(
                profile
                    .resolve(&preferences)
                    .inactive_override(Command::NewWorkspace),
                Some(crate::keybindings::Reservation::System(
                    SystemReservation::Screenshot
                ))
            );
        }
        for (id, source, key, modifiers) in [
            (
                "com.apple.keylayout.Norwegian",
                "shift-cmd-7",
                "/",
                gpui::Modifiers::command(),
            ),
            (
                "com.apple.keylayout.Turkish-QWERTY-PC",
                "shift-cmd-ı",
                "I",
                gpui::Modifiers::command(),
            ),
            (
                "com.apple.keylayout.Norwegian",
                "ctrl-shift-2",
                "\"",
                gpui::Modifiers::control(),
            ),
            (
                "com.apple.keylayout.German",
                "ctrl-shift-2",
                "\"",
                gpui::Modifiers::control(),
            ),
        ] {
            let mut profile = crate::desktop_profile::default_keymap::profile(
                Rc::new(layout(id)),
                super::super::macos_reserved_shortcuts::shortcuts(),
            )
            .unwrap();
            profile
                .refresh_layout(&crate::platform::keyboard_layout::testing::UnknownLayout)
                .unwrap();
            let preferences: KeybindingPreferences =
                serde_json::from_value(serde_json::json!({"new_workspace": source})).unwrap();
            let native = gpui::Keystroke {
                modifiers,
                key: key.into(),
                key_char: None,
            };
            assert!(
                profile
                    .resolve(&preferences)
                    .key_bindings()
                    .iter()
                    .any(|binding| {
                        binding
                            .action()
                            .partial_eq(Command::NewWorkspace.action().as_ref())
                            && native.should_match(&binding.keystrokes()[0])
                    }),
                "{id}"
            );
        }
        assert_eq!(
            Shortcut::parse("shift-cmd-k")
                .unwrap()
                .resolve(&layout("com.apple.keylayout.US"))
                .to_string(),
            "shift-cmd-k"
        );
    }
}
