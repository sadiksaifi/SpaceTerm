#[cfg(test)]
pub use spaceterm_terminal_client::find::TerminalFindSnapshot;
#[cfg(feature = "desktop")]
pub use spaceterm_terminal_client::find::{FindDirection, MAX_QUERY_BYTES};
pub use spaceterm_terminal_client::find::{FindHighlightSpan, FindQueryGeneration};
