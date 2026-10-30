//! POSIX shell words for the command lines the CLI prints for a person to copy, such as a
//! confirmation or a target handle. Single quotes keep every character literal in `sh`, Bash,
//! Zsh and Fish.

/// `text` in single quotes, with each `'` closed, escaped and reopened.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `text` as one shell word: as it is when every character is one no shell interprets, quoted
/// otherwise. Brackets, as in `[::1]:9231`, are glob characters, so they are quoted.
pub fn word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.,:/=@%+".contains(&b));
    if plain { text.to_owned() } else { quote(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_words_with_shell_characters_are_quoted() {
        assert_eq!(word("id:4e4e"), "id:4e4e");
        assert_eq!(word("192.0.2.4:9231"), "192.0.2.4:9231");
        assert_eq!(word("[::1]:9231"), "'[::1]:9231'");
        assert_eq!(word(""), "''");
        assert_eq!(word("it's"), "'it'\\''s'");
        assert_eq!(quote("path:/tmp"), "'path:/tmp'");
    }
}
