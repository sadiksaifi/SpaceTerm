use std::collections::BTreeSet;
use std::ffi::{CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::num::NonZeroU16;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use thiserror::Error;

use super::destination::SshHostAlias;
use crate::platform::app_paths::{AppPathRoot, AppPaths, AppPathsError};

const HEADER: &str = "# This file is managed by SpaceTerm.\n\n";
const PRECEDENCE_TAIL: &str = concat!(
    "Host *\n",
    "  Include ~/.ssh/config\n",
    "  Include /etc/ssh/ssh_config\n",
);
const TOKEN_BYTES: usize = 255;
const IDENTITY_FILE_BYTES: usize = 1024;
const MANAGED_CONFIG_BYTES: usize = 1024 * 1024;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const TEMP_CREATION_ATTEMPTS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedSshHostField {
    Alias,
    HostName,
    User,
    IdentityFile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ManagedSshHostValueError {
    Required,
    TooLong { maximum: usize },
    Pattern,
    Negated,
    Whitespace,
    Control,
    LeadingOption,
    ReservedKeyword,
    Unsafe,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid {field:?}: {kind:?}")]
pub(crate) struct ManagedSshHostValidationError {
    pub(crate) field: ManagedSshHostField,
    pub(crate) kind: ManagedSshHostValueError,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedSshHost {
    alias: SshHostAlias,
    host_name: String,
    user: Option<String>,
    port: Option<NonZeroU16>,
    identity_file: Option<String>,
}

impl ManagedSshHost {
    pub(crate) fn new(
        alias: String,
        host_name: String,
        user: Option<String>,
        port: Option<NonZeroU16>,
        identity_file: Option<String>,
    ) -> Result<Self, ManagedSshHostValidationError> {
        validate_alias(&alias)?;
        validate_host_name(&host_name)?;
        if let Some(user) = user.as_deref() {
            validate_user(user)?;
        }
        if let Some(identity_file) = identity_file.as_deref() {
            validate_identity_file(identity_file)?;
        }
        let alias = SshHostAlias::new(alias).map_err(|_| ManagedSshHostValidationError {
            field: ManagedSshHostField::Alias,
            kind: ManagedSshHostValueError::Unsafe,
        })?;
        Ok(Self {
            alias,
            host_name,
            user,
            port,
            identity_file,
        })
    }

    pub(crate) const fn alias(&self) -> &SshHostAlias {
        &self.alias
    }

    pub(crate) fn host_name(&self) -> &str {
        &self.host_name
    }

    pub(crate) fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub(crate) const fn port(&self) -> Option<NonZeroU16> {
        self.port
    }

    pub(crate) fn identity_file(&self) -> Option<&str> {
        self.identity_file.as_deref()
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ManagedHostsFormatError {
    #[error("the managed SSH config is not in SpaceTerm's canonical format")]
    NonCanonical,
}

#[derive(Debug, Error)]
pub(crate) enum ManagedHostsError {
    #[error("SSH alias `{alias}` is already configured")]
    AliasCollision { alias: String },
    #[error("managed SSH alias `{alias}` does not exist")]
    Missing { alias: String },
    #[error("the managed SSH config is not in SpaceTerm's canonical format")]
    NonCanonical,
    #[error(transparent)]
    Paths(#[from] AppPathsError),
    #[error("managed SSH config I/O failed: {0}")]
    Io(#[from] io::Error),
}

pub(crate) trait ManagedHostsFilesystem {
    fn read(&self, directory: &Path, name: &OsStr) -> io::Result<Option<Vec<u8>>>;

    fn atomic_replace(&self, directory: &Path, name: &OsStr, bytes: &[u8]) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NativeManagedHostsFilesystem;

impl ManagedHostsFilesystem for NativeManagedHostsFilesystem {
    fn read(&self, directory: &Path, name: &OsStr) -> io::Result<Option<Vec<u8>>> {
        let directory = match open_private_directory(directory) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let file = match open_file_at(&directory, name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        validate_private_file(&file.metadata()?)?;
        let mut bytes = Vec::new();
        (&file)
            .take(MANAGED_CONFIG_BYTES.saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MANAGED_CONFIG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "managed SSH config exceeds its size limit",
            ));
        }
        Ok(Some(bytes))
    }

    fn atomic_replace(&self, directory: &Path, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MANAGED_CONFIG_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "managed SSH config exceeds its size limit",
            ));
        }
        let directory = open_private_directory(directory)?;
        validate_target_at(&directory, name)?;
        let (temporary_name, mut temporary) = create_temporary_file(&directory, name)?;
        let mut rollback = TemporaryRollback::new(&directory, &temporary_name);
        temporary.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
        validate_private_file(&temporary.metadata()?)?;
        temporary.write_all(bytes)?;
        temporary.sync_all()?;
        validate_target_at(&directory, name)?;
        rename_at(&directory, &temporary_name, name)?;
        rollback.disarm();
        directory.sync_all()?;
        Ok(())
    }
}

pub(crate) struct ManagedHostsStore<'a, F> {
    paths: &'a AppPaths,
    filesystem: &'a F,
}

impl<'a, F: ManagedHostsFilesystem> ManagedHostsStore<'a, F> {
    pub(crate) const fn new(paths: &'a AppPaths, filesystem: &'a F) -> Self {
        Self { paths, filesystem }
    }

    pub(crate) fn load(&self) -> Result<Vec<ManagedSshHost>, ManagedHostsError> {
        let target = self.paths.managed_ssh_config();
        let name = target.file_name().ok_or_else(invalid_managed_path)?;
        let Some(bytes) = self.filesystem.read(self.paths.config(), name)? else {
            return Ok(Vec::new());
        };
        parse_managed_hosts(&bytes).map_err(|_| ManagedHostsError::NonCanonical)
    }

    pub(crate) fn upsert(
        &self,
        host: ManagedSshHost,
        configured_aliases: &[SshHostAlias],
        editing_alias: Option<&SshHostAlias>,
    ) -> Result<(), ManagedHostsError> {
        if configured_aliases
            .iter()
            .any(|configured| configured == host.alias())
            && editing_alias != Some(host.alias())
        {
            return Err(ManagedHostsError::AliasCollision {
                alias: host.alias().as_str().to_owned(),
            });
        }
        let mut hosts = self.load()?;
        if let Some(editing_alias) = editing_alias {
            let position = hosts
                .iter()
                .position(|existing| existing.alias() == editing_alias)
                .ok_or_else(|| ManagedHostsError::Missing {
                    alias: editing_alias.as_str().to_owned(),
                })?;
            if editing_alias != host.alias()
                && hosts
                    .iter()
                    .any(|existing| existing.alias() == host.alias())
            {
                return Err(ManagedHostsError::AliasCollision {
                    alias: host.alias().as_str().to_owned(),
                });
            }
            hosts.remove(position);
        } else if hosts
            .iter()
            .any(|existing| existing.alias() == host.alias())
        {
            return Err(ManagedHostsError::AliasCollision {
                alias: host.alias().as_str().to_owned(),
            });
        }
        hosts.push(host);
        self.write(&hosts)
    }

    pub(crate) fn delete(&self, alias: &SshHostAlias) -> Result<(), ManagedHostsError> {
        let mut hosts = self.load()?;
        let position = hosts
            .iter()
            .position(|host| host.alias() == alias)
            .ok_or_else(|| ManagedHostsError::Missing {
                alias: alias.as_str().to_owned(),
            })?;
        hosts.remove(position);
        self.write(&hosts)
    }

    fn write(&self, hosts: &[ManagedSshHost]) -> Result<(), ManagedHostsError> {
        let directory = self.paths.ensure_root(AppPathRoot::Config)?;
        let target = self.paths.managed_ssh_config();
        let name = target.file_name().ok_or_else(invalid_managed_path)?;
        let bytes = serialize_managed_hosts(hosts);
        if bytes.len() > MANAGED_CONFIG_BYTES {
            return Err(ManagedHostsError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "managed SSH config exceeds its size limit",
            )));
        }
        self.filesystem
            .atomic_replace(directory, name, bytes.as_bytes())?;
        Ok(())
    }
}

fn invalid_managed_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "managed SSH config has no file name",
    )
}

