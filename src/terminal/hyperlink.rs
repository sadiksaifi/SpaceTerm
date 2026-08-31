use std::fmt;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::metadata::TerminalLocalFileCapabilities;

pub(crate) const MAX_LINK_BYTES: usize = 4096;
const LOCAL_EMISSION_METADATA_PREFIX: &[u8; 8] = b"STLF\0\0\0\x01";
const LOCAL_EMISSION_METADATA_HEADER_BYTES: usize = LOCAL_EMISSION_METADATA_PREFIX.len() + 8 * 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HyperlinkKind {
    Url,
    LocalPath,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct HyperlinkTarget {
    pub(crate) identity: u64,
    pub(crate) kind: HyperlinkKind,
    pub(crate) value: String,
    local_file_identity: Option<LocalFileIdentity>,
}

impl fmt::Debug for HyperlinkTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut target = formatter.debug_struct("HyperlinkTarget");
        target.field("kind", &self.kind);
        match self.kind {
            HyperlinkKind::Url => target
                .field("identity", &self.identity)
                .field("value", &self.value),
            HyperlinkKind::LocalPath => target
                .field("identity", &"<redacted-local-file>")
                .field("value", &"<validated-local-file>"),
        };
        target.finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalFileIdentity {
    device: u64,
    inode: u64,
}

impl From<&Metadata> for LocalFileIdentity {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

impl HyperlinkTarget {
    pub(crate) const fn is_local_file(&self) -> bool {
        matches!(self.kind, HyperlinkKind::LocalPath)
    }

    pub(crate) fn url(value: &str) -> Option<Self> {
        valid_text(value)
            .then(|| {
                value
                    .strip_prefix("https://")
                    .or_else(|| value.strip_prefix("http://"))
            })
            .flatten()
            .filter(|rest| !rest.is_empty())?;
        Some(Self::new(HyperlinkKind::Url, value.to_owned(), None))
    }

    pub(crate) fn osc8(
        value: &str,
        trusted_directory: &Path,
        local_hostname: Option<&str>,
        local_file_capabilities: TerminalLocalFileCapabilities,
    ) -> Option<Self> {
        Self::url(value).or_else(|| {
            local_file_capabilities.are_enabled().then_some(())?;
            let path = parse_local_file_uri(value, local_hostname)?;
            Self::local(&path, trusted_directory)
        })
    }

    fn local(value: &str, trusted_directory: &Path) -> Option<Self> {
        if !valid_text(value) {
            return None;
        }
        let path = Path::new(value);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            if !trusted_directory.is_absolute() {
                return None;
            }
            trusted_directory.join(path)
        };
        let resolved = resolved.canonicalize().ok()?;
        let metadata = resolved.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        let resolved = resolved.to_str()?.to_owned();
        if !valid_text(&resolved) {
            return None;
        }
        Some(Self::new(
            HyperlinkKind::LocalPath,
            resolved,
            Some(LocalFileIdentity::from(&metadata)),
        ))
    }

    fn new(
        kind: HyperlinkKind,
        value: String,
        local_file_identity: Option<LocalFileIdentity>,
    ) -> Self {
        Self {
            identity: stable_identity(kind, value.as_bytes()),
            kind,
            value,
            local_file_identity,
        }
    }

    pub(crate) fn activation_url(
        &self,
        local_file_capabilities: TerminalLocalFileCapabilities,
    ) -> Option<String> {
        match self.kind {
            HyperlinkKind::Url => Some(self.value.clone()),
            HyperlinkKind::LocalPath => {
                local_file_capabilities.are_enabled().then_some(())?;
                // This is intentionally action-triggered filesystem I/O. Hover/render/context
                // eligibility uses the immutable target and never performs synchronous I/O.
                let path = self.revalidated_local_path(local_file_capabilities)?;
                Some(file_url(path.to_str()?))
            }
        }
    }

    /// Returns the canonical file URI immediately after construction. Callers
    /// must not retain this as proof that the filesystem entry is still safe.
    #[cfg(test)]
    pub(crate) fn canonical_file_url(&self) -> Option<String> {
        (self.kind == HyperlinkKind::LocalPath).then(|| file_url(&self.value))
    }

    /// Serializes the validated target into resolver-only metadata retained
    /// separately from terminal text and hyperlink URI formatting.
    pub(crate) fn local_emission_metadata(
        &self,
        local_file_capabilities: TerminalLocalFileCapabilities,
    ) -> Option<Vec<u8>> {
        local_file_capabilities.are_enabled().then_some(())?;
        let identity = self.local_file_identity?;
        (self.kind == HyperlinkKind::LocalPath).then_some(())?;
        let total = LOCAL_EMISSION_METADATA_HEADER_BYTES.checked_add(self.value.len())?;
        if total > MAX_LINK_BYTES {
            return None;
        }
        let mut metadata = Vec::with_capacity(total);
        metadata.extend_from_slice(LOCAL_EMISSION_METADATA_PREFIX);
        metadata.extend_from_slice(&self.identity.to_be_bytes());
        metadata.extend_from_slice(&identity.device.to_be_bytes());
        metadata.extend_from_slice(&identity.inode.to_be_bytes());
        metadata.extend_from_slice(self.value.as_bytes());
        Some(metadata)
    }

    pub(crate) fn from_local_emission_metadata(
        metadata: &[u8],
        local_file_capabilities: TerminalLocalFileCapabilities,
    ) -> Option<Self> {
        local_file_capabilities.are_enabled().then_some(())?;
        if metadata.len() > MAX_LINK_BYTES
            || metadata.len() <= LOCAL_EMISSION_METADATA_HEADER_BYTES
            || !metadata.starts_with(LOCAL_EMISSION_METADATA_PREFIX)
        {
            return None;
        }
        let identity = decode_u64(metadata.get(8..16)?)?;
        let device = decode_u64(metadata.get(16..24)?)?;
        let inode = decode_u64(metadata.get(24..32)?)?;
        let value = std::str::from_utf8(metadata.get(32..)?).ok()?;
        if !valid_text(value)
            || !Path::new(value).is_absolute()
            || stable_identity(HyperlinkKind::LocalPath, value.as_bytes()) != identity
        {
            return None;
        }
        Some(Self {
            identity,
            kind: HyperlinkKind::LocalPath,
            value: value.to_owned(),
            local_file_identity: Some(LocalFileIdentity { device, inode }),
        })
    }

    pub(crate) fn revalidated_local_path(
        &self,
        local_file_capabilities: TerminalLocalFileCapabilities,
    ) -> Option<PathBuf> {
        local_file_capabilities.are_enabled().then_some(())?;
        if self.kind != HyperlinkKind::LocalPath {
            return None;
        }
        let path = Path::new(&self.value).canonicalize().ok()?;
        let metadata = path.metadata().ok()?;
        let canonical = path.to_str()?;
        if !metadata.is_file()
            || canonical != self.value
            || stable_identity(HyperlinkKind::LocalPath, canonical.as_bytes()) != self.identity
            || Some(LocalFileIdentity::from(&metadata)) != self.local_file_identity
        {
            return None;
        }
        // Revalidation rejects stale snapshot targets and replaced filesystem identities. The
        // native open still occurs afterward, so eliminating the final TOCTOU window would require
        // handing an already-open descriptor to the platform API.
        Some(path)
    }
}

pub(crate) fn has_file_scheme(value: &[u8]) -> bool {
    value
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"file:"))
}

