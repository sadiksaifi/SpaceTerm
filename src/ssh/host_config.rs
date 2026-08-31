use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use super::destination::SshHostAlias;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostConfigSource {
    Managed,
    User,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostConfigProvenance {
    source: HostConfigSource,
    path: PathBuf,
    line: usize,
}

impl HostConfigProvenance {
    pub(crate) const fn source(&self) -> HostConfigSource {
        self.source
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn line(&self) -> usize {
        self.line
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DirectSshTarget {
    user: Option<String>,
    hostname: String,
    port: Option<u16>,
}

impl DirectSshTarget {
    pub(crate) fn subtitle(&self) -> String {
        let mut subtitle = String::new();
        if let Some(user) = &self.user {
            subtitle.push_str(user);
            subtitle.push('@');
        }
        subtitle.push_str(&self.hostname);
        if let Some(port) = self.port {
            subtitle.push(':');
            subtitle.push_str(&port.to_string());
        }
        subtitle
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveredSshHost {
    alias: SshHostAlias,
    provenance: HostConfigProvenance,
    direct_target: Option<DirectSshTarget>,
}

impl DiscoveredSshHost {
    pub(crate) const fn alias(&self) -> &SshHostAlias {
        &self.alias
    }

    pub(crate) const fn provenance(&self) -> &HostConfigProvenance {
        &self.provenance
    }

    pub(crate) const fn direct_target(&self) -> Option<&DirectSshTarget> {
        self.direct_target.as_ref()
    }

    pub(crate) fn subtitle(&self) -> String {
        self.direct_target.as_ref().map_or_else(
            || self.provenance.path.display().to_string(),
            DirectSshTarget::subtitle,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostConfigRoots {
    pub(crate) managed: PathBuf,
    pub(crate) user: PathBuf,
    pub(crate) home: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HostDiscoveryLimits {
    pub(crate) include_depth: usize,
    pub(crate) files: usize,
    pub(crate) total_bytes: usize,
    pub(crate) file_bytes: usize,
    pub(crate) glob_matches: usize,
    pub(crate) results: usize,
    pub(crate) token_bytes: usize,
}

impl Default for HostDiscoveryLimits {
    fn default() -> Self {
        Self {
            include_depth: 8,
            files: 128,
            total_bytes: 1024 * 1024,
            file_bytes: 256 * 1024,
            glob_matches: 256,
            results: 512,
            token_bytes: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostConfigIssueKind {
    Read,
    InvalidUtf8,
    IncludeDepthLimit,
    FileLimit,
    TotalByteLimit,
    FileByteLimit,
    GlobLimit,
    ResultLimit,
    TokenLimit,
    IncludeCycle,
    MalformedLine,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostConfigIssue {
    source: HostConfigSource,
    path: PathBuf,
    line: Option<usize>,
    kind: HostConfigIssueKind,
}

impl HostConfigIssue {
    pub(crate) const fn source(&self) -> HostConfigSource {
        self.source
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn line(&self) -> Option<usize> {
        self.line
    }

    pub(crate) const fn kind(&self) -> HostConfigIssueKind {
        self.kind
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct HostDiscovery {
    pub(crate) hosts: Vec<DiscoveredSshHost>,
    pub(crate) issues: Vec<HostConfigIssue>,
}

pub(crate) trait HostConfigFilesystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;

    fn read_file_limited(&self, path: &Path, maximum_bytes: usize) -> io::Result<Vec<u8>>;

    fn read_directory_limited(
        &self,
        path: &Path,
        maximum_entries: usize,
    ) -> io::Result<Vec<PathBuf>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeHostConfigFilesystem;

impl HostConfigFilesystem for NativeHostConfigFilesystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn read_file_limited(&self, path: &Path, maximum_bytes: usize) -> io::Result<Vec<u8>> {
        let file = File::open(path)?;
        let mut contents = Vec::with_capacity(maximum_bytes.min(16 * 1024));
        file.take(maximum_bytes.saturating_add(1) as u64)
            .read_to_end(&mut contents)?;
        Ok(contents)
    }

    fn read_directory_limited(
        &self,
        path: &Path,
        maximum_entries: usize,
    ) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(path)?
            .take(maximum_entries.saturating_add(1))
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }
}

pub(crate) fn discover_ssh_hosts(
    filesystem: &impl HostConfigFilesystem,
    roots: &HostConfigRoots,
    limits: HostDiscoveryLimits,
) -> HostDiscovery {
    let mut discovery = HostDiscovery::default();
    for (source, root) in [
        (HostConfigSource::Managed, roots.managed.as_path()),
        (HostConfigSource::User, roots.user.as_path()),
    ] {
        let mut scanner = SourceScanner::new(filesystem, roots, limits, source);
        scanner.scan_file(root, 0);
        discovery.hosts.append(&mut scanner.hosts);
        discovery.issues.append(&mut scanner.issues);
    }
    discovery
}

struct SourceScanner<'a, F> {
    filesystem: &'a F,
    roots: &'a HostConfigRoots,
    limits: HostDiscoveryLimits,
    source: HostConfigSource,
    hosts: Vec<DiscoveredSshHost>,
    issues: Vec<HostConfigIssue>,
    visited: BTreeSet<PathBuf>,
    active: BTreeSet<PathBuf>,
    files: usize,
    total_bytes: usize,
    glob_matches: usize,
    result_limit_reported: bool,
}

impl<'a, F: HostConfigFilesystem> SourceScanner<'a, F> {
    fn new(
        filesystem: &'a F,
        roots: &'a HostConfigRoots,
        limits: HostDiscoveryLimits,
        source: HostConfigSource,
    ) -> Self {
        Self {
            filesystem,
            roots,
            limits,
            source,
            hosts: Vec::new(),
            issues: Vec::new(),
            visited: BTreeSet::new(),
            active: BTreeSet::new(),
            files: 0,
            total_bytes: 0,
            glob_matches: 0,
            result_limit_reported: false,
        }
    }

    fn scan_file(&mut self, path: &Path, depth: usize) {
        if is_system_ssh_path(path) {
            return;
        }
        if depth > self.limits.include_depth {
            self.issue(path, None, HostConfigIssueKind::IncludeDepthLimit);
            return;
        }
        let canonical = match self.filesystem.canonicalize(path) {
            Ok(canonical) => canonical,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(_) => {
                self.issue(path, None, HostConfigIssueKind::Read);
                return;
            }
        };
        if is_system_ssh_path(&canonical) {
            return;
        }
        if self.active.contains(&canonical) {
            self.issue(path, None, HostConfigIssueKind::IncludeCycle);
            return;
        }
        if self.visited.contains(&canonical) {
            return;
        }
        if self.files >= self.limits.files {
            self.issue(path, None, HostConfigIssueKind::FileLimit);
            return;
        }
        self.files += 1;
        let contents = match self
            .filesystem
            .read_file_limited(path, self.limits.file_bytes)
        {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(_) => {
                self.issue(path, None, HostConfigIssueKind::Read);
                return;
            }
        };
        if contents.len() > self.limits.file_bytes {
            self.issue(path, None, HostConfigIssueKind::FileByteLimit);
            return;
        }
        if self.total_bytes.saturating_add(contents.len()) > self.limits.total_bytes {
            self.issue(path, None, HostConfigIssueKind::TotalByteLimit);
            return;
        }
        self.total_bytes += contents.len();
        let text = match std::str::from_utf8(&contents) {
            Ok(text) => text,
            Err(_) => {
                self.issue(path, None, HostConfigIssueKind::InvalidUtf8);
                return;
            }
        };
        self.active.insert(canonical.clone());
        self.parse_file(path, text, depth);
        self.active.remove(&canonical);
        self.visited.insert(canonical);
    }

    fn parse_file(&mut self, path: &Path, text: &str, depth: usize) {
        let mut stanza = None;
        let mut global = true;
        let mut match_seen = false;
        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            let mut tokens = match tokenize_config_line(line, self.limits.token_bytes) {
                Ok(tokens) => tokens,
                Err(TokenizeError::TooLong) => {
                    self.issue(path, Some(line_number), HostConfigIssueKind::TokenLimit);
                    continue;
                }
                Err(TokenizeError::Malformed) => {
                    self.issue(path, Some(line_number), HostConfigIssueKind::MalformedLine);
                    continue;
                }
            };
            if tokens.is_empty() {
                continue;
            }
            split_keyword_assignment(&mut tokens);
            let keyword = tokens[0].to_ascii_lowercase();
            match keyword.as_str() {
                "include" if global && !match_seen => {
                    for include in &tokens[1..] {
                        for included_path in self.expand_include(path, include, line_number) {
                            self.scan_file(&included_path, depth.saturating_add(1));
                        }
                    }
                }
                "host" => {
                    self.finish_stanza(stanza.take());
                    global = false;
                    if !match_seen {
                        stanza = Some(ParsedStanza::new(path, line_number, &tokens[1..]));
                    }
                }
                "match" => {
                    self.finish_stanza(stanza.take());
                    global = false;
                    match_seen = true;
                }
                "hostname" | "user" | "port" => {
                    if let Some(stanza) = &mut stanza {
                        stanza.option(&keyword, &tokens[1..]);
                    }
                }
                _ => {}
            }
        }
        self.finish_stanza(stanza);
    }

    fn finish_stanza(&mut self, stanza: Option<ParsedStanza>) {
        let Some(stanza) = stanza else {
            return;
        };
        let direct_target = stanza.direct_target();
        for alias in stanza.aliases {
            if self.hosts.len() >= self.limits.results {
                if !self.result_limit_reported {
                    self.issue(
                        &stanza.path,
                        Some(stanza.line),
                        HostConfigIssueKind::ResultLimit,
                    );
                    self.result_limit_reported = true;
                }
                return;
            }
            self.hosts.push(DiscoveredSshHost {
                alias,
                provenance: HostConfigProvenance {
                    source: self.source,
                    path: stanza.path.clone(),
                    line: stanza.line,
                },
                direct_target: direct_target.clone(),
            });
        }
    }

    fn expand_include(&mut self, including: &Path, token: &str, line: usize) -> Vec<PathBuf> {
        let path = if token == "~" {
            self.roots.home.clone()
        } else if let Some(relative) = token.strip_prefix("~/") {
            self.roots.home.join(relative)
        } else if Path::new(token).is_absolute() {
            PathBuf::from(token)
        } else {
            including
                .parent()
                .unwrap_or_else(|| Path::new("/"))
                .join(token)
        };
        let Some(path) = normalize_absolute_path(&path) else {
            self.issue(including, Some(line), HostConfigIssueKind::MalformedLine);
            return Vec::new();
        };
        let mut expanded = vec![PathBuf::from("/")];
        for component in path.components() {
            let Component::Normal(component) = component else {
                continue;
            };
            let component = component.to_string_lossy();
            if contains_glob(&component) {
                let mut next = Vec::new();
                for directory in &expanded {
                    let entries = match self
                        .filesystem
                        .read_directory_limited(directory, self.limits.glob_matches)
                    {
                        Ok(entries) => entries,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(_) => {
                            self.issue(directory, Some(line), HostConfigIssueKind::Read);
                            continue;
                        }
                    };
                    if entries.len() > self.limits.glob_matches {
                        self.issue(directory, Some(line), HostConfigIssueKind::GlobLimit);
                    }
                    for entry in entries.into_iter().take(self.limits.glob_matches) {
                        let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
                            continue;
                        };
                        if glob_matches(&component, name) {
                            if self.glob_matches >= self.limits.glob_matches {
                                self.issue(directory, Some(line), HostConfigIssueKind::GlobLimit);
                                break;
                            }
                            self.glob_matches += 1;
                            next.push(entry);
                        }
                    }
                }
                expanded = next;
            } else {
                for directory in &mut expanded {
                    directory.push(component.as_ref());
                }
            }
        }
        expanded.sort();
        expanded
    }

    fn issue(&mut self, path: &Path, line: Option<usize>, kind: HostConfigIssueKind) {
        self.issues.push(HostConfigIssue {
            source: self.source,
            path: path.to_path_buf(),
            line,
            kind,
        });
    }
}

struct ParsedStanza {
    path: PathBuf,
    line: usize,
    aliases: Vec<SshHostAlias>,
    ambiguous: bool,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
}

impl ParsedStanza {
    fn new(path: &Path, line: usize, tokens: &[String]) -> Self {
        let mut aliases = Vec::new();
        let mut ambiguous = tokens.is_empty();
        for token in tokens {
            match SshHostAlias::new(token.clone()) {
                Ok(alias) => aliases.push(alias),
                Err(_) => ambiguous = true,
            }
        }
        Self {
            path: path.to_path_buf(),
            line,
            aliases,
            ambiguous,
            hostname: None,
            user: None,
            port: None,
        }
    }

    fn option(&mut self, keyword: &str, values: &[String]) {
        if values.len() != 1 {
            self.ambiguous = true;
            return;
        }
        let value = &values[0];
        match keyword {
            "hostname" if self.hostname.is_none() && valid_hostname(value) => {
                self.hostname = Some(value.clone());
            }
            "user" if self.user.is_none() && valid_user(value) => {
                self.user = Some(value.clone());
            }
            "port" if self.port.is_none() => match value.parse::<u16>() {
                Ok(port) if port != 0 => self.port = Some(port),
                _ => self.ambiguous = true,
            },
            "hostname" | "user" | "port" => self.ambiguous = true,
            _ => {}
        }
    }

    fn direct_target(&self) -> Option<DirectSshTarget> {
        if self.ambiguous {
            return None;
        }
        Some(DirectSshTarget {
            user: self.user.clone(),
            hostname: self.hostname.clone()?,
            port: self.port,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TokenizeError {
    TooLong,
    Malformed,
}

fn tokenize_config_line(line: &str, maximum_bytes: usize) -> Result<Vec<String>, TokenizeError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                current.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '#' {
            break;
        } else if character.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
        if current.len() > maximum_bytes {
            return Err(TokenizeError::TooLong);
        }
    }
    if escaped || quote.is_some() {
        return Err(TokenizeError::Malformed);
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn split_keyword_assignment(tokens: &mut Vec<String>) {
    let Some((keyword, value)) = tokens[0].split_once('=') else {
        return;
    };
    let keyword = keyword.to_owned();
    let value = value.to_owned();
    tokens[0] = keyword;
    if !value.is_empty() {
        tokens.insert(1, value);
    }
}

fn valid_hostname(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | ':')
        })
}

fn valid_user(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | '@')
        })
}

fn normalize_absolute_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

fn contains_glob(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let mut pattern_index = 0;
    let mut value_index = 0;
    let mut star = None;
    let mut star_value = 0;
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            pattern_index += 1;
            star_value = value_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            star_value += 1;
            value_index = star_value;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn is_system_ssh_path(path: &Path) -> bool {
    path.starts_with("/etc/ssh")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct MemoryFilesystem {
        files: BTreeMap<PathBuf, Vec<u8>>,
        canonical: BTreeMap<PathBuf, PathBuf>,
    }

    impl MemoryFilesystem {
        fn file(mut self, path: &str, contents: impl AsRef<[u8]>) -> Self {
            self.files
                .insert(PathBuf::from(path), contents.as_ref().to_vec());
            self
        }

        fn canonical(mut self, path: &str, canonical: &str) -> Self {
            self.canonical
                .insert(PathBuf::from(path), PathBuf::from(canonical));
            self
        }
    }

    impl HostConfigFilesystem for MemoryFilesystem {
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            if let Some(canonical) = self.canonical.get(path) {
                return Ok(canonical.clone());
            }
            if self.files.contains_key(path) {
                return Ok(path.to_path_buf());
            }
            Err(io::Error::new(io::ErrorKind::NotFound, "missing test file"))
        }

        fn read_file_limited(&self, path: &Path, maximum_bytes: usize) -> io::Result<Vec<u8>> {
            let canonical = self.canonicalize(path)?;
            let contents = self
                .files
                .get(path)
                .or_else(|| self.files.get(&canonical))
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "missing test file"))?;
            Ok(contents
                .iter()
                .copied()
                .take(maximum_bytes.saturating_add(1))
                .collect())
        }

        fn read_directory_limited(
            &self,
            path: &Path,
            maximum_entries: usize,
        ) -> io::Result<Vec<PathBuf>> {
            let mut entries = self
                .files
                .keys()
                .filter(|candidate| candidate.parent() == Some(path))
                .take(maximum_entries.saturating_add(1))
                .cloned()
                .collect::<Vec<_>>();
            entries.sort();
            Ok(entries)
        }
    }

    fn roots() -> HostConfigRoots {
        HostConfigRoots {
            managed: PathBuf::from("/managed/ssh_config"),
            user: PathBuf::from("/home/test/.ssh/config"),
            home: PathBuf::from("/home/test"),
        }
    }

    fn aliases(discovery: &HostDiscovery) -> Vec<&str> {
        discovery
            .hosts
            .iter()
            .map(|host| host.alias().as_str())
            .collect()
    }

    #[test]
    fn discovery_should_parse_multiple_quoted_literal_aliases_and_direct_subtitles() {
        let filesystem = MemoryFilesystem::default().file(
            "/managed/ssh_config",
            "Host work \"build\"\n  HostName \"build.example\"\n  User deploy\n  Port 2222\n",
        );

        let discovery = discover_ssh_hosts(&filesystem, &roots(), HostDiscoveryLimits::default());

        assert_eq!(aliases(&discovery), ["work", "build"]);
        assert_eq!(discovery.hosts[0].subtitle(), "deploy@build.example:2222");
        assert_eq!(
            discovery.hosts[0].provenance(),
            &HostConfigProvenance {
                source: HostConfigSource::Managed,
                path: PathBuf::from("/managed/ssh_config"),
                line: 1,
            }
        );
    }

    #[test]
    fn discovery_should_exclude_patterns_negations_malformed_match_and_system_candidates() {
        let filesystem = MemoryFilesystem::default()
            .file(
                "/managed/ssh_config",
                "Include /etc/ssh/ssh_config system-alias\nHost good *.example !blocked bad/path\nHostName good.example\nMatch host *\n  Host match-derived\n",
            )
            .file("/etc/ssh/ssh_config", "Host system\n")
            .file("/managed/system-alias", "Host canonical-system\n")
            .canonical("/managed/system-alias", "/etc/ssh/ssh_config");

        let discovery = discover_ssh_hosts(&filesystem, &roots(), HostDiscoveryLimits::default());

        assert_eq!(aliases(&discovery), ["good"]);
        assert_eq!(discovery.hosts[0].direct_target(), None);
    }

    #[test]
    fn discovery_should_follow_only_global_tilde_absolute_relative_and_glob_includes() {
        let filesystem = MemoryFilesystem::default()
            .file(
                "/managed/ssh_config",
                "Include ~/one.conf /shared/two.conf parts/*.conf\nHost root\n  Include /shared/ignored.conf\n",
            )
            .file("/home/test/one.conf", "Host one\n")
            .file("/shared/two.conf", "Host two\n")
            .file("/managed/parts/a.conf", "Host a\n")
            .file("/managed/parts/b.conf", "Host b\n")
            .file("/shared/ignored.conf", "Host ignored\n");

        let discovery = discover_ssh_hosts(&filesystem, &roots(), HostDiscoveryLimits::default());

        assert_eq!(aliases(&discovery), ["one", "two", "a", "b", "root"]);
    }

    #[test]
    fn discovery_should_detect_cycles_by_canonical_path_and_continue_the_source() {
        let filesystem = MemoryFilesystem::default()
            .file(
                "/managed/ssh_config",
                "Include alias.conf\nHost after-cycle\n",
            )
            .file("/managed/alias.conf", "Include ssh_config\nHost included\n")
            .canonical("/managed/alias.conf", "/canonical/shared")
            .canonical("/managed/ssh_config", "/canonical/root")
            .canonical("/managed/ssh_config", "/canonical/root")
            .canonical("/managed/ssh_config", "/canonical/root");

        let discovery = discover_ssh_hosts(&filesystem, &roots(), HostDiscoveryLimits::default());

        assert_eq!(aliases(&discovery), ["included", "after-cycle"]);
        assert!(discovery.issues.iter().any(|issue| {
            issue.source() == HostConfigSource::Managed
                && issue.kind() == HostConfigIssueKind::IncludeCycle
        }));
    }

    #[test]
    fn discovery_should_report_one_bad_source_without_discarding_the_other_root() {
        let filesystem = MemoryFilesystem::default()
            .file("/managed/ssh_config", [0xff, 0xfe])
            .file("/home/test/.ssh/config", "Host user-host\n");

        let discovery = discover_ssh_hosts(&filesystem, &roots(), HostDiscoveryLimits::default());

        assert_eq!(aliases(&discovery), ["user-host"]);
        assert_eq!(
            discovery.hosts[0].provenance(),
            &HostConfigProvenance {
                source: HostConfigSource::User,
                path: PathBuf::from("/home/test/.ssh/config"),
                line: 1,
            }
        );
        assert_eq!(discovery.issues[0].source(), HostConfigSource::Managed);
        assert_eq!(discovery.issues[0].kind(), HostConfigIssueKind::InvalidUtf8);
    }

    #[test]
    fn discovery_should_report_a_malformed_line_and_keep_valid_entries_from_the_file() {
        let filesystem = MemoryFilesystem::default().file(
            "/managed/ssh_config",
            "Host \"unterminated\nHost valid-after-error\n",
        );

        let discovery = discover_ssh_hosts(&filesystem, &roots(), HostDiscoveryLimits::default());

        assert_eq!(aliases(&discovery), ["valid-after-error"]);
        assert!(discovery.issues.iter().any(|issue| {
            issue.kind() == HostConfigIssueKind::MalformedLine
                && issue.path() == Path::new("/managed/ssh_config")
                && issue.line() == Some(1)
        }));
    }

    #[test]
    fn discovery_should_enforce_include_depth_file_and_total_byte_limits_independently() {
        let filesystem = MemoryFilesystem::default()
            .file(
                "/managed/ssh_config",
                "Include first.conf second.conf\nHost root\n",
            )
            .file("/managed/first.conf", "Include deep.conf\nHost first\n")
            .file("/managed/deep.conf", "Host deep\n")
            .file("/managed/second.conf", "Host second\n");
        let limits = HostDiscoveryLimits {
            include_depth: 1,
            files: 2,
            total_bytes: 80,
            ..HostDiscoveryLimits::default()
        };

        let discovery = discover_ssh_hosts(&filesystem, &roots(), limits);

        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| { issue.kind() == HostConfigIssueKind::IncludeDepthLimit })
        );
        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| issue.kind() == HostConfigIssueKind::FileLimit)
        );

        let total_limited = discover_ssh_hosts(
            &filesystem,
            &roots(),
            HostDiscoveryLimits {
                total_bytes: 8,
                ..HostDiscoveryLimits::default()
            },
        );
        assert!(
            total_limited
                .issues
                .iter()
                .any(|issue| issue.kind() == HostConfigIssueKind::TotalByteLimit)
        );
    }

    #[test]
    fn discovery_should_enforce_per_file_glob_result_and_token_limits() {
        let filesystem = MemoryFilesystem::default()
            .file(
                "/managed/ssh_config",
                "Include parts/*.conf\nHost first second third\n",
            )
            .file("/managed/parts/a.conf", "Host a\n")
            .file("/managed/parts/b.conf", "Host b\n");
        for (limits, kind) in [
            (
                HostDiscoveryLimits {
                    file_bytes: 8,
                    ..HostDiscoveryLimits::default()
                },
                HostConfigIssueKind::FileByteLimit,
            ),
            (
                HostDiscoveryLimits {
                    glob_matches: 1,
                    ..HostDiscoveryLimits::default()
                },
                HostConfigIssueKind::GlobLimit,
            ),
            (
                HostDiscoveryLimits {
                    results: 1,
                    ..HostDiscoveryLimits::default()
                },
                HostConfigIssueKind::ResultLimit,
            ),
            (
                HostDiscoveryLimits {
                    token_bytes: 5,
                    ..HostDiscoveryLimits::default()
                },
                HostConfigIssueKind::TokenLimit,
            ),
        ] {
            let discovery = discover_ssh_hosts(&filesystem, &roots(), limits);
            assert!(
                discovery.issues.iter().any(|issue| issue.kind() == kind),
                "missing {kind:?}"
            );
        }
    }

    #[test]
    fn discovery_should_not_treat_missing_optional_roots_as_issues() {
        let discovery = discover_ssh_hosts(
            &MemoryFilesystem::default(),
            &roots(),
            HostDiscoveryLimits::default(),
        );

        assert_eq!(discovery, HostDiscovery::default());
    }
}
