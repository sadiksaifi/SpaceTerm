//! Explicit local path spelling policy, independent of the running host.
use std::path::{Path, PathBuf};

/// Composition selects the path dialect. No other host implementation is implied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalPathSemantics {
    Posix,
}

impl LocalPathSemantics {
    pub(crate) const fn separator(self) -> char {
        match self {
            Self::Posix => '/',
        }
    }
    pub(crate) const fn home_prefix(self) -> &'static str {
        match self {
            Self::Posix => "~/",
        }
    }
    pub(crate) fn is_absolute(self, path: &Path) -> bool {
        match self {
            Self::Posix => path.as_os_str().as_encoded_bytes().starts_with(b"/"),
        }
    }
    pub(crate) fn expand_home(self, value: &str, home: &Path) -> PathBuf {
        match value.strip_prefix(self.home_prefix()) {
            Some("") => home.to_owned(),
            Some(rest) => home.join(rest.trim_start_matches(self.separator())),
            None => value.into(),
        }
    }
    pub(crate) fn absolute_uri_path(self, encoded: &str) -> bool {
        match self {
            Self::Posix => encoded.starts_with('/'),
        }
    }

    /// URI slashes belong to the file protocol; conversion to local spelling is dialect policy.
    pub(crate) fn decode_uri_path(self, decoded: String) -> Option<String> {
        match self {
            Self::Posix => (!decoded.starts_with("//")).then_some(decoded),
        }
    }
    /// OSC 7 retains repeated separators, matching reported directory spelling.
    pub(crate) fn decode_directory_uri_path(self, decoded: String) -> Option<String> {
        self.is_absolute(Path::new(&decoded)).then_some(decoded)
    }

    pub(crate) fn directory_basename(self, path: &str) -> Option<String> {
        path.trim_end_matches(self.separator())
            .rsplit(self.separator())
            .next()
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    pub(crate) fn file_url(self, path: &str) -> String {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut url = String::from("file://");
        for byte in path.bytes() {
            if byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~')
                || byte == self.separator() as u8
            {
                url.push(char::from(byte));
            } else {
                url.push('%');
                url.push(char::from(HEX[usize::from(byte >> 4)]));
                url.push(char::from(HEX[usize::from(byte & 0x0f)]));
            }
        }
        url
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryPathFormatError {
    Relative,
    BareTilde,
    UnsupportedTilde,
}

impl DirectoryPathFormatError {
    pub(crate) const fn message(self, semantics: LocalPathSemantics) -> &'static str {
        match semantics {
            LocalPathSemantics::Posix => match self {
                Self::Relative => "Enter an absolute path beginning with / or ~/.",
                Self::BareTilde => "Use ~/ to open your home directory.",
                Self::UnsupportedTilde => "Only ~/ is supported for home-relative paths.",
            },
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ParsedDirectoryPath {
    display: String,
    exact_path: PathBuf,
    enumeration_directory: PathBuf,
    pub(crate) leaf_filter: String,
    trailing_separator: bool,
}

impl std::fmt::Debug for ParsedDirectoryPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ParsedDirectoryPath(<redacted>)")
    }
}

impl ParsedDirectoryPath {
    pub(crate) fn display(&self) -> &str {
        &self.display
    }

    pub(crate) fn exact_path(&self) -> &Path {
        &self.exact_path
    }

    pub(crate) fn enumeration_directory(&self) -> &Path {
        &self.enumeration_directory
    }

    #[cfg(test)]
    pub(crate) fn leaf_filter(&self) -> &str {
        &self.leaf_filter
    }

    #[cfg(test)]
    pub(crate) const fn trailing_separator(&self) -> bool {
        self.trailing_separator
    }

    pub(crate) fn reveals_dot_directories(&self) -> bool {
        self.leaf_filter.starts_with('.')
    }
}

pub(crate) fn parse_directory_path(
    semantics: LocalPathSemantics,
    input: &str,
    home: &Path,
) -> Result<ParsedDirectoryPath, DirectoryPathFormatError> {
    if input == "~" {
        return Err(DirectoryPathFormatError::BareTilde);
    }
    if input.starts_with('~') && !input.starts_with(semantics.home_prefix()) {
        return Err(DirectoryPathFormatError::UnsupportedTilde);
    }
    if !semantics.is_absolute(Path::new(input)) && !input.starts_with(semantics.home_prefix()) {
        return Err(DirectoryPathFormatError::Relative);
    }

    let exact_path = semantics.expand_home(input, home);
    let trailing_separator = input.ends_with(semantics.separator());
    let (enumeration_directory, leaf_filter) = if trailing_separator {
        (exact_path.clone(), String::new())
    } else {
        let separator = input
            .rfind(semantics.separator())
            .ok_or(DirectoryPathFormatError::Relative)?;
        let display_directory = &input[..=separator];
        (
            semantics.expand_home(display_directory, home),
            input[separator + 1..].to_owned(),
        )
    };

    Ok(ParsedDirectoryPath {
        display: input.to_owned(),
        exact_path,
        enumeration_directory,
        leaf_filter,
        trailing_separator,
    })
}

pub(crate) fn display_directory_with_style(
    semantics: LocalPathSemantics,
    path: &Path,
    home: &Path,
    prefer_tilde: bool,
) -> Option<String> {
    if prefer_tilde && path == home {
        return Some(semantics.home_prefix().to_owned());
    }
    if prefer_tilde && let Ok(relative) = path.strip_prefix(home) {
        let relative = relative.to_str()?;
        return Some(format!(
            "{}{relative}{}",
            semantics.home_prefix(),
            semantics.separator()
        ));
    }
    let path = path.to_str()?;
    Some(format!(
        "{}{}",
        path.trim_end_matches(semantics.separator()),
        semantics.separator()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_posix_entry_preserves_spelling_and_selects_one_level() {
        let semantics = LocalPathSemantics::Posix;
        let home = Path::new("/fixture/home");
        for (input, exact, directory, leaf) in [
            ("/", "/", "/", ""),
            ("~/", "/fixture/home", "/fixture/home", ""),
            (
                "~//project",
                "/fixture/home/project",
                "/fixture/home/",
                "project",
            ),
            (
                "~/link/../project",
                "/fixture/home/link/../project",
                "/fixture/home/link/../",
                "project",
            ),
            ("/project/", "/project/", "/project/", ""),
            ("/project x/界", "/project x/界", "/project x/", "界"),
        ] {
            let parsed = parse_directory_path(semantics, input, home).unwrap();
            assert_eq!(
                (
                    parsed.display(),
                    parsed.exact_path().as_os_str(),
                    parsed.enumeration_directory().as_os_str(),
                    parsed.leaf_filter()
                ),
                (
                    input,
                    std::ffi::OsStr::new(exact),
                    std::ffi::OsStr::new(directory),
                    leaf
                )
            );
        }
        for input in ["relative", "~", "~someone/project"] {
            assert!(parse_directory_path(semantics, input, home).is_err());
        }
    }

    #[test]
    fn home_display_requires_a_complete_component_and_retains_absolute_style() {
        let semantics = LocalPathSemantics::Posix;
        let home = Path::new("/fixture/home");
        for (path, tilde, expected) in [
            ("/fixture/home", true, "~/"),
            ("/fixture/home/project", true, "~/project/"),
            ("/fixture/home-other", true, "/fixture/home-other/"),
            ("/fixture/home/project", false, "/fixture/home/project/"),
            ("/", true, "/"),
        ] {
            assert_eq!(
                display_directory_with_style(semantics, Path::new(path), home, tilde).as_deref(),
                Some(expected)
            );
        }
    }
}
