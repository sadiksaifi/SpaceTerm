//! Fixed descriptor-exec of the retained patched systemd-creds closure.
//! The release issuer, not this parser, authenticates and protects that closure.

use std::{
    ffi::CString,
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::fs::FileExt,
    },
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    boot_credentials::{
        CredentialReleasePermit, LockedSecret32,
        linux::{RetainedCredentialExecution, credential_name, recognize_credential_tool_output},
    },
    native_deployment::{contracts::NativeFailure, profile::LinuxCredentialMode},
    startup_exchange::{
        ExchangeDeadline,
        unix::{ExactChild, PipeEndpoint, anonymous_pipe, process_identity},
    },
};

const CIPHERTEXT_LIMIT: usize = 65_536;
const HOST: [u8; 16] = [
    0x5a, 0x1c, 0x6a, 0x86, 0xdf, 0x9d, 0x40, 0x96, 0xb1, 0xd5, 0xa6, 0x5e, 0x08, 0x62, 0xf1, 0x9a,
];
const HOST_TPM2: [u8; 16] = [
    0x93, 0xa8, 0x94, 0x09, 0x48, 0x74, 0x44, 0x90, 0x90, 0xca, 0xf2, 0xfc, 0x93, 0xca, 0xb5, 0x53,
];

/// Checks the unencrypted v260.5 recipe before privileged tool execution. The
/// authenticated tool still verifies AEAD, embedded name, timestamps and TPM.
/// Recipe IDs and layout are from creds-util.{h,c} at the frozen source commit
/// 84ff4f0dff41c26472cd48c4c4955c2d77f5ac08, not a --with-key decrypt flag.
pub fn inspect_recipe(decoded: &[u8], mode: LinuxCredentialMode) -> Result<(), NativeFailure> {
    let reject = NativeFailure::CredentialPolicyRejected;
    let expected = match mode {
        LinuxCredentialMode::Host => HOST,
        LinuxCredentialMode::HostAndTpm2 => HOST_TPM2,
    };
    if decoded.len() < 88
        || decoded.len() > CIPHERTEXT_LIMIT
        || decoded[..16] != expected
        || little32(decoded, 16)? != 32
        || little32(decoded, 20)? != 1
        || little32(decoded, 24)? != 12
        || little32(decoded, 28)? != 16
        || decoded[44..48].iter().any(|byte| *byte != 0)
    {
        return Err(reject);
    }
    if mode == LinuxCredentialMode::HostAndTpm2 {
        if decoded.len() < 112 {
            return Err(reject);
        }
        let mask = u64::from_le_bytes(decoded[48..56].try_into().map_err(|_| reject)?);
        let bank = u16::from_le_bytes(decoded[56..58].try_into().map_err(|_| reject)?);
        let algorithm = u16::from_le_bytes(decoded[58..60].try_into().map_err(|_| reject)?);
        let blob = little32(decoded, 60)? as usize;
        let policy = little32(decoded, 64)? as usize;
        if mask >> 24 != 0
            || !matches!(bank, 4 | 11)
            || !matches!(algorithm, 1 | 35)
            || blob == 0
            || blob > 16_384
            || policy == 0
            || policy > 16_384
        {
            return Err(reject);
        }
        let unaligned = 68 + blob + policy;
        let end = unaligned.div_ceil(8) * 8;
        if decoded.len() < end + 40 || decoded[unaligned..end].iter().any(|byte| *byte != 0) {
            return Err(reject);
        }
    }
    Ok(())
}
fn little32(bytes: &[u8], offset: usize) -> Result<u32, NativeFailure> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(NativeFailure::CredentialPolicyRejected)?
            .try_into()
            .map_err(|_| NativeFailure::CredentialPolicyRejected)?,
    ))
}

pub(crate) fn inspect_ciphertext(
    file: &File,
    mode: LinuxCredentialMode,
) -> Result<Vec<u8>, NativeFailure> {
    let mut encoded = vec![0; CIPHERTEXT_LIMIT + 1];
    let mut used = 0;
    loop {
        let count = file
            .read_at(&mut encoded[used..], used as u64)
            .map_err(|_| NativeFailure::IoUnavailable)?;
        if count == 0 {
            break;
        }
        used += count;
        if used > CIPHERTEXT_LIMIT {
            return Err(NativeFailure::LimitExceeded);
        }
    }
    encoded.truncate(used);
    let compact: Vec<_> = encoded
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let decoded = STANDARD
        .decode(compact)
        .map_err(|_| NativeFailure::CredentialPolicyRejected)?;
    inspect_recipe(&decoded, mode)?;
    Ok(encoded)
}

