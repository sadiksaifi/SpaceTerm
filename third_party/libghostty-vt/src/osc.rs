//! Handling OSC (Operating System Command) escape sequences.

use std::{ffi::CStr, marker::PhantomData};

use crate::{
    alloc::{Allocator, Object, Ref},
    error::{Result, from_result},
    ffi,
};

/// OSC (Operating System Command) sequence parser and command handling.
///
/// The parser operates in a streaming fashion, processing input byte-by-byte
/// to handle OSC sequences that may arrive in fragments across multiple reads.
/// This interface makes it easy to integrate into most environments and avoids
/// over-allocating buffers.
#[derive(Debug)]
pub struct Parser<'alloc>(Object<'alloc, ffi::OscParserImpl>);

impl<'alloc> Parser<'alloc> {
    /// Create a new OSC parser.
    pub fn new() -> Result<Self> {
        // SAFETY: A NULL allocator is always valid
        unsafe { Self::new_inner(std::ptr::null()) }
    }

    /// Create a new OSC parser with a custom allocator.
    ///
    /// See the [crate-level documentation](crate#memory-management-and-lifetimes)
    /// regarding custom memory management and lifetimes.
    pub fn new_with_alloc<'ctx: 'alloc>(alloc: &'alloc Allocator<'ctx>) -> Result<Self> {
        // SAFETY: Borrow checking should forbid invalid allocators
        unsafe { Self::new_inner(alloc.to_raw()) }
    }

    unsafe fn new_inner(alloc: *const ffi::Allocator) -> Result<Self> {
        let mut raw: ffi::OscParser = std::ptr::null_mut();
        let result = unsafe { ffi::ghostty_osc_new(alloc, &raw mut raw) };
        from_result(result)?;
        Ok(Self(Object::new(raw)?))
    }

    /// Reset an OSC parser instance to its initial state.
    ///
    /// Resets the parser state, clearing any partially parsed OSC sequences
    /// and returning the parser to its initial state. This is useful for
    /// reusing a parser instance or recovering from parse errors.
    pub fn reset(&mut self) {
        unsafe { ffi::ghostty_osc_reset(self.0.as_raw()) }
    }

    /// Parse the next byte in an OSC sequence.
    ///
    /// Processes a single byte as part of an OSC sequence. The parser maintains
    /// internal state to track the progress through the sequence. Call this
    /// function for each byte in the sequence data.
    ///
    /// When finished pumping the parser with bytes, call [`Parser::end`] to
    /// get the final result.
    pub fn next_byte(&mut self, byte: u8) {
        unsafe { ffi::ghostty_osc_next(self.0.as_raw(), byte) }
    }

    /// Finalize OSC parsing and retrieve the parsed command.
    ///
    /// Call this function after feeding all bytes of an OSC sequence to the parser
    /// using [`Parser::next_byte`] with the exception of the terminating character
    /// (ESC or ST). This function finalizes the parsing process and returns the
    /// parsed OSC command. Invalid commands will return a command with type
    /// [`CommandType::Invalid`].
    ///
    /// The terminator parameter specifies the byte that terminated the OSC
    /// sequence (typically 0x07 for BEL or 0x5C for ST after ESC).
    /// This information is preserved in the parsed command so that responses
    /// can use the same terminator format for better compatibility with the
    /// calling program. For commands that do not require a response, this
    /// parameter is ignored and the resulting command will not retain the
    /// terminator information.
    pub fn end<'p>(&'p mut self, terminator: u8) -> Command<'p, 'alloc> {
        let raw = unsafe { ffi::ghostty_osc_end(self.0.as_raw(), terminator) };
        Command {
            inner: Ref::new(raw).ok(),
            _parser: PhantomData,
        }
    }
}

impl Drop for Parser<'_> {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_osc_free(self.0.as_raw()) }
    }
}

/// A parsed OSC (Operating System Command) command.
///
/// The command can be queried for its type and associated data.
#[derive(Debug)]
pub struct Command<'p, 'alloc> {
    inner: Option<Ref<'p, ffi::OscCommandImpl>>,
    _parser: PhantomData<&'p Parser<'alloc>>,
}

