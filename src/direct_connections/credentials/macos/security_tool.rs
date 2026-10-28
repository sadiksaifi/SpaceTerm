//! Preflight's credentials in the login Keychain, read and written through `/usr/bin/security`.
//!
//! Preflight is ad hoc signed, so each build has a new code identity. The file-based Keychain
//! binds an item to the cdhash of the build that created it, so every new build would ask for the
//! Keychain password. An item that `security` creates trusts only `security`, which every build
//! can run, so a new build reads it without a prompt. Any process of this account can read it the
//! same way. Release and Development keep their items in-process with [`super::Keychain`].
//!
//! A quiet operation first checks in-process that `security` can use the item without asking.
//! An item an older Preflight build created asks once; "Always Allow" lets `security` use it.

use std::ffi::{CStr, c_char};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use core_foundation::base::OSStatus;
use core_foundation::base::TCFType;
use security_framework::os::macos::keychain::SecKeychain;
use security_framework_sys::base::{SecKeychainRef, errSecSuccess};
use zeroize::Zeroizing;

use super::super::{CredentialError, CredentialStore, Interaction, account, hex};
use super::tool_access::{ToolAccess, tool_access};

const SECURITY: &str = "/usr/bin/security";
// SecKeychainStatus bit for an unlocked Keychain.
const UNLOCKED: u32 = 1;

unsafe extern "C" {
    // These public Security functions are absent from security-framework-sys 2.17.
    fn SecKeychainGetStatus(keychain: SecKeychainRef, status: *mut u32) -> OSStatus;
    fn SecKeychainGetPath(
        keychain: SecKeychainRef,
        length: *mut u32,
        path: *mut c_char,
    ) -> OSStatus;
}

pub(in super::super) struct SecurityTool {
    service: String,
    /// The default Keychain when absent.
    keychain: Option<PathBuf>,
}

impl SecurityTool {
    pub(in super::super) fn new(identity: &str) -> Self {
        Self {
            service: super::service(identity),
            keychain: None,
        }
    }

    #[cfg(all(test, feature = "native-tests"))]
    fn at(identity: &str, keychain: PathBuf) -> Self {
        Self {
            service: super::service(identity),
            keychain: Some(keychain),
        }
    }

    /// The Keychain and its path. A quiet operation refuses a locked Keychain, because `security`
    /// would ask the person to unlock it.
    fn keychain(&self, interaction: Interaction) -> Result<(SecKeychain, String), CredentialError> {
        let keychain = match &self.keychain {
            Some(path) => SecKeychain::open(path),
            None => SecKeychain::default(),
        }
        .map_err(|_| CredentialError::Unavailable)?;
        if interaction == Interaction::Quiet {
            let mut status = 0;
            // SAFETY: The Keychain reference and output status live throughout this call.
            if unsafe { SecKeychainGetStatus(keychain.as_concrete_TypeRef(), &mut status) }
                != errSecSuccess
            {
                return Err(CredentialError::Unavailable);
            }
            if status & UNLOCKED == 0 {
                return Err(CredentialError::InteractionRequired);
            }
        }
        let mut path: [c_char; 1024] = [0; 1024];
        let mut length = path.len() as u32;
        // SAFETY: The buffer holds `length` bytes, and Security writes a terminated path into it.
        if unsafe {
            SecKeychainGetPath(
                keychain.as_concrete_TypeRef(),
                &mut length,
                path.as_mut_ptr(),
            )
        } != errSecSuccess
        {
            return Err(CredentialError::Unavailable);
        }
        // SAFETY: A successful call leaves a NUL-terminated string in the buffer.
        let path = unsafe { CStr::from_ptr(path.as_ptr()) }
            .to_str()
            .map_err(|_| CredentialError::Unavailable)?;
        if !quotable(path) {
            return Err(CredentialError::Unavailable);
        }
        Ok((keychain, path.to_owned()))
    }

    /// Refuses a quiet operation on an item `security` would ask for, such as one an older
    /// Preflight build created. Returns whether the item exists.
    fn quiet(
        &self,
        keychain: &SecKeychain,
        account: &str,
        interaction: Interaction,
    ) -> Result<bool, CredentialError> {
        if interaction == Interaction::Requested {
            return Ok(true);
        }
        match tool_access(keychain, &self.service, account)? {
            ToolAccess::Absent => Ok(false),
            ToolAccess::Quiet => Ok(true),
            ToolAccess::Asks => Err(CredentialError::InteractionRequired),
        }
    }
}

fn run(arguments: &[&str]) -> Result<Zeroizing<Vec<u8>>, Failure> {
    let output = Command::new(SECURITY)
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| Failure::Store(CredentialError::Unavailable))?;
    let stdout = Zeroizing::new(output.stdout);
    exit(output.status.code()).map(|()| stdout)
}