pub(crate) fn retrieve(
    permit: &CredentialReleasePermit,
    execution: RetainedCredentialExecution<'_>,
    destination: &mut LockedSecret32,
) -> Result<(), NativeFailure> {
    let result = retrieve_inner(permit, execution, destination);
    if result.is_err() {
        destination.wipe();
    }
    result
}
fn retrieve_inner(
    permit: &CredentialReleasePermit,
    execution: RetainedCredentialExecution<'_>,
    destination: &mut LockedSecret32,
) -> Result<(), NativeFailure> {
    let ciphertext = inspect_ciphertext(
        execution.ciphertext,
        permit.credential_binding().linux_mode()?,
    )?;
    let name = CString::new(format!(
        "--name={}",
        credential_name(permit.credential_binding())?
    ))
    .map_err(|_| NativeFailure::InvalidRecord)?;
    let arguments = [
        c"systemd-creds",
        c"decrypt",
        &name,
        c"--no-ask-password",
        c"--refuse-null",
        c"--newline=no",
        c"-",
        c"-",
    ];
    let mut argv: Vec<_> = arguments.iter().map(|arg| arg.as_ptr()).collect();
    argv.push(std::ptr::null());
    let environment = [
        c"PATH=/usr/bin:/bin",
        c"LC_ALL=C",
        c"SYSTEMD_LOG_LEVEL=emerg",
    ];
    let mut envp: Vec<_> = environment.iter().map(|arg| arg.as_ptr()).collect();
    envp.push(std::ptr::null());
    let (input_read, input_write) = anonymous_pipe()?;
    let (output_read, output_write) = anonymous_pipe()?;
    let error = File::options()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(|_| NativeFailure::IoUnavailable)?;
    // A stalled tool must leave time for exact kill/reap and guard retirement,
    // rather than racing the guard's group kill at the same absolute deadline.
    let deadline = crate::native_receiver::guard::credential_deadline()?
        .reserve(Duration::from_millis(400))?;
    // Stabilize dup2 sources even if the launcher started with a closed stdio.
    let input = duplicate(input_read.as_raw_fd())?;
    let output = duplicate(output_write.as_raw_fd())?;
    let errors = duplicate(error.as_raw_fd())?;
    let tool = duplicate(execution.tool.as_raw_fd())?;
    // This native lease includes the patched tool, host-key facility and all
    // loader/configuration/dependency ancestors, protected against replacement.
    permit.recheck()?;
    // SAFETY: the one-shot retriever is single-threaded and this child performs
    // only descriptor syscalls and authenticated descriptor-exec before any library.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(NativeFailure::NativeUnavailable);
    }
    if pid == 0 {
        if unsafe { libc::dup2(input.as_raw_fd(), 0) } < 0
            || unsafe { libc::dup2(output.as_raw_fd(), 1) } < 0
            || unsafe { libc::dup2(errors.as_raw_fd(), 2) } < 0
            || unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32) } != 0
        {
            unsafe { libc::_exit(34) }
        }
        unsafe {
            libc::syscall(
                libc::SYS_execveat,
                tool.as_raw_fd(),
                c"".as_ptr(),
                argv.as_ptr(),
                envp.as_ptr(),
                libc::AT_EMPTY_PATH,
            );
            libc::_exit(28);
        }
    }
    drop((input_read, output_write, error, input, output, errors, tool));
    let identity = match process_identity(pid as u32) {
        Ok(identity) => identity,
        Err(_) => {
            // The fork reservation is still ours, but without its identity no
            // success or fabricated wait token is possible. The independent
            // retriever guard remains responsible for group retirement.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            return Err(NativeFailure::CleanupUnresolved);
        }
    };
    let mut child = unsafe { ExactChild::inherit(identity) }?;
    let exchange = (|| {
        PipeEndpoint::take(input_write, true)?.write_frame(&ciphertext, deadline)?;
        let output = PipeEndpoint::take(output_read, false)?;
        // Native recognizer owns the locked output; preserve the absolute tool budget.
        let secret = recognize_credential_tool_output(output.descriptor()?, deadline.instant())?;
        match child.reap(deadline) {
            Ok(true) => {}
            Ok(false) => return Err(NativeFailure::CredentialUnavailable),
            Err(_) => return Err(NativeFailure::CleanupUnresolved),
        }
        permit.recheck()?;
        destination.write(secret.bytes())
    })();
    if exchange.is_err() && child.has_wait_reservation() {
        // A retained direct-child wait reservation prevents PID reuse here.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        if child
            .reap(ExchangeDeadline::start(Duration::from_millis(250))?)
            .is_err()
        {
            return Err(NativeFailure::CleanupUnresolved);
        }
    }
    exchange
}

