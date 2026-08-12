#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::all)]
#![allow(rustdoc::all)]

mod bindings;

use std::ops::Deref;

pub use bindings::*;

/// Initialize a "sized" FFI object.
#[macro_export]
macro_rules! sized {
    ($ty:ty) => {{
        let mut t = <$ty as ::std::default::Default>::default();
        t.size = ::std::mem::size_of::<$ty>();
        t
    }};
}

impl<S> From<S> for bindings::String
where
    S: Deref<Target = str>,
{
    fn from(value: S) -> Self {
        Self {
            ptr: value.as_ptr(),
            len: value.len(),
        }
    }
}

impl bindings::String {
    /// # Safety
    ///
    /// The caller must uphold that the associated lifetime is valid
    /// with the given context behind the FFI string, and that it contains
    /// valid UTF-8 data.
    pub unsafe fn to_str<'a>(self) -> &'a str {
        // SAFETY: To be upheld by caller
        let slice = unsafe { std::slice::from_raw_parts(self.ptr, self.len) };
        unsafe { std::str::from_utf8_unchecked(slice) }
    }
}

#[cfg(test)]
mod abi_tests {
    use super::*;

    #[test]
    fn terminal_effect_extension_has_stable_option_and_callback_shapes() {
        unsafe extern "C" fn resolve(
            _: Terminal,
            _: *mut std::os::raw::c_void,
            _: String,
            _: *mut Buffer,
            _: *mut Buffer,
        ) -> HyperlinkResolution::Type {
            HyperlinkResolution::SUPPRESS
        }

        assert_eq!(TerminalOption::HYPERLINK_RESOLVE, 27);
        assert_eq!(TerminalOption::SEMANTIC_PROMPT, 28);
        assert_eq!(TerminalOption::PROGRESS_REPORT, 29);
        assert_eq!(HyperlinkResolution::PASSTHROUGH, 0);
        assert_eq!(HyperlinkResolution::REPLACE, 1);
        assert_eq!(HyperlinkResolution::SUPPRESS, 2);
        let callback: TerminalHyperlinkResolveFn = Some(resolve);
        assert!(callback.is_some());
        let _: unsafe extern "C" fn(*const GridRef, *mut u8, usize, *mut usize) -> Result::Type =
            ghostty_grid_ref_hyperlink_userdata;
        assert_eq!(
            std::mem::size_of::<TerminalHyperlinkResolveFn>(),
            std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<TerminalSemanticPromptFn>(),
            std::mem::size_of::<usize>()
        );
        assert_eq!(
            std::mem::size_of::<TerminalProgressReportFn>(),
            std::mem::size_of::<usize>()
        );
    }
}