fn open_private_directory(path: &Path) -> io::Result<File> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || metadata.uid() != effective_user_id()
        || metadata.mode() & 0o7777 != PRIVATE_DIRECTORY_MODE
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed SSH config directory is not owner-private",
        ));
    }
    Ok(directory)
}

fn validate_private_file(metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.is_file()
        || metadata.uid() != effective_user_id()
        || metadata.mode() & 0o7777 != PRIVATE_FILE_MODE
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed SSH config file is not owner-private",
        ));
    }
    Ok(())
}

fn validate_target_at(directory: &File, name: &OsStr) -> io::Result<()> {
    let name = component_cstring(name)?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `directory` and `name` remain valid for the call, and metadata points to writable memory.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == -1 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(error);
    }
    // SAFETY: fstatat initialized metadata after returning success.
    let metadata = unsafe { metadata.assume_init() };
    let mode = metadata.st_mode as u32;
    if mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFREG)
        || metadata.st_uid != effective_user_id()
        || mode & 0o7777 != PRIVATE_FILE_MODE
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed SSH config target is unsafe",
        ));
    }
    Ok(())
}

fn create_temporary_file(directory: &File, target: &OsStr) -> io::Result<(CString, File)> {
    for _ in 0..TEMP_CREATION_ATTEMPTS {
        let suffix = random_suffix()?;
        let mut name = target.as_bytes().to_vec();
        name.extend_from_slice(b".");
        name.extend_from_slice(suffix.as_bytes());
        name.extend_from_slice(b".tmp");
        let name = CString::new(name)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid temporary name"))?;
        match open_file_at_cstring(
            directory,
            &name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            PRIVATE_FILE_MODE,
        ) {
            Ok(file) => return Ok((name, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique managed SSH config temporary file",
    ))
}

fn random_suffix() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut suffix = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        suffix.push(HEX[usize::from(byte >> 4)] as char);
        suffix.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    Ok(suffix)
}

fn open_file_at(directory: &File, name: &OsStr, flags: i32, mode: u32) -> io::Result<File> {
    let name = component_cstring(name)?;
    open_file_at_cstring(directory, &name, flags, mode)
}

fn open_file_at_cstring(
    directory: &File,
    name: &CString,
    flags: i32,
    mode: u32,
) -> io::Result<File> {
    // SAFETY: `directory` and `name` remain valid for the call. A successful descriptor is owned below.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn rename_at(directory: &File, source: &CString, target: &OsStr) -> io::Result<()> {
    let target = component_cstring(target)?;
    // SAFETY: both names and the directory descriptor remain valid for the call.
    let result = unsafe {
        libc::renameat(
            directory.as_raw_fd(),
            source.as_ptr(),
            directory.as_raw_fd(),
            target.as_ptr(),
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn component_cstring(name: &OsStr) -> io::Result<CString> {
    if name.is_empty() || name.as_bytes().contains(&b'/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "managed SSH config name is not a path component",
        ));
    }
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path component contains NUL"))
}

fn effective_user_id() -> u32 {
    // SAFETY: geteuid takes no arguments and has no preconditions.
    unsafe { libc::geteuid() }
}

struct TemporaryRollback<'a> {
    directory: &'a File,
    name: &'a CString,
    active: bool,
}

impl<'a> TemporaryRollback<'a> {
    const fn new(directory: &'a File, name: &'a CString) -> Self {
        Self {
            directory,
            name,
            active: true,
        }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TemporaryRollback<'_> {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: the directory and name outlive this rollback guard.
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0);
            }
        }
    }
}