fn decode_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

fn parse_local_file_uri(value: &str, local_hostname: Option<&str>) -> Option<String> {
    if !valid_text(value) {
        return None;
    }
    let remainder = value
        .get(..5)?
        .eq_ignore_ascii_case("file:")
        .then(|| &value[5..])?;
    let encoded_path = if let Some(authority_and_path) = remainder.strip_prefix("//") {
        let slash = authority_and_path.find('/')?;
        let (authority, path) = authority_and_path.split_at(slash);
        let authority_is_local = authority.is_empty()
            || authority.eq_ignore_ascii_case("localhost")
            || local_hostname.is_some_and(|hostname| authority.eq_ignore_ascii_case(hostname));
        authority_is_local.then_some(path)?
    } else {
        remainder
    };
    if encoded_path.is_empty()
        || encoded_path.starts_with("//")
        || encoded_path.contains(['?', '#'])
        || !encoded_path.bytes().all(uri_path_byte_is_valid)
    {
        return None;
    }
    let path = percent_decode(encoded_path)?;
    (valid_text(&path) && !path.starts_with("//")).then_some(path)
}

fn uri_path_byte_is_valid(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'@'
                | b'/'
                | b'%'
        )
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        let high = hex_digit(*bytes.get(index + 1)?)?;
        let low = hex_digit(*bytes.get(index + 2)?)?;
        decoded.push(high << 4 | low);
        index += 3;
    }
    String::from_utf8(decoded).ok()
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn file_url(path: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut url = String::with_capacity("file://".len() + path.len());
    url.push_str("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            url.push(char::from(byte));
        } else {
            url.push('%');
            url.push(char::from(HEX[usize::from(byte >> 4)]));
            url.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    url
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
    use std::fs;

    use super::*;

    const LOCAL_FILES: TerminalLocalFileCapabilities = TerminalLocalFileCapabilities::Enabled;

    fn temporary_directory(name: &str) -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("spaceterm-hyperlink-{}-{name}", std::process::id()));
        _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

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
    fn url_validation_remains_web_only_and_bounded() {
        assert!(HyperlinkTarget::url("https://example.test").is_some());
        assert!(HyperlinkTarget::url("file:///tmp/secret").is_none());
        assert!(HyperlinkTarget::url("https://x\n.invalid").is_none());
        assert!(HyperlinkTarget::url(&format!("https://{}", "x".repeat(MAX_LINK_BYTES))).is_none());
    }

    #[test]
    fn osc8_resolves_percent_encoded_relative_file_against_trusted_directory() {
        let directory = temporary_directory("relative");
        let file = directory.join("preview file.txt");
        fs::write(&file, b"preview").unwrap();

        let target =
            HyperlinkTarget::osc8("file:preview%20file.txt", &directory, None, LOCAL_FILES)
                .unwrap();
        let canonical = file.canonicalize().unwrap();
        let canonical = canonical.to_str().unwrap();

        assert_eq!(target.kind, HyperlinkKind::LocalPath);
        assert_eq!(target.value, canonical);
        assert_eq!(
            target.identity,
            stable_identity(HyperlinkKind::LocalPath, canonical.as_bytes())
        );
        assert_eq!(
            target.activation_url(LOCAL_FILES),
            Some(file_url(canonical))
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn osc8_accepts_absolute_files_from_empty_localhost_and_verified_local_authorities() {
        let directory = temporary_directory("authorities");
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        let encoded_path = file.to_str().unwrap();

        for uri in [
            format!("file://{encoded_path}"),
            format!("file://localhost{encoded_path}"),
            format!("file://mac.local{encoded_path}"),
        ] {
            assert!(
                HyperlinkTarget::osc8(&uri, &directory, Some("mac.local"), LOCAL_FILES).is_some()
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn osc8_rejects_remote_malformed_missing_and_non_regular_file_targets() {
        let directory = temporary_directory("rejections");
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();

        let rejected = [
            format!("file://remote.test{}", file.to_str().unwrap()),
            "file:preview%ZZ.txt".to_owned(),
            "file:preview file.txt".to_owned(),
            "file:preview.txt?query".to_owned(),
            "file:////remote.test/share/preview.txt".to_owned(),
            "file:%2F%2Fremote.test/share/preview.txt".to_owned(),
            "file:missing.txt".to_owned(),
            "file:.".to_owned(),
        ];

        assert!(rejected.iter().all(|uri| {
            HyperlinkTarget::osc8(uri, &directory, Some("mac.local"), LOCAL_FILES).is_none()
        }));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn osc8_does_not_treat_arbitrary_detected_text_as_a_local_file() {
        let directory = temporary_directory("not-detected");
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();

        let detected = detect_url_cells(&["preview.txt".to_owned()]);

        assert_eq!(detected, vec![None]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn local_emission_metadata_round_trips_identity_without_filesystem_access() {
        let directory = temporary_directory("emission-metadata");
        let file = directory.join("preview.txt");
        fs::write(&file, b"first").unwrap();
        let target =
            HyperlinkTarget::osc8("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();
        let metadata = target.local_emission_metadata(LOCAL_FILES).unwrap();

        fs::remove_file(&file).unwrap();
        let restored =
            HyperlinkTarget::from_local_emission_metadata(&metadata, LOCAL_FILES).unwrap();

        assert_eq!(restored, target);
        assert_eq!(restored.activation_url(LOCAL_FILES), None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn disabled_local_file_capabilities_suppress_emission_and_restoration() {
        let directory = temporary_directory("disabled-emission");
        let file = directory.join("preview.txt");
        fs::write(&file, b"preview").unwrap();
        let target =
            HyperlinkTarget::osc8("file:preview.txt", &directory, None, LOCAL_FILES).unwrap();
        let metadata = target.local_emission_metadata(LOCAL_FILES).unwrap();

        assert_eq!(
            target.local_emission_metadata(TerminalLocalFileCapabilities::Disabled),
            None
        );
        assert_eq!(
            HyperlinkTarget::from_local_emission_metadata(
                &metadata,
                TerminalLocalFileCapabilities::Disabled,
            ),
            None
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn malformed_local_emission_metadata_is_rejected() {
        assert!(HyperlinkTarget::from_local_emission_metadata(b"forged", LOCAL_FILES).is_none());
    }

    #[test]
    fn local_target_debug_output_redacts_path_and_filesystem_identity() {
        let directory = temporary_directory("debug-redaction");
        let file = directory.join("private-preview.txt");
        fs::write(&file, b"first").unwrap();
        let target =
            HyperlinkTarget::osc8("file:private-preview.txt", &directory, None, LOCAL_FILES)
                .unwrap();

        let diagnostics = format!("{target:?}");

        assert!(!diagnostics.contains("private-preview.txt"));
        assert!(!diagnostics.contains(&target.identity.to_string()));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stable_identity_does_not_depend_on_wrapping_or_scrollback_position() {
        let first = HyperlinkTarget::url("https://example.test/path").unwrap();
        let second = HyperlinkTarget::url("https://example.test/path").unwrap();
        assert_eq!(first.identity, second.identity);
    }
}