impl CredentialStore for SecurityTool {
    fn read(
        &self,
        key: &[u8; 32],
        interaction: Interaction,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CredentialError> {
        let (store, keychain) = self.keychain(interaction)?;
        let account = account(key);
        if !self.quiet(&store, &account, interaction)? {
            return Ok(None);
        }
        let arguments = [
            "find-generic-password",
            "-s",
            &self.service,
            "-a",
            &account,
            "-w",
            &keychain,
        ];
        match run(&arguments) {
            Ok(output) => decode(&output).map(Some),
            Err(Failure::Absent) => Ok(None),
            Err(Failure::Store(error)) => Err(error),
        }
    }

    fn insert(&self, key: &[u8; 32], secret: &[u8]) -> Result<(), CredentialError> {
        let (_, keychain) = self.keychain(Interaction::Requested)?;
        // Interactive mode reads the command from standard input, so the secret never appears
        // in this process's arguments. Without -U, an existing item is never replaced.
        let mut command = Zeroizing::new(String::new());
        command.push_str("add-generic-password -s ");
        command.push_str(&self.service);
        command.push_str(" -a ");
        command.push_str(&account(key));
        command.push_str(" -w ");
        hex(secret, &mut command);
        command.push_str(" \"");
        command.push_str(&keychain);
        command.push_str("\"\n");
        let mut child = Command::new(SECURITY)
            .arg("-i")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| CredentialError::Unavailable)?;
        let written = child
            .stdin
            .take()
            .map(|mut input| input.write_all(command.as_bytes()));
        let status = child.wait().map_err(|_| CredentialError::Unavailable)?;
        match written {
            Some(Ok(())) => exit(status.code()).map_err(|failure| match failure {
                Failure::Absent => CredentialError::Unavailable,
                // A duplicate is Unavailable: the existing item is never replaced.
                Failure::Store(error) => error,
            }),
            _ => Err(CredentialError::Unavailable),
        }
    }

    fn remove(&self, key: &[u8; 32], interaction: Interaction) -> Result<(), CredentialError> {
        let (store, keychain) = self.keychain(interaction)?;
        let account = account(key);
        if !self.quiet(&store, &account, interaction)? {
            return Ok(());
        }
        let arguments = [
            "delete-generic-password",
            "-s",
            &self.service,
            "-a",
            &account,
            &keychain,
        ];
        match run(&arguments) {
            Ok(_) | Err(Failure::Absent) => Ok(()),
            Err(Failure::Store(error)) => Err(error),
        }
    }
}

/// How one `security` command ended without success.
#[derive(Debug, Eq, PartialEq)]
enum Failure {
    /// errSecItemNotFound. Callers decide what absence means.
    Absent,
    Store(CredentialError),
}

/// `security` exits with the low byte of the failing status.
const fn exit_code(status: OSStatus) -> i32 {
    status & 0xff
}

fn exit(code: Option<i32>) -> Result<(), Failure> {
    const NOT_FOUND: i32 = exit_code(-25300);
    const INTERACTION: [i32; 4] = [
        // Authentication failed, interaction forbidden/required, or the person cancelled.
        exit_code(-25293),
        exit_code(-25308),
        exit_code(-25315),
        exit_code(-128),
    ];
    match code {
        Some(0) => Ok(()),
        Some(NOT_FOUND) => Err(Failure::Absent),
        Some(code) if INTERACTION.contains(&code) => {
            Err(Failure::Store(CredentialError::InteractionRequired))
        }
        _ => Err(Failure::Store(CredentialError::Unavailable)),
    }
}

/// Whether `security -i` reads this path as one double-quoted argument.
fn quotable(path: &str) -> bool {
    !path.is_empty()
        && !path
            .chars()
            .any(|c| c == '"' || c == '\\' || c.is_control())
}