fn validate_alias(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::Alias, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | '@' | ':' | '[' | ']')
    })
}

fn validate_host_name(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::HostName, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | ':' | '[' | ']')
    })
}

fn validate_user(value: &str) -> Result<(), ManagedSshHostValidationError> {
    validate_token(ManagedSshHostField::User, value, |character| {
        character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | '@' | '+')
    })
}

fn validate_token(
    field: ManagedSshHostField,
    value: &str,
    allowed: impl Fn(char) -> bool,
) -> Result<(), ManagedSshHostValidationError> {
    let kind = if value.is_empty() {
        Some(ManagedSshHostValueError::Required)
    } else if value.len() > TOKEN_BYTES {
        Some(ManagedSshHostValueError::TooLong {
            maximum: TOKEN_BYTES,
        })
    } else if value.chars().any(char::is_control) {
        Some(ManagedSshHostValueError::Control)
    } else if value.chars().any(char::is_whitespace) {
        Some(ManagedSshHostValueError::Whitespace)
    } else if value.starts_with('-') {
        Some(ManagedSshHostValueError::LeadingOption)
    } else if value.starts_with('!') {
        Some(ManagedSshHostValueError::Negated)
    } else if value.contains(['*', '?']) {
        Some(ManagedSshHostValueError::Pattern)
    } else if is_reserved_keyword(value) {
        Some(ManagedSshHostValueError::ReservedKeyword)
    } else if !value.chars().all(allowed) {
        Some(ManagedSshHostValueError::Unsafe)
    } else {
        None
    };
    if let Some(kind) = kind {
        Err(ManagedSshHostValidationError { field, kind })
    } else {
        Ok(())
    }
}

