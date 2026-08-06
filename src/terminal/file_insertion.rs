use std::path::{Path, PathBuf};

pub(crate) const MAX_FILE_ITEMS: usize = 256;
pub(crate) const MAX_FILE_INSERTION_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileInsertionSource {
    Pasteboard,
    Services,
    Drop,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileInsertion {
    pub(crate) source: FileInsertionSource,
    pub(crate) text: String,
}

pub(crate) fn prepare_file_insertion(
    source: FileInsertionSource,
    paths: &[PathBuf],
) -> Result<FileInsertion, &'static str> {
    if paths.is_empty() {
        return Err("no file paths were supplied");
    }
    if paths.len() > MAX_FILE_ITEMS {
        return Err("too many file paths were supplied");
    }
    let mut quoted = Vec::with_capacity(paths.len());
    for path in paths {
        if !path.is_absolute() {
            return Err("file paths must be absolute");
        }
        quoted.push(shell_quote(path));
    }
    let text = quoted.join(" ");
    if text.len() > MAX_FILE_INSERTION_BYTES {
        return Err("file insertion exceeds the size limit");
    }
    Ok(FileInsertion { source, text })
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_filenames_are_single_quoted_without_losing_unicode_or_newlines() {
        let insertion = prepare_file_insertion(
            FileInsertionSource::Pasteboard,
            &[PathBuf::from("/tmp/a b'c\n😀")],
        )
        .unwrap();
        assert_eq!(insertion.text, "'/tmp/a b'\"'\"'c\n😀'");
    }

    #[test]
    fn multiple_items_preserve_order_with_one_space_separator_for_every_source() {
        let paths = [PathBuf::from("/a"), PathBuf::from("/b c")];
        for source in [
            FileInsertionSource::Pasteboard,
            FileInsertionSource::Services,
            FileInsertionSource::Drop,
        ] {
            assert_eq!(
                prepare_file_insertion(source, &paths).unwrap().text,
                "'/a' '/b c'"
            );
        }
    }

    #[test]
    fn relative_empty_and_oversized_inputs_are_rejected() {
        assert!(prepare_file_insertion(FileInsertionSource::Drop, &[]).is_err());
        assert!(
            prepare_file_insertion(FileInsertionSource::Drop, &[PathBuf::from("relative")])
                .is_err()
        );
        let many = vec![PathBuf::from("/x"); MAX_FILE_ITEMS + 1];
        assert!(prepare_file_insertion(FileInsertionSource::Drop, &many).is_err());
    }
}
