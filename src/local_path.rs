//! Explicit local path spelling policy, independent of the running host.
use std::path::Path;

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
    pub(crate) fn is_absolute(self, path: &Path) -> bool {
        match self {
            Self::Posix => path.as_os_str().as_encoded_bytes().starts_with(b"/"),
        }
    }
    #[cfg_attr(
        not(target_os = "macos"),
        allow(dead_code, reason = "only native file URL intake checks URI paths")
    )]
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
