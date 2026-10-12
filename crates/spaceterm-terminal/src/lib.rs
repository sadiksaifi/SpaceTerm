//! Portable terminal presentation and optional libghostty-vt engine.
//! Native input, clipboard execution, PTY ownership, and rendering stay in their adapters.
pub mod accessibility;
pub mod appearance;
pub mod attention;
mod color;
pub mod directory;
#[cfg(feature = "engine")]
pub mod emulator;
pub mod failure;
pub mod find;
pub mod geometry;
pub mod graphics;
pub mod hyperlink;
pub mod identity;
pub mod key;
#[cfg(feature = "engine")]
mod keyboard_protocol;
mod local_authority;
pub mod local_filesystem;
pub mod local_path;
pub mod metadata;
#[cfg(all(
    test,
    feature = "native-tests",
    any(target_os = "macos", target_os = "linux")
))]
mod native_test_adapters;
pub mod permission_request;
pub mod pointer_input;
pub mod selection;
pub mod snapshot;
pub mod title;
pub use accessibility::{TerminalAccessibilityModel, TerminalAccessibilityState};
pub use appearance::TerminalAppearanceUpdate;
#[cfg(any(test, feature = "test-support"))]
pub use appearance::test_terminal_appearance_update;
#[cfg(feature = "engine")]
pub use emulator::{EmulatorAction, ScrollbackMovement, TerminalEmulator};
pub use find::{FindDirection, FindHighlightSpan, FindQueryGeneration, TerminalFindSnapshot};
pub use hyperlink::HyperlinkTarget;
pub use key::{InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, PhysicalKey};
pub use snapshot::*;