impl<'p> Command<'p, '_> {
    /// Get the type of an OSC command.
    ///
    /// This can be used to determine what kind of command was parsed and
    /// what data might be available from it.
    #[must_use]
    pub fn command_type(self) -> CommandType<'p> {
        self.command_type_inner().unwrap_or(CommandType::Invalid)
    }

    fn command_type_inner(&self) -> Option<CommandType<'p>> {
        use ffi::OscCommandType as Type;

        let inner = self.inner.as_ref()?;
        let raw_type = unsafe { ffi::ghostty_osc_command_type(inner.as_raw()) };
        Some(match raw_type {
            Type::CHANGE_WINDOW_TITLE => CommandType::ChangeWindowTitle {
                title: self.window_title()?,
            },
            Type::CHANGE_WINDOW_ICON => CommandType::ChangeWindowIcon,
            Type::SEMANTIC_PROMPT => CommandType::SemanticPrompt,
            Type::CLIPBOARD_CONTENTS => CommandType::ClipboardContents,
            Type::REPORT_PWD => CommandType::ReportPwd,
            Type::MOUSE_SHAPE => CommandType::MouseShape,
            Type::COLOR_OPERATION => CommandType::ColorOperation,
            Type::KITTY_COLOR_PROTOCOL => CommandType::KittyColorProtocol,
            Type::SHOW_DESKTOP_NOTIFICATION => CommandType::ShowDesktopNotification,
            Type::HYPERLINK_START => CommandType::HyperlinkStart,
            Type::HYPERLINK_END => CommandType::HyperlinkEnd,
            Type::CONEMU_SLEEP => CommandType::ConemuSleep,
            Type::CONEMU_SHOW_MESSAGE_BOX => CommandType::ConemuShowMessageBox,
            Type::CONEMU_CHANGE_TAB_TITLE => CommandType::ConemuChangeTabTitle,
            Type::CONEMU_PROGRESS_REPORT => CommandType::ConemuProgressReport,
            Type::CONEMU_WAIT_INPUT => CommandType::ConemuWaitInput,
            Type::CONEMU_GUIMACRO => CommandType::ConemuGuiMacro,
            Type::CONEMU_RUN_PROCESS => CommandType::ConemuRunProcess,
            Type::CONEMU_OUTPUT_ENVIRONMENT_VARIABLE => {
                CommandType::ConemuOutputEnvironmentVariable
            }
            Type::CONEMU_XTERM_EMULATION => CommandType::ConemuXtermEmulation,
            Type::CONEMU_COMMENT => CommandType::ConemuComment,
            Type::KITTY_TEXT_SIZING => CommandType::KittyTextSizing,

            _ => return None,
        })
    }

    fn window_title(&self) -> Option<&'p str> {
        let inner = self.inner.as_ref()?;
        let mut value: *const std::ffi::c_char = std::ptr::null();
        // SAFETY: This tag writes a const char pointer into the output.
        let result = unsafe {
            ffi::ghostty_osc_command_data(
                inner.as_raw(),
                ffi::OscCommandData::CHANGE_WINDOW_TITLE_STR,
                std::ptr::from_mut(&mut value).cast(),
            )
        };
        if !result || value.is_null() {
            return None;
        }
        // SAFETY: The native title is NUL-terminated and valid for the parser borrow 'p.
        unsafe { CStr::from_ptr(value) }.to_str().ok()
    }
}

/// Type of an OSC command.
#[repr(i32)]
#[derive(Debug, Clone, Default)]
#[expect(missing_docs, reason = "missing upstream docs")]
pub enum CommandType<'p> {
    #[default]
    Invalid,
    ChangeWindowTitle {
        /// Window title string data.
        title: &'p str,
    },
    ChangeWindowIcon,
    SemanticPrompt,
    ClipboardContents,
    ReportPwd,
    MouseShape,
    ColorOperation,
    KittyColorProtocol,
    ShowDesktopNotification,
    HyperlinkStart,
    HyperlinkEnd,
    ConemuSleep,
    ConemuShowMessageBox,
    ConemuChangeTabTitle,
    ConemuProgressReport,
    ConemuWaitInput,
    ConemuGuiMacro,
    ConemuRunProcess,
    ConemuOutputEnvironmentVariable,
    ConemuXtermEmulation,
    ConemuComment,
    KittyTextSizing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_title_validates_utf8_and_borrows_exact_text() {
        let mut parser = Parser::new().unwrap();
        for title in ["SpaceTerm café 終端", ""] {
            parser.reset();
            for byte in b"2;".iter().copied().chain(title.bytes()) {
                parser.next_byte(byte);
            }
            let CommandType::ChangeWindowTitle { title: actual } = parser.end(7).command_type()
            else {
                panic!("expected a window title");
            };
            assert_eq!(actual, title);
        }
        parser.reset();
        for byte in b"2;\xff" {
            parser.next_byte(*byte);
        }
        assert!(matches!(parser.end(7).command_type(), CommandType::Invalid));
        parser.reset();
        for byte in b"not-an-osc-command" {
            parser.next_byte(*byte);
        }
        assert!(matches!(parser.end(7).command_type(), CommandType::Invalid));
    }
}
