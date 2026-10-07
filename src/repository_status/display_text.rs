//! Display sanitizing for repository-supplied text: paths, branch names, and titles.

use std::sync::Arc;

/// Decodes untrusted bytes lossily and replaces control and bidirectional formatting characters
/// with U+FFFD, so repository text cannot reorder or hide surrounding interface text.
pub(crate) fn display_text(bytes: &[u8]) -> Arc<str> {
    sanitize_display_text(&String::from_utf8_lossy(bytes)).into()
}

/// Replaces control and bidirectional formatting characters with U+FFFD.
pub(crate) fn sanitize_display_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if is_unsafe_display_character(character) {
                char::REPLACEMENT_CHARACTER
            } else {
                character
            }
        })
        .collect()
}

fn is_unsafe_display_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_should_replace_controls_and_bidirectional_formatting() {
        let text = display_text("a\tb\u{1b}c\u{7f}d\u{202e}e\u{2066}f\u{200f}g\u{061c}h\u{2028}i".as_bytes());

        assert_eq!(
            &*text,
            "a\u{fffd}b\u{fffd}c\u{fffd}d\u{fffd}e\u{fffd}f\u{fffd}g\u{fffd}h\u{fffd}i"
        );
    }

    #[test]
    fn display_text_should_decode_invalid_utf8_lossily_and_keep_ordinary_text() {
        assert_eq!(&*display_text(b"caf\xc3\xa9 \xff/\xd7\x90.rs"), "café \u{fffd}/א.rs");
    }
}
