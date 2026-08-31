use std::collections::{BTreeMap, BTreeSet};
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
    provenance: Option<HostConfigProvenance>,
    direct_target: Option<DirectSshTarget>,
}

impl DiscoveredSshHost {
    pub(crate) const fn alias(&self) -> &SshHostAlias {
        &self.alias
    }

    pub(crate) const fn provenance(&self) -> Option<&HostConfigProvenance> {
        self.provenance.as_ref()
    }

    pub(crate) const fn direct_target(&self) -> Option<&DirectSshTarget> {
        self.direct_target.as_ref()
    }

    pub(crate) fn subtitle(&self) -> String {
        self.direct_target.as_ref().map_or_else(
            || {
                self.provenance.as_ref().map_or_else(
                    || "Multiple SSH config sources".to_owned(),
                    |provenance| provenance.path.display().to_string(),
                )
            },
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
        let capacity = maximum_entries.saturating_add(1);
        let mut entries = BTreeSet::new();
        for entry in std::fs::read_dir(path)? {
            entries.insert(entry?.path());
            if entries.len() > capacity {
                entries.pop_last();
            }
        }
        Ok(entries.into_iter().collect())
    }
}

pub(crate) fn discover_ssh_hosts(
    filesystem: &impl HostConfigFilesystem,
    roots: &HostConfigRoots,
    limits: HostDiscoveryLimits,
) -> HostDiscovery {
    let mut discovery = HostDiscovery::default();
    let mut known_aliases = BTreeSet::new();
    for (source, root) in [
        (HostConfigSource::Managed, roots.managed.as_path()),
        (HostConfigSource::User, roots.user.as_path()),
    ] {
        let mut scanner = SourceScanner::new(filesystem, roots, limits, source, &known_aliases);
        scanner.scan_file(root, 0);
        known_aliases.extend(
            scanner
                .hosts
                .iter()
                .map(|host| host.alias.as_str().to_owned()),
        );
        discovery.hosts.append(&mut scanner.hosts);
        discovery.issues.append(&mut scanner.issues);
    }
    discovery.hosts = consolidate_hosts(discovery.hosts);
    discovery
}

fn consolidate_hosts(hosts: Vec<DiscoveredSshHost>) -> Vec<DiscoveredSshHost> {
    let mut indexes: BTreeMap<String, usize> = BTreeMap::new();
    let mut consolidated: Vec<DiscoveredSshHost> = Vec::new();
    for host in hosts {
        if let Some(index) = indexes.get(host.alias.as_str()).copied() {
            let existing = &mut consolidated[index];
            if existing.provenance != host.provenance {
                existing.provenance = None;
            }
            existing.direct_target = None;
        } else {
            indexes.insert(host.alias.as_str().to_owned(), consolidated.len());
            consolidated.push(host);
        }
    }
    consolidated
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
    known_aliases: BTreeSet<String>,
    new_results: usize,
}

impl<'a, F: HostConfigFilesystem> SourceScanner<'a, F> {
    fn new(
        filesystem: &'a F,
        roots: &'a HostConfigRoots,
        limits: HostDiscoveryLimits,
        source: HostConfigSource,
        known_aliases: &BTreeSet<String>,
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
            known_aliases: known_aliases.clone(),
            new_results: 0,
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
        let mut section = ConfigSection::Global;
        for (index, line) in text.lines().enumerate() {
            let line_number = index + 1;
            let (keyword, tokens) = match parse_config_directive(line, self.limits.token_bytes) {
                Ok(Some(directive)) => directive,
                Ok(None) => continue,
                Err(TokenizeError::TooLong) => {
                    self.issue(path, Some(line_number), HostConfigIssueKind::TokenLimit);
                    continue;
                }
                Err(TokenizeError::Malformed) => {
                    self.issue(path, Some(line_number), HostConfigIssueKind::MalformedLine);
                    continue;
                }
            };
            match keyword.as_str() {
                "include" if section == ConfigSection::Global => {
                    for include in &tokens {
                        for included_path in self.expand_include(path, include, line_number) {
                            self.scan_file(&included_path, depth.saturating_add(1));
                        }
                    }
                }
                "host" => {
                    self.finish_stanza(stanza.take());
                    section = ConfigSection::Host;
                    stanza = Some(ParsedStanza::new(path, line_number, &tokens));
                }
                "match" => {
                    self.finish_stanza(stanza.take());
                    section = ConfigSection::Match;
                }
                "hostname" | "user" | "port" => {
                    if let Some(stanza) = &mut stanza {
                        stanza.option(&keyword, &tokens);
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
            let is_new = !self.known_aliases.contains(alias.as_str());
            if is_new && self.new_results >= self.limits.results {
                if !self.result_limit_reported {
                    self.issue(
                        &stanza.path,
                        Some(stanza.line),
                        HostConfigIssueKind::ResultLimit,
                    );
                    self.result_limit_reported = true;
                }
                continue;
            }
            if is_new {
                self.known_aliases.insert(alias.as_str().to_owned());
                self.new_results += 1;
            }
            self.hosts.push(DiscoveredSshHost {
                alias,
                provenance: Some(HostConfigProvenance {
                    source: self.source,
                    path: stanza.path.clone(),
                    line: stanza.line,
                }),
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
            self.roots.home.join(".ssh").join(token)
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
                let component = unescape_glob_literal(&component);
                for directory in &mut expanded {
                    directory.push(&component);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigSection {
    Global,
    Host,
    Match,
}

fn parse_config_directive(
    line: &str,
    maximum_bytes: usize,
) -> Result<Option<(String, Vec<String>)>, TokenizeError> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let keyword_end = line
        .find(|character: char| character.is_whitespace() || character == '=')
        .unwrap_or(line.len());
    let keyword = &line[..keyword_end];
    if keyword.is_empty() || keyword.len() > maximum_bytes || keyword.contains(['\'', '"', '\\']) {
        return Err(TokenizeError::Malformed);
    }
    let mut arguments = line[keyword_end..].trim_start();
    if let Some(after_equals) = arguments.strip_prefix('=') {
        arguments = after_equals.trim_start();
    }
    let tokens = tokenize_config_arguments(arguments, maximum_bytes)?;
    Ok(Some((keyword.to_ascii_lowercase(), tokens)))
}

fn tokenize_config_arguments(
    line: &str,
    maximum_bytes: usize,
) -> Result<Vec<String>, TokenizeError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut token_started = false;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' {
            let Some(next) = characters.next() else {
                return Err(TokenizeError::Malformed);
            };
            token_started = true;
            if next.is_whitespace() || matches!(next, '\\' | '\'' | '"' | '#') {
                current.push(next);
            } else {
                current.push('\\');
                current.push(next);
            }
        } else if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                current.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
            token_started = true;
        } else if character == '#' && !token_started {
            break;
        } else if character.is_whitespace() {
            if token_started {
                tokens.push(std::mem::take(&mut current));
                token_started = false;
            }
        } else {
            current.push(character);
            token_started = true;
        }
        if current.len() > maximum_bytes {
            return Err(TokenizeError::TooLong);
        }
    }
    if quote.is_some() {
        return Err(TokenizeError::Malformed);
    }
    if token_started {
        tokens.push(current);
    }
    Ok(tokens)
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
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if matches!(character, '*' | '?' | '[') {
            return true;
        }
    }
    false
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    if value.starts_with('.') && explicit_first_character(pattern) != Some('.') {
        return false;
    }
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut memo = BTreeMap::new();
    glob_matches_from(&pattern, &value, 0, 0, &mut memo)
}

fn glob_matches_from(
    pattern: &[char],
    value: &[char],
    pattern_index: usize,
    value_index: usize,
    memo: &mut BTreeMap<(usize, usize), bool>,
) -> bool {
    if let Some(result) = memo.get(&(pattern_index, value_index)) {
        return *result;
    }
    let result = if pattern_index == pattern.len() {
        value_index == value.len()
    } else {
        match pattern[pattern_index] {
            '*' => (value_index..=value.len())
                .any(|next| glob_matches_from(pattern, value, pattern_index + 1, next, memo)),
            '?' => {
                value_index < value.len()
                    && glob_matches_from(pattern, value, pattern_index + 1, value_index + 1, memo)
            }
            '\\' if pattern_index + 1 < pattern.len() => {
                value.get(value_index) == pattern.get(pattern_index + 1)
                    && glob_matches_from(pattern, value, pattern_index + 2, value_index + 1, memo)
            }
            '[' => match bracket_class(pattern, pattern_index, value.get(value_index).copied()) {
                Some((matched, next_pattern)) => {
                    matched
                        && glob_matches_from(pattern, value, next_pattern, value_index + 1, memo)
                }
                None => {
                    value.get(value_index) == Some(&'[')
                        && glob_matches_from(
                            pattern,
                            value,
                            pattern_index + 1,
                            value_index + 1,
                            memo,
                        )
                }
            },
            literal => {
                value.get(value_index) == Some(&literal)
                    && glob_matches_from(pattern, value, pattern_index + 1, value_index + 1, memo)
            }
        }
    };
    memo.insert((pattern_index, value_index), result);
    result
}

fn bracket_class(pattern: &[char], start: usize, value: Option<char>) -> Option<(bool, usize)> {
    let value = value?;
    let mut index = start + 1;
    let negated = matches!(pattern.get(index), Some('!' | '^'));
    if negated {
        index += 1;
    }
    let mut matched = false;
    let mut populated = false;
    while index < pattern.len() && pattern[index] != ']' {
        let (first, after_first) = class_character(pattern, index)?;
        populated = true;
        if pattern.get(after_first) == Some(&'-')
            && pattern
                .get(after_first + 1)
                .is_some_and(|character| *character != ']')
        {
            let (last, after_last) = class_character(pattern, after_first + 1)?;
            matched |= first <= value && value <= last;
            index = after_last;
        } else {
            matched |= first == value;
            index = after_first;
        }
    }
    if !populated || pattern.get(index) != Some(&']') {
        return None;
    }
    Some((if negated { !matched } else { matched }, index + 1))
}

fn class_character(pattern: &[char], index: usize) -> Option<(char, usize)> {
    match pattern.get(index).copied()? {
        '\\' => Some((*pattern.get(index + 1)?, index + 2)),
        character => Some((character, index + 1)),
    }
}

fn explicit_first_character(pattern: &str) -> Option<char> {
    let characters = pattern.chars().collect::<Vec<_>>();
    match *characters.first()? {
        '\\' => characters.get(1).copied(),
        '[' => {
            bracket_class(&characters, 0, Some('.')).and_then(|(matched, _)| matched.then_some('.'))
        }
        character if matches!(character, '*' | '?') => None,
        character => Some(character),
    }
}

fn unescape_glob_literal(value: &str) -> String {
    let mut result = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            if let Some(escaped) = characters.next() {
                result.push(escaped);
            } else {
                result.push('\\');
            }
        } else {
            result.push(character);
        }
    }
    result
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
                .cloned()
                .collect::<Vec<_>>();
            entries.sort();
            entries.truncate(maximum_entries.saturating_add(1));
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
            Some(&HostConfigProvenance {
                source: HostConfigSource::Managed,
                path: PathBuf::from("/managed/ssh_config"),
                line: 1,
            })
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
            .file(
                "/home/test/.ssh/system-alias",
                "Host canonical-system\n",
            )
            .canonical("/home/test/.ssh/system-alias", "/etc/ssh/ssh_config");

        let discovery = discover_ssh_hosts(&filesystem, &roots(), HostDiscoveryLimits::default());

        assert_eq!(aliases(&discovery), ["good", "bad/path", "match-derived"]);
        assert_eq!(discovery.hosts[0].direct_target(), None);
    }

