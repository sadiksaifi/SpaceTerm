use std::path::PathBuf;

/// Selected from the local shell configuration, never inferred from the operating system.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShellInsertionDialect {
    Posix,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileInsertionPolicy {
    pub(crate) paths: crate::local_path::LocalPathSemantics,
    pub(crate) shell: ShellInsertionDialect,
}

impl FileInsertionPolicy {
    #[cfg(test)]
    pub(crate) const fn fixture() -> Self {
        Self {
            paths: crate::local_path::LocalPathSemantics::Posix,
            shell: ShellInsertionDialect::Posix,
        }
    }
}

pub(crate) const MAX_FILE_ITEMS: usize = 256;
pub(crate) const MAX_FILE_INSERTION_BYTES: usize = 1024 * 1024;

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct FileInsertion {
    pub(crate) text: String,
}

pub(crate) fn prepare_file_insertion(
    policy: FileInsertionPolicy,
    paths: &[PathBuf],
) -> Result<FileInsertion, &'static str> {
    if paths.is_empty() {
        return Err("no file paths were supplied");
    }
    if paths.len() > MAX_FILE_ITEMS {
        return Err("too many file paths were supplied");
    }
    let mut text = String::new();
    for path in paths {
        if !policy.paths.is_absolute(path) {
            return Err("file paths must be absolute");
        }
        let value = path.to_str().ok_or("file paths must be valid UTF-8")?;
        if value.as_bytes().contains(&0) {
            return Err("file paths must not contain NUL");
        }
        let ShellInsertionDialect::Posix = policy.shell;
        let quotes = value.bytes().filter(|byte| *byte == b'\'').count();
        let length = value
            .len()
            .checked_add(quotes.saturating_mul(4))
            .and_then(|length| length.checked_add(2 + usize::from(!text.is_empty())))
            .ok_or("file insertion exceeds the size limit")?;
        if length > MAX_FILE_INSERTION_BYTES.saturating_sub(text.len()) {
            return Err("file insertion exceeds the size limit");
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push('\'');
        for ch in value.chars() {
            if ch == '\'' {
                text.push_str("'\"'\"'");
            } else {
                text.push(ch);
            }
        }
        text.push('\'');
    }
    Ok(FileInsertion { text })
}

impl std::fmt::Debug for FileInsertion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileInsertion").finish_non_exhaustive()
    }
}

/// Parses only local absolute file URLs before shell conversion, without filesystem access.
pub(crate) fn parse_file_urls(
    semantics: crate::local_path::LocalPathSemantics,
    urls: &[String],
) -> Result<Vec<PathBuf>, &'static str> {
    if urls.len() > MAX_FILE_ITEMS {
        return Err("too many file URLs");
    }
    let mut bytes = 0usize;
    urls.iter()
        .map(|url| {
            if url.len() > MAX_FILE_INSERTION_BYTES.saturating_sub(bytes) {
                return Err("file URLs exceed the size limit");
            }
            bytes += url.len();
            if !url
                .get(..5)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
            {
                return Err("clipboard URL is not a file URL");
            }
            let mut path = &url[5..];
            if let Some(rest) = path.strip_prefix("//") {
                let slash = rest.find('/').ok_or("file URL has no absolute path")?;
                let (authority, value) = rest.split_at(slash);
                if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
                    return Err("file URL authority is not local");
                }
                path = value;
            }
            if !semantics.absolute_uri_path(path)
                || path.starts_with("//")
                || path.contains(['?', '#'])
            {
                return Err("file URL has no absolute path");
            }
            let mut decoded = Vec::with_capacity(path.len());
            let mut input = path.bytes();
            while let Some(byte) = input.next() {
                let byte = if byte == b'%' {
                    let high = input.next().and_then(|b| (b as char).to_digit(16));
                    let low = input.next().and_then(|b| (b as char).to_digit(16));
                    match (high, low) {
                        (Some(h), Some(l)) => (h * 16 + l) as u8,
                        _ => return Err("file URL encoding is invalid"),
                    }
                } else {
                    byte
                };
                if byte == 0 {
                    return Err("file URL contains NUL");
                }
                decoded.push(byte);
            }
            let path = String::from_utf8(decoded).map_err(|_| "file URL is not valid UTF-8")?;
            let path = semantics
                .decode_uri_path(path)
                .ok_or("file URL authority is not local")?;
            let path = PathBuf::from(path);
            if !semantics.is_absolute(&path) {
                return Err("file URL has no absolute path");
            }
            Ok(path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_preserve_order_and_decode_before_shared_quoting() {
        let paths = parse_file_urls(
            crate::local_path::LocalPathSemantics::Posix,
            &["file:///a%20b%27c%0A".into(), "file://localhost/d".into()],
        )
        .unwrap();
        let insertion = prepare_file_insertion(FileInsertionPolicy::fixture(), &paths).unwrap();
        assert!(insertion.text == "'/a b'\"'\"'c\n' '/d'");
        assert!(
            parse_file_urls(crate::local_path::LocalPathSemantics::Posix, &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_file_urls_and_aggregate_expansion_are_bounded() {
        for value in [
            "https://example.test",
            "file:relative",
            "file:%2Fencoded-root",
            "file://remote/a",
            "file:///a%00b",
            "file:///a%GG",
            "file:///a%FF",
            "file:///%2fa",
            "file:///a?query",
        ] {
            assert!(
                parse_file_urls(
                    crate::local_path::LocalPathSemantics::Posix,
                    &[value.into()]
                )
                .is_err()
            );
        }
        let path = PathBuf::from(format!("/{}", "'".repeat(MAX_FILE_INSERTION_BYTES / 4)));
        assert!(prepare_file_insertion(FileInsertionPolicy::fixture(), &[path]).is_err());
        let path = PathBuf::from(format!("/{}", "x".repeat(MAX_FILE_INSERTION_BYTES - 3)));
        assert_eq!(
            prepare_file_insertion(FileInsertionPolicy::fixture(), &[path])
                .unwrap()
                .text
                .len(),
            MAX_FILE_INSERTION_BYTES
        );
        assert!(
            parse_file_urls(
                crate::local_path::LocalPathSemantics::Posix,
                &[format!("file:///{}", "x".repeat(MAX_FILE_INSERTION_BYTES))]
            )
            .is_err()
        );
    }

    #[test]
    fn hostile_filenames_are_single_quoted_without_losing_unicode_or_newlines() {
        let insertion = prepare_file_insertion(
            FileInsertionPolicy::fixture(),
            &[PathBuf::from("/tmp/a b'c\n😀")],
        )
        .unwrap();
        assert_eq!(insertion.text, "'/tmp/a b'\"'\"'c\n😀'");
    }

    #[test]
    fn multiple_items_preserve_order_with_one_space_separator() {
        let paths = [PathBuf::from("/a"), PathBuf::from("/b c")];
        assert_eq!(
            prepare_file_insertion(FileInsertionPolicy::fixture(), &paths)
                .unwrap()
                .text,
            "'/a' '/b c'"
        );
    }

    #[test]
    fn relative_empty_and_oversized_inputs_are_rejected() {
        assert!(prepare_file_insertion(FileInsertionPolicy::fixture(), &[]).is_err());
        assert!(
            prepare_file_insertion(FileInsertionPolicy::fixture(), &[PathBuf::from("relative")])
                .is_err()
        );
        let many = vec![PathBuf::from("/x"); MAX_FILE_ITEMS + 1];
        assert!(prepare_file_insertion(FileInsertionPolicy::fixture(), &many).is_err());
    }
}