fn duplicate(fd: i32) -> Result<OwnedFd, NativeFailure> {
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    if duplicate < 0 {
        return Err(NativeFailure::IoUnavailable);
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(mode: LinuxCredentialMode) -> Vec<u8> {
        let mut bytes = vec![0; 160];
        // Public recipe IDs are not credential payloads.
        bytes[..16].copy_from_slice(&match mode {
            LinuxCredentialMode::Host => HOST,
            LinuxCredentialMode::HostAndTpm2 => HOST_TPM2,
        });
        for (offset, value) in [(16, 32u32), (20, 1), (24, 12), (28, 16)] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        if mode == LinuxCredentialMode::HostAndTpm2 {
            bytes[56..58].copy_from_slice(&11u16.to_le_bytes());
            bytes[58..60].copy_from_slice(&35u16.to_le_bytes());
            bytes[60..64].copy_from_slice(&16u32.to_le_bytes());
            bytes[64..68].copy_from_slice(&32u32.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn decrypt_recipe_requires_the_registered_conjunction() {
        let host = header(LinuxCredentialMode::Host);
        let tpm = header(LinuxCredentialMode::HostAndTpm2);
        assert_eq!(inspect_recipe(&host, LinuxCredentialMode::Host), Ok(()));
        assert_eq!(
            inspect_recipe(&tpm, LinuxCredentialMode::HostAndTpm2),
            Ok(())
        );
        assert_eq!(
            inspect_recipe(&host, LinuxCredentialMode::HostAndTpm2),
            Err(NativeFailure::CredentialPolicyRejected)
        );
        assert_eq!(
            inspect_recipe(&tpm, LinuxCredentialMode::Host),
            Err(NativeFailure::CredentialPolicyRejected)
        );
        let mut unknown = host;
        unknown[..16].fill(0);
        assert_eq!(
            inspect_recipe(&unknown, LinuxCredentialMode::Host),
            Err(NativeFailure::CredentialPolicyRejected)
        );
    }

    #[test]
    fn decrypt_header_bounds_lengths_algorithms_and_padding() {
        let valid = header(LinuxCredentialMode::HostAndTpm2);
        for length in [0, 15, 31, 47, 87, 111, 151] {
            assert!(inspect_recipe(&valid[..length], LinuxCredentialMode::HostAndTpm2).is_err());
        }
        for offset in [16, 20, 24, 28, 44, 51, 56, 58, 60, 64, 116] {
            let mut invalid = valid.clone();
            invalid[offset] = 255;
            assert!(
                inspect_recipe(&invalid, LinuxCredentialMode::HostAndTpm2).is_err(),
                "offset {offset}"
            );
        }
        assert_eq!(
            inspect_recipe(&vec![0; CIPHERTEXT_LIMIT + 1], LinuxCredentialMode::Host),
            Err(NativeFailure::CredentialPolicyRejected)
        );
    }

    #[test]
    fn ciphertext_inspection_is_bounded_and_read_only() {
        use std::io::Write;
        let mut file = tempfile::tempfile().unwrap();
        let bytes = STANDARD.encode(header(LinuxCredentialMode::Host));
        file.write_all(bytes.as_bytes()).unwrap();
        let inspected = inspect_ciphertext(&file, LinuxCredentialMode::Host).unwrap();
        assert_eq!(inspected, bytes.as_bytes());
        assert_eq!(file.metadata().unwrap().len(), bytes.len() as u64);
        assert!(inspect_ciphertext(&file, LinuxCredentialMode::HostAndTpm2).is_err());
        file.set_len((CIPHERTEXT_LIMIT + 1) as u64).unwrap();
        assert!(matches!(
            inspect_ciphertext(&file, LinuxCredentialMode::Host),
            Err(NativeFailure::LimitExceeded)
        ));
    }
}