/// The secret is stored as lowercase hexadecimal text, which `security -w` prints unchanged.
fn decode(output: &[u8]) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    let text = output.strip_suffix(b"\n").unwrap_or(output);
    let (pairs, []) = text.as_chunks::<2>() else {
        return Err(CredentialError::Unavailable);
    };
    if pairs.is_empty() {
        return Err(CredentialError::Unavailable);
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut secret = Zeroizing::new(Vec::with_capacity(pairs.len()));
    for [high, low] in pairs {
        let (Some(high), Some(low)) = (digit(*high), digit(*low)) else {
            return Err(CredentialError::Unavailable);
        };
        secret.push((high << 4) | low);
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::{CredentialError, Failure, decode, exit, exit_code, hex, quotable};

    #[test]
    fn security_exit_codes_classify_absence_interaction_and_other_failures() {
        assert_eq!(exit(Some(0)), Ok(()));
        // Observed: a missing item exits 44 and a duplicate 45.
        assert_eq!(exit_code(-25300), 44);
        assert_eq!(exit(Some(44)), Err(Failure::Absent));
        assert_eq!(
            exit(Some(45)),
            Err(Failure::Store(CredentialError::Unavailable))
        );
        assert_eq!(
            exit(Some(exit_code(-128))),
            Err(Failure::Store(CredentialError::InteractionRequired))
        );
        assert_eq!(
            exit(Some(exit_code(-25308))),
            Err(Failure::Store(CredentialError::InteractionRequired))
        );
        assert_eq!(
            exit(None),
            Err(Failure::Store(CredentialError::Unavailable))
        );
    }

    #[test]
    fn secrets_round_trip_through_the_printed_text() {
        let secret = [0, 1, 0x7f, 0x80, 0xfe, 0xff];
        let mut text = String::new();
        hex(&secret, &mut text);
        text.push('\n');
        assert_eq!(decode(text.as_bytes()).unwrap().as_slice(), &secret);
    }

    #[test]
    fn malformed_printed_text_is_unavailable_and_never_partially_decoded() {
        for output in [&b""[..], b"\n", b"abc\n", b"0g\n", b"AB\n", b"00 11\n"] {
            assert_eq!(decode(output), Err(CredentialError::Unavailable));
        }
    }

    #[test]
    fn keychain_paths_that_interactive_mode_would_split_are_refused() {
        assert!(quotable(
            "/Users/someone/Library/Keychains/login.keychain-db"
        ));
        assert!(quotable("/tmp/with space/test.keychain-db"));
        for path in ["", "/tmp/a\"b", "/tmp/a\\b", "/tmp/a\nb"] {
            assert!(!quotable(path));
        }
    }
}

/// Each test uses a throwaway Keychain, so no prompt appears and no user Keychain changes.
#[cfg(all(test, feature = "native-tests"))]
mod native_tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use security_framework::os::macos::keychain::SecKeychain;

    use super::super::super::native_tests::refuses_replacement_and_preserves_other_pairings;
    use super::super::super::{CredentialError, CredentialStore, Interaction, account, hex};
    use super::super::service;
    use super::super::tool_access::{ToolAccess, tool_access};
    use super::SecurityTool;

    const IDENTITY: &str = "spaceterm-preflight";

    fn security(arguments: &[&str]) -> bool {
        Command::new("/usr/bin/security")
            .args(arguments)
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Runs `test` against a new unlocked Keychain, then deletes that Keychain.
    fn throwaway(
        test: impl FnOnce(&Path, &str) -> Result<(), CredentialError>,
    ) -> Result<(), CredentialError> {
        let mut scope = [0; 8];
        getrandom::fill(&mut scope).map_err(|_| CredentialError::Unavailable)?;
        let mut name = String::from("spaceterm-credentials-");
        hex(&scope, &mut name);
        let directory = std::env::temp_dir().join(name);
        std::fs::create_dir(&directory).map_err(|_| CredentialError::Unavailable)?;
        let keychain: PathBuf = directory.join("test.keychain-db");
        let result = match keychain.to_str() {
            Some(path) => {
                let mut password = String::new();
                hex(&scope, &mut password);
                let result = if security(&["create-keychain", "-p", &password, path]) {
                    test(&keychain, path)
                } else {
                    Err(CredentialError::Unavailable)
                };
                let deleted = security(&["delete-keychain", path]);
                result.and(if deleted {
                    Ok(())
                } else {
                    Err(CredentialError::Unavailable)
                })
            }
            None => Err(CredentialError::Unavailable),
        };
        let _ = std::fs::remove_dir_all(&directory);
        result
    }

    #[test]
    fn macos_preflight_store_refuses_replacement_and_never_asks_quietly()
    -> Result<(), CredentialError> {
        throwaway(|keychain, path| {
            let store = SecurityTool::at(IDENTITY, keychain.to_owned());
            refuses_replacement_and_preserves_other_pairings(&store)?;
            let key = [7; 32];
            store.insert(&key, b"locked verification secret")?;
            if !security(&["lock-keychain", path]) {
                return Err(CredentialError::Unavailable);
            }
            // A locked Keychain would make `security` ask; a quiet read refuses first.
            match store.read(&key, Interaction::Quiet) {
                Err(CredentialError::InteractionRequired) => Ok(()),
                _ => Err(CredentialError::Unavailable),
            }
        })
    }

    #[test]
    fn macos_preflight_store_refuses_quietly_an_item_security_would_ask_for()
    -> Result<(), CredentialError> {
        throwaway(|keychain, path| {
            let key = [9; 32];
            let account = account(&key);
            // `-T ""` trusts no application, as an older build's item does not trust `security`.
            if !security(&[
                "add-generic-password",
                "-s",
                &service(IDENTITY),
                "-a",
                &account,
                "-w",
                "00",
                "-T",
                "",
                path,
            ]) {
                return Err(CredentialError::Unavailable);
            }
            let opened = SecKeychain::open(keychain).map_err(|_| CredentialError::Unavailable)?;
            // Checked before any store operation, so a wrong judgment fails here instead of
            // letting `security` ask.
            if tool_access(&opened, &service(IDENTITY), &account)? != ToolAccess::Asks {
                return Err(CredentialError::Unavailable);
            }
            let store = SecurityTool::at(IDENTITY, keychain.to_owned());
            match (
                store.read(&key, Interaction::Quiet),
                store.remove(&key, Interaction::Quiet),
            ) {
                (
                    Err(CredentialError::InteractionRequired),
                    Err(CredentialError::InteractionRequired),
                ) => Ok(()),
                _ => Err(CredentialError::Unavailable),
            }
        })
    }
}