    #[test]
    fn directive_parser_should_follow_openssh_keyword_comment_quote_and_escape_rules() {
        assert_eq!(
            parse_config_directive(
                "Host = foo#literal \"\" escaped\\ value literal\\* # comment",
                128,
            ),
            Ok(Some((
                "host".to_owned(),
                vec![
                    "foo#literal".to_owned(),
                    String::new(),
                    "escaped value".to_owned(),
                    "literal\\*".to_owned(),
                ],
            )))
        );
        assert_eq!(
            parse_config_directive("\"Host\" invalid", 128),
            Err(TokenizeError::Malformed)
        );
    }

    #[test]
    fn discovery_should_allow_a_host_section_after_match_without_enabling_its_include() {
        let filesystem = MemoryFilesystem::default()
            .file(
                "/managed/ssh_config",
                "Match host old\n  Include ignored.conf\nHost later\n",
            )
            .file("/home/test/.ssh/ignored.conf", "Host ignored\n");

        let discovery = discover_ssh_hosts(&filesystem, &roots(), HostDiscoveryLimits::default());

        assert_eq!(aliases(&discovery), ["later"]);
    }

    #[test]
    fn discovery_should_consolidate_duplicates_without_spending_new_result_budget() {
        let filesystem = MemoryFilesystem::default()
            .file(
                "/managed/ssh_config",
                "Host duplicate\n  HostName managed.example\n",
            )
            .file(
                "/home/test/.ssh/config",
                "Host duplicate\n  HostName user.example\nHost user-only\n",
            );
        let limits = HostDiscoveryLimits {
            results: 1,
            ..HostDiscoveryLimits::default()
        };

        let discovery = discover_ssh_hosts(&filesystem, &roots(), limits);

        assert_eq!(aliases(&discovery), ["duplicate", "user-only"]);
        assert_eq!(discovery.hosts[0].provenance(), None);
        assert_eq!(discovery.hosts[0].direct_target(), None);
    }

