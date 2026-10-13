//! Authenticate the existing companion's current endpoint holder, not its socket creator.

use spaceterm_host::native_deployment::contracts::NativeFailure;
use std::{
    ffi::{CString, c_void},
    os::fd::AsRawFd,
    os::unix::net::UnixStream,
    ptr,
};

type Cf = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Cf);
    fn CFStringCreateWithCString(allocator: Cf, text: *const libc::c_char, encoding: u32) -> Cf;
    fn CFNumberCreate(allocator: Cf, kind: i32, value: *const c_void) -> Cf;
    fn CFDictionaryCreate(
        allocator: Cf,
        keys: *const Cf,
        values: *const Cf,
        count: isize,
        key_callbacks: Cf,
        value_callbacks: Cf,
    ) -> Cf;
    static kCFTypeDictionaryKeyCallBacks: [usize; 6];
    static kCFTypeDictionaryValueCallBacks: [usize; 5];
}
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecGuestAttributePid: Cf;
    fn SecCodeCopyGuestWithAttributes(host: Cf, attributes: Cf, flags: u32, code: *mut Cf) -> i32;
    fn SecRequirementCreateWithString(text: Cf, flags: u32, requirement: *mut Cf) -> i32;
    fn SecCodeCheckValidity(code: Cf, flags: u32, requirement: Cf) -> i32;
}
unsafe extern "C" {
    fn proc_pidinfo(pid: i32, flavor: i32, argument: u64, buffer: *mut c_void, size: i32) -> i32;
}
struct Owned(Cf);
impl Owned {
    fn new(value: Cf) -> Result<Self, NativeFailure> {
        if value.is_null() {
            Err(NativeFailure::NativeUnavailable)
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: each Owned holds one successfully created CoreFoundation object.
        unsafe { CFRelease(self.0) };
    }
}
#[repr(C)]
#[derive(Default)]
struct Identity {
    uuid: [u8; 16],
    unique: u64,
    parent: u64,
    version: u32,
    parent_version: u32,
    reserved: [u64; 2],
}

fn process_identity(pid: i32) -> Result<(u64, u32), NativeFailure> {
    let mut identity = Identity::default();
    // SAFETY: flavor 17 has this fixed 56-byte native layout, and the writable
    // buffer lives for the call. A partial or unknown response is refused.
    let size = unsafe { proc_pidinfo(pid, 17, 0, (&mut identity as *mut Identity).cast(), 56) };
    if size != 56 || identity.unique == 0 || identity.version == 0 {
        return Err(NativeFailure::ProcessIdentityMismatch);
    }
    Ok((identity.unique, identity.version))
}

fn signed_process(pid: i32, identifier: &str) -> Result<(), NativeFailure> {
    let text = CString::new(format!("identifier \"{identifier}\" and certificate leaf = H\"9e07cd5c15c5c862a21b93ea55d4303bc19b7366\""))
        .map_err(|_| NativeFailure::InvalidRecord)?;
    // SAFETY: all created objects are checked and retained until the dependent
    // call finishes. Callback pointers reference Apple's exported constants.
    unsafe {
        let number = Owned::new(CFNumberCreate(ptr::null(), 3, (&pid as *const i32).cast()))?;
        let key = kSecGuestAttributePid;
        let dictionary = Owned::new(CFDictionaryCreate(
            ptr::null(),
            &key,
            &number.0,
            1,
            ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
            ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
        ))?;
        let string = Owned::new(CFStringCreateWithCString(
            ptr::null(),
            text.as_ptr(),
            0x0800_0100,
        ))?;
        let mut code = ptr::null();
        if SecCodeCopyGuestWithAttributes(ptr::null(), dictionary.0, 0, &mut code) != 0 {
            return Err(NativeFailure::ExecutionBindingUnproven);
        }
        let code = Owned::new(code)?;
        let mut requirement = ptr::null();
        if SecRequirementCreateWithString(string.0, 0, &mut requirement) != 0 {
            return Err(NativeFailure::ArtifactRejected);
        }
        let requirement = Owned::new(requirement)?;
        if SecCodeCheckValidity(code.0, 0, requirement.0) != 0 {
            return Err(NativeFailure::ExecutionBindingUnproven);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct Peer {
    pid: i32,
    unique: u64,
    version: u32,
}
impl Peer {
    pub(super) fn authenticate(stream: &UnixStream, pid: i32) -> Result<Self, NativeFailure> {
        if pid <= 0 || pid == std::process::id() as i32 {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        let (unique, version) = process_identity(pid)?;
        let peer = Self {
            pid,
            unique,
            version,
        };
        peer.recheck(stream)?;
        Ok(peer)
    }
    pub(super) fn recheck(self, stream: &UnixStream) -> Result<(), NativeFailure> {
        let (mut uid, mut gid, mut pid) = (0, 0, 0_i32);
        let mut size = std::mem::size_of::<i32>() as libc::socklen_t;
        // SAFETY: these native calls write only their explicitly sized outputs.
        let valid = unsafe {
            libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0
                && uid == libc::getuid()
                && libc::getsockopt(
                    stream.as_raw_fd(),
                    0,
                    2,
                    (&mut pid as *mut i32).cast(),
                    &mut size,
                ) == 0
                && size as usize == std::mem::size_of::<i32>()
                && pid == self.pid
        };
        if !valid || process_identity(self.pid)? != (self.unique, self.version) {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        signed_process(
            self.pid,
            "io.github.sadiksaifi.spaceterm-development.server.native-evidence.companion",
        )?;
        if process_identity(self.pid)? != (self.unique, self.version) {
            return Err(NativeFailure::ProcessIdentityMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        os::fd::OwnedFd,
        process::{Command, Stdio},
        time::Duration,
    };

    #[test]
    fn inherited_endpoint_writer_with_the_wrong_code_identity_cannot_supply_a_receipt() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        let output: OwnedFd = writer.into();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "printf x; /bin/sleep 1"])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::from(output))
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert!(matches!(
            Peer::authenticate(&reader, child.id() as i32),
            Err(NativeFailure::ExecutionBindingUnproven)
        ));
        child.wait().unwrap();
    }
}