fn validate_identity_file(value: &str) -> Result<(), ManagedSshHostValidationError> {
    let field = ManagedSshHostField::IdentityFile;
    let kind = if value.is_empty() {
        Some(ManagedSshHostValueError::Required)
    } else if value.len() > IDENTITY_FILE_BYTES {
        Some(ManagedSshHostValueError::TooLong {
            maximum: IDENTITY_FILE_BYTES,
        })
    } else if value.chars().any(char::is_control) {
        Some(ManagedSshHostValueError::Control)
    } else if value.starts_with('-') {
        Some(ManagedSshHostValueError::LeadingOption)
    } else if value.starts_with('!') {
        Some(ManagedSshHostValueError::Negated)
    } else if value.contains(['*', '?']) {
        Some(ManagedSshHostValueError::Pattern)
    } else if !concrete_identity_path(value) {
        Some(ManagedSshHostValueError::Unsafe)
    } else {
        None
    };
    if let Some(kind) = kind {
        Err(ManagedSshHostValidationError { field, kind })
    } else {
        Ok(())
    }
}

fn concrete_identity_path(value: &str) -> bool {
    let relative = if let Some(relative) = value.strip_prefix("~/") {
        relative
    } else if let Some(relative) = value.strip_prefix('/') {
        relative
    } else {
        return false;
    };
    !relative.is_empty()
        && !relative.ends_with('/')
        && relative
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn is_reserved_keyword(value: &str) -> bool {
    [
        "host",
        "hostname",
        "user",
        "port",
        "identityfile",
        "include",
        "match",
    ]
    .iter()
    .any(|keyword| value.eq_ignore_ascii_case(keyword))
}

fn serialize_managed_hosts(hosts: &[ManagedSshHost]) -> String {
    let mut ordered = hosts.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.alias.cmp(&right.alias));
    let mut serialized = String::from(HEADER);
    for host in ordered {
        serialized.push_str("Host ");
        serialized.push_str(host.alias.as_str());
        serialized.push('\n');
        serialized.push_str("  HostName ");
        serialized.push_str(&host.host_name);
        serialized.push('\n');
        if let Some(user) = &host.user {
            serialized.push_str("  User ");
            serialized.push_str(user);
            serialized.push('\n');
        }
        if let Some(port) = host.port {
            serialized.push_str("  Port ");
            serialized.push_str(&port.to_string());
            serialized.push('\n');
        }
        if let Some(identity_file) = &host.identity_file {
            serialized.push_str("  IdentityFile ");
            quote_argument(identity_file, &mut serialized);
            serialized.push('\n');
        }
        serialized.push('\n');
    }
    serialized.push_str(PRECEDENCE_TAIL);
    serialized
}