    #[test]
    fn lexical_glob_should_support_classes_escaping_and_dotfile_rules() {
        assert!(glob_matches("[ab][0-9].conf", "a7.conf"));
        assert!(glob_matches("[!a].conf", "b.conf"));
        assert!(glob_matches(r"[\]].conf", "].conf"));
        assert!(glob_matches(r"literal\*.conf", "literal*.conf"));
        assert!(!glob_matches("*.conf", ".hidden.conf"));
        assert!(glob_matches(".*.conf", ".hidden.conf"));
        assert!(glob_matches("[.]hidden.conf", ".hidden.conf"));
    }

    #[test]
    fn glob_expansion_should_choose_lexically_first_entries_before_bounding() {
        let filesystem = MemoryFilesystem::default()
            .file("/managed/ssh_config", "Include conf/*.conf\n")
            .file("/home/test/.ssh/conf/z.conf", "Host z\n")
            .file("/home/test/.ssh/conf/a.conf", "Host a\n")
            .file("/home/test/.ssh/conf/b.conf", "Host b\n");
        let limits = HostDiscoveryLimits {
            glob_matches: 2,
            ..HostDiscoveryLimits::default()
        };

        let discovery = discover_ssh_hosts(&filesystem, &roots(), limits);

        assert_eq!(aliases(&discovery), ["a", "b"]);
        assert!(
            discovery
                .issues
                .iter()
                .any(|issue| issue.kind() == HostConfigIssueKind::GlobLimit)
        );
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
            .file("/home/test/.ssh/parts/a.conf", "Host a\n")
            .file("/home/test/.ssh/parts/b.conf", "Host b\n")
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
            .file(
                "/home/test/.ssh/alias.conf",
                "Include alias.conf\nHost included\n",
            )
            .canonical("/home/test/.ssh/alias.conf", "/canonical/shared")
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
            Some(&HostConfigProvenance {
                source: HostConfigSource::User,
                path: PathBuf::from("/home/test/.ssh/config"),
                line: 1,
            })
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
            .file(
                "/home/test/.ssh/first.conf",
                "Include deep.conf\nHost first\n",
            )
            .file("/home/test/.ssh/deep.conf", "Host deep\n")
            .file("/home/test/.ssh/second.conf", "Host second\n");
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
            .file("/home/test/.ssh/parts/a.conf", "Host a\n")
            .file("/home/test/.ssh/parts/b.conf", "Host b\n");
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
