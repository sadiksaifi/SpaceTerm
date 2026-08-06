use std::path::Path;

pub(crate) const MAX_LINK_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HyperlinkKind {
    Url,
    #[expect(
        dead_code,
        reason = "local-path detection is configured by the Session host"
    )]
    LocalPath,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HyperlinkTarget {
    pub(crate) identity: u64,
    pub(crate) kind: HyperlinkKind,
    pub(crate) value: String,
}

impl HyperlinkTarget {
    pub(crate) fn url(value: &str) -> Option<Self> {
        valid_text(value)
            .then(|| {
                value
                    .strip_prefix("https://")
                    .or_else(|| value.strip_prefix("http://"))
            })
            .flatten()
            .filter(|rest| !rest.is_empty())?;
        Some(Self::new(HyperlinkKind::Url, value.to_owned()))
    }

    #[expect(
        dead_code,
        reason = "local-path detection is configured by the Session host"
    )]
    pub(crate) fn local(value: &str, trusted_directory: &Path) -> Option<Self> {
        if !valid_text(value) {
            return None;
        }
        let path = Path::new(value);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            trusted_directory.join(path)
        };
        let resolved = resolved.canonicalize().ok()?;
        Some(Self::new(
            HyperlinkKind::LocalPath,
            resolved.to_string_lossy().into_owned(),
        ))
    }

    fn new(kind: HyperlinkKind, value: String) -> Self {
        Self {
            identity: stable_identity(kind, value.as_bytes()),
            kind,
            value,
        }
    }

    pub(crate) fn activation_url(&self) -> String {
        match self.kind {
            HyperlinkKind::Url => self.value.clone(),
            HyperlinkKind::LocalPath => format!("file://{}", self.value),
        }
    }
}

pub(crate) fn detect_url_cells(cells: &[String]) -> Vec<Option<HyperlinkTarget>> {
    let mut result = vec![None; cells.len()];
    let text = cells.concat();
    let mut cell_offsets = Vec::with_capacity(cells.len() + 1);
    let mut offset = 0;
    for cell in cells {
        cell_offsets.push(offset);
        offset += cell.len();
    }
    cell_offsets.push(offset);

    for token in text.split_whitespace() {
        let token = token.trim_end_matches(|character: char| ",.;:!?)]}".contains(character));
        let Some(target) = HyperlinkTarget::url(token) else {
            continue;
        };
        let Some(start) = text.find(token) else {
            continue;
        };
        let end = start + token.len();
        for (index, range) in cell_offsets.windows(2).enumerate() {
            if range[0] < end && range[1] > start {
                result[index] = Some(target.clone());
            }
        }
    }
    result
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_LINK_BYTES && !value.chars().any(char::is_control)
}

const fn stable_identity(kind: HyperlinkKind, bytes: &[u8]) -> u64 {
    let mut hash = match kind {
        HyperlinkKind::Url => 0xcbf29ce484222325,
        HyperlinkKind::LocalPath => 0x84222325cbf29ce4,
    };
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        index += 1;
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_byte_ranges_map_to_complete_cells() {
        let cells = vec![
            "😀".to_owned(),
            " ".to_owned(),
            "https://例.test/a".to_owned(),
        ];
        let links = detect_url_cells(&cells);
        assert!(links[0].is_none());
        assert_eq!(
            links[2].as_ref().map(|link| link.value.as_str()),
            Some("https://例.test/a")
        );
    }

    #[test]
    fn url_and_local_path_validation_are_separate_and_bounded() {
        assert!(HyperlinkTarget::url("https://example.test").is_some());
        assert!(HyperlinkTarget::url("file:///tmp/secret").is_none());
        assert!(HyperlinkTarget::url("https://x\n.invalid").is_none());
        assert!(HyperlinkTarget::url(&format!("https://{}", "x".repeat(MAX_LINK_BYTES))).is_none());

        let temporary = std::env::temp_dir();
        assert!(HyperlinkTarget::local(".", &temporary).is_some());
        assert!(HyperlinkTarget::local("missing-spaceterm-link", &temporary).is_none());
    }

    #[test]
    fn stable_identity_does_not_depend_on_wrapping_or_scrollback_position() {
        let first = HyperlinkTarget::url("https://example.test/path").unwrap();
        let second = HyperlinkTarget::url("https://example.test/path").unwrap();
        assert_eq!(first.identity, second.identity);
    }
}