fn quote_argument(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        if matches!(character, '\\' | '"') {
            output.push('\\');
        }
        output.push(character);
    }
    output.push('"');
}

fn parse_managed_hosts(bytes: &[u8]) -> Result<Vec<ManagedSshHost>, ManagedHostsFormatError> {
    let text = std::str::from_utf8(bytes).map_err(|_| ManagedHostsFormatError::NonCanonical)?;
    let body = text
        .strip_prefix(HEADER)
        .and_then(|text| text.strip_suffix(PRECEDENCE_TAIL))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let mut hosts = Vec::new();
    let mut aliases = BTreeSet::new();
    if !body.is_empty() {
        let stanzas = body
            .strip_suffix("\n\n")
            .ok_or(ManagedHostsFormatError::NonCanonical)?;
        for stanza in stanzas.split("\n\n") {
            let host = parse_stanza(stanza)?;
            if !aliases.insert(host.alias.as_str().to_owned()) {
                return Err(ManagedHostsFormatError::NonCanonical);
            }
            hosts.push(host);
        }
    }
    if serialize_managed_hosts(&hosts) != text {
        return Err(ManagedHostsFormatError::NonCanonical);
    }
    Ok(hosts)
}

fn parse_stanza(stanza: &str) -> Result<ManagedSshHost, ManagedHostsFormatError> {
    let mut lines = stanza.lines().peekable();
    let alias = lines
        .next()
        .and_then(|line| line.strip_prefix("Host "))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let host_name = lines
        .next()
        .and_then(|line| line.strip_prefix("  HostName "))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let user = take_prefixed(&mut lines, "  User ").map(str::to_owned);
    let port = take_prefixed(&mut lines, "  Port ")
        .map(|value| value.parse::<NonZeroU16>())
        .transpose()
        .map_err(|_| ManagedHostsFormatError::NonCanonical)?;
    let identity_file = take_prefixed(&mut lines, "  IdentityFile ")
        .map(parse_quoted_argument)
        .transpose()?;
    if lines.next().is_some() {
        return Err(ManagedHostsFormatError::NonCanonical);
    }
    ManagedSshHost::new(
        alias.to_owned(),
        host_name.to_owned(),
        user,
        port,
        identity_file,
    )
    .map_err(|_| ManagedHostsFormatError::NonCanonical)
}

fn take_prefixed<'a, I>(lines: &mut std::iter::Peekable<I>, prefix: &str) -> Option<&'a str>
where
    I: Iterator<Item = &'a str>,
{
    lines.peek().and_then(|line| line.strip_prefix(prefix))?;
    lines.next().and_then(|line| line.strip_prefix(prefix))
}

fn parse_quoted_argument(value: &str) -> Result<String, ManagedHostsFormatError> {
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(ManagedHostsFormatError::NonCanonical)?;
    let mut parsed = String::new();
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            let escaped = characters
                .next()
                .filter(|escaped| matches!(escaped, '\\' | '"'))
                .ok_or(ManagedHostsFormatError::NonCanonical)?;
            parsed.push(escaped);
        } else if character == '"' {
            return Err(ManagedHostsFormatError::NonCanonical);
        } else {
            parsed.push(character);
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::num::NonZeroU16;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::platform::app_paths::{AppPathEnvironment, AppPaths};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = PathBuf::from(format!(
                "/private/tmp/spaceterm-managed-hosts-{}-{sequence}",
                std::process::id()
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }

        fn paths(&self) -> AppPaths {
            AppPaths::resolve(&AppPathEnvironment {
                home: None,
                xdg_config_home: Some(self.0.join("config").into_os_string()),
                xdg_data_home: Some(self.0.join("data").into_os_string()),
                xdg_state_home: Some(self.0.join("state").into_os_string()),
                xdg_cache_home: Some(self.0.join("cache").into_os_string()),
                xdg_runtime_dir: Some(self.0.join("runtime").into_os_string()),
                macos_temporary_directory: self.0.join("temporary"),
            })
            .unwrap()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct MemoryFilesystem {
        bytes: RefCell<Option<Vec<u8>>>,
        fail_replace: Cell<bool>,
    }

    impl MemoryFilesystem {
        fn with_bytes(bytes: Vec<u8>) -> Self {
            Self {
                bytes: RefCell::new(Some(bytes)),
                fail_replace: Cell::new(false),
            }
        }
    }

    impl ManagedHostsFilesystem for MemoryFilesystem {
        fn read(
            &self,
            _directory: &Path,
            _name: &std::ffi::OsStr,
        ) -> std::io::Result<Option<Vec<u8>>> {
            Ok(self.bytes.borrow().clone())
        }

        fn atomic_replace(
            &self,
            _directory: &Path,
            _name: &std::ffi::OsStr,
            bytes: &[u8],
        ) -> std::io::Result<()> {
            if self.fail_replace.get() {
                return Err(std::io::Error::other("injected replacement failure"));
            }
            *self.bytes.borrow_mut() = Some(bytes.to_vec());
            Ok(())
        }
    }

    fn host(alias: &str, hostname: &str) -> ManagedSshHost {
        ManagedSshHost::new(alias.to_owned(), hostname.to_owned(), None, None, None).unwrap()
    }

    #[test]
    fn validation_should_report_the_required_field_inline() {
        let error =
            ManagedSshHost::new(String::new(), "server.example".to_owned(), None, None, None)
                .unwrap_err();

        assert_eq!(
            error,
            ManagedSshHostValidationError {
                field: ManagedSshHostField::Alias,
                kind: ManagedSshHostValueError::Required,
            }
        );
    }

    #[test]
    fn validation_should_distinguish_injection_hazards() {
        for (alias, kind) in [
            ("*.example", ManagedSshHostValueError::Pattern),
            ("!blocked", ManagedSshHostValueError::Negated),
            ("two words", ManagedSshHostValueError::Whitespace),
            ("line\nbreak", ManagedSshHostValueError::Control),
            ("-option", ManagedSshHostValueError::LeadingOption),
            ("Host", ManagedSshHostValueError::ReservedKeyword),
            ("bad#alias", ManagedSshHostValueError::Unsafe),
        ] {
            let error = ManagedSshHost::new(
                alias.to_owned(),
                "server.example".to_owned(),
                None,
                None,
                None,
            )
            .unwrap_err();
            assert_eq!(error.kind, kind, "unexpected validation for {alias:?}");
        }
    }

    #[test]
    fn validation_should_accept_a_concrete_identity_path_with_spaces() {
        let managed = ManagedSshHost::new(
            "work".to_owned(),
            "server.example".to_owned(),
            Some("deploy".to_owned()),
            NonZeroU16::new(2222),
            Some("~/Keys/Work Key\"1\"".to_owned()),
        );

        assert!(managed.is_ok(), "unexpected error: {managed:?}");
    }

    #[test]
    fn validation_should_reject_nonconcrete_identity_paths() {
        for identity in ["relative/key", "~/../key", "/keys/*", "/keys/line\nbreak"] {
            let error = ManagedSshHost::new(
                "work".to_owned(),
                "server.example".to_owned(),
                None,
                None,
                Some(identity.to_owned()),
            )
            .unwrap_err();
            assert_eq!(error.field, ManagedSshHostField::IdentityFile);
        }
    }

    #[test]
    fn canonical_format_should_sort_hosts_quote_paths_and_end_with_precedence_tail() {
        let hosts = vec![
            ManagedSshHost::new(
                "zeta".to_owned(),
                "zeta.example".to_owned(),
                None,
                None,
                Some("~/Keys/Zeta Key".to_owned()),
            )
            .unwrap(),
            host("alpha", "alpha.example"),
        ];

        let serialized = serialize_managed_hosts(&hosts);

        assert_eq!(
            serialized,
            concat!(
                "# This file is managed by SpaceTerm.\n\n",
                "Host alpha\n",
                "  HostName alpha.example\n\n",
                "Host zeta\n",
                "  HostName zeta.example\n",
                "  IdentityFile \"~/Keys/Zeta Key\"\n\n",
                "Host *\n",
                "  Include ~/.ssh/config\n",
                "  Include /etc/ssh/ssh_config\n",
            )
        );
    }

    #[test]
    fn canonical_format_should_round_trip_all_five_fields() {
        let expected = ManagedSshHost::new(
            "work".to_owned(),
            "server.example".to_owned(),
            Some("deploy".to_owned()),
            NonZeroU16::new(2222),
            Some("~/Keys/Work Key\"1\"".to_owned()),
        )
        .unwrap();
        let bytes = serialize_managed_hosts(std::slice::from_ref(&expected));

        let parsed = parse_managed_hosts(bytes.as_bytes()).unwrap();

        assert_eq!(parsed, vec![expected]);
    }

    #[test]
    fn canonical_parser_should_reject_unknown_or_noncanonical_text() {
        for bytes in [
            b"Host manual\n  HostName manual.example\n".as_slice(),
            concat!(
                "# This file is managed by SpaceTerm.\n\n",
                "Host work\n",
                "  HostName work.example\n",
                "  ProxyCommand unsafe\n\n",
                "Host *\n",
                "  Include ~/.ssh/config\n",
                "  Include /etc/ssh/ssh_config\n",
            )
            .as_bytes(),
        ] {
            assert_eq!(
                parse_managed_hosts(bytes),
                Err(ManagedHostsFormatError::NonCanonical)
            );
        }
    }

    #[test]
    fn store_should_upsert_and_load_hosts_in_deterministic_order() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let filesystem = MemoryFilesystem::default();
        let store = ManagedHostsStore::new(&paths, &filesystem);
        store
            .upsert(host("zeta", "zeta.example"), &[], None)
            .unwrap();
        store
            .upsert(host("alpha", "alpha.example"), &[], None)
            .unwrap();

        let loaded = store.load().unwrap();

        assert_eq!(
            loaded
                .iter()
                .map(|host| host.alias().as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
    }

    #[test]
    fn store_should_reject_a_configured_alias_collision() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let filesystem = MemoryFilesystem::default();
        let store = ManagedHostsStore::new(&paths, &filesystem);
        let configured = [SshHostAlias::new("work".to_owned()).unwrap()];

        let error = store
            .upsert(host("work", "work.example"), &configured, None)
            .unwrap_err();

        assert!(matches!(
            error,
            ManagedHostsError::AliasCollision { alias } if alias == "work"
        ));
    }

    #[test]
    fn store_should_allow_the_exact_managed_alias_being_edited() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let original = host("work", "old.example");
        let filesystem = MemoryFilesystem::with_bytes(
            serialize_managed_hosts(std::slice::from_ref(&original)).into_bytes(),
        );
        let store = ManagedHostsStore::new(&paths, &filesystem);
        let alias = SshHostAlias::new("work".to_owned()).unwrap();

        store
            .upsert(
                host("work", "new.example"),
                std::slice::from_ref(&alias),
                Some(&alias),
            )
            .unwrap();

        assert_eq!(store.load().unwrap()[0].host_name(), "new.example");
    }

    #[test]
    fn store_should_leave_original_bytes_when_atomic_replace_fails() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let original = serialize_managed_hosts(&[host("work", "old.example")]).into_bytes();
        let filesystem = MemoryFilesystem::with_bytes(original.clone());
        filesystem.fail_replace.set(true);
        let store = ManagedHostsStore::new(&paths, &filesystem);
        let alias = SshHostAlias::new("work".to_owned()).unwrap();

        let error = store.upsert(host("work", "new.example"), &[], Some(&alias));

        assert!(
            error.is_err() && filesystem.bytes.borrow().as_deref() == Some(original.as_slice())
        );
    }

    #[test]
    fn store_should_reject_noncanonical_content_without_rewriting_it() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let original = b"Host manual\n  HostName manual.example\n".to_vec();
        let filesystem = MemoryFilesystem::with_bytes(original.clone());
        let store = ManagedHostsStore::new(&paths, &filesystem);

        let error = store.upsert(host("work", "work.example"), &[], None);

        assert!(
            matches!(error, Err(ManagedHostsError::NonCanonical))
                && filesystem.bytes.borrow().as_deref() == Some(original.as_slice())
        );
    }

    #[test]
    fn store_should_return_a_typed_error_when_deleting_a_missing_alias() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let filesystem = MemoryFilesystem::default();
        let store = ManagedHostsStore::new(&paths, &filesystem);
        let alias = SshHostAlias::new("missing".to_owned()).unwrap();

        let error = store.delete(&alias).unwrap_err();

        assert!(matches!(
            error,
            ManagedHostsError::Missing { alias } if alias == "missing"
        ));
    }

    #[test]
    fn store_should_delete_an_existing_alias_and_keep_the_canonical_tail() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let filesystem = MemoryFilesystem::with_bytes(
            serialize_managed_hosts(&[host("work", "work.example")]).into_bytes(),
        );
        let store = ManagedHostsStore::new(&paths, &filesystem);
        let alias = SshHostAlias::new("work".to_owned()).unwrap();

        store.delete(&alias).unwrap();

        assert_eq!(
            filesystem.bytes.borrow().as_deref(),
            Some(format!("{HEADER}{PRECEDENCE_TAIL}").as_bytes())
        );
    }

    #[test]
    fn native_store_should_create_private_config_and_file_permissions() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let filesystem = NativeManagedHostsFilesystem;
        let store = ManagedHostsStore::new(&paths, &filesystem);

        store
            .upsert(host("work", "work.example"), &[], None)
            .unwrap();

        let config_mode = fs::metadata(paths.config()).unwrap().permissions().mode() & 0o777;
        let file_mode = fs::metadata(paths.managed_ssh_config())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((config_mode, file_mode), (0o700, 0o600));
    }

    #[test]
    fn native_store_should_not_leave_temporary_files_after_success() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        let filesystem = NativeManagedHostsFilesystem;
        let store = ManagedHostsStore::new(&paths, &filesystem);
        store
            .upsert(host("work", "work.example"), &[], None)
            .unwrap();

        let entries = fs::read_dir(paths.config()).unwrap().count();

        assert_eq!(entries, 1);
    }

    #[test]
    fn native_store_should_reject_a_symlink_target_without_following_it() {
        let directory = TestDirectory::new();
        let paths = directory.paths();
        paths.ensure_root(AppPathRoot::Config).unwrap();
        let outside = directory.0.join("outside");
        fs::write(&outside, b"outside bytes").unwrap();
        std::os::unix::fs::symlink(&outside, paths.managed_ssh_config()).unwrap();
        let filesystem = NativeManagedHostsFilesystem;
        let store = ManagedHostsStore::new(&paths, &filesystem);

        let result = store.upsert(host("work", "work.example"), &[], None);

        assert!(result.is_err() && fs::read(outside).unwrap() == b"outside bytes");
    }
}
