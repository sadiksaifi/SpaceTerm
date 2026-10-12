//! Explicitly injected native release peer. This module is absent from default
//! builds and exposes observations, not a permit issuer, to the subprocess fixture.

use std::{fs::File, num::NonZeroU64, os::unix::fs::FileExt};

use super::{RetainedCredentialExecution, SystemdBootCredentials};
use crate::{
    boot_credentials::{
        BootCredentialAdapter, CredentialReleasePermit, LockedSecret32, RetainedCredentialRelease,
    },
    native_deployment::{
        contracts::{DataGenerationId, NativeFailure},
        profile::{BootCredentialBinding, LinuxCredentialMode},
    },
    startup::{AccountBuildKey, BuildIdentity, InstallationId, PendingStartupContext},
    startup_exchange::unix::{LockedMemory, process_identity},
};

/// Retirement follows native effects, not the executor's number of rechecks.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Retirement {
    Never,
    BeforeTool,
    AfterTool,
}

struct ReleasePeer {
    context: PendingStartupContext,
    binding: BootCredentialBinding,
    tool: File,
    ciphertext: File,
    audit: File,
    retirement: Retirement,
}
impl RetainedCredentialRelease for ReleasePeer {
    fn context(&self) -> &PendingStartupContext {
        &self.context
    }
    fn credential_binding(&self) -> &BootCredentialBinding {
        &self.binding
    }
    fn recheck(&self) -> Result<(), NativeFailure> {
        let retired = match self.retirement {
            Retirement::Never => false,
            Retirement::BeforeTool => true,
            Retirement::AfterTool => self.audit.metadata().unwrap().len() != 0,
        };
        if retired {
            Err(NativeFailure::GenerationMismatch)
        } else {
            Ok(())
        }
    }
    fn linux_execution(&self) -> Result<RetainedCredentialExecution<'_>, NativeFailure> {
        Ok(RetainedCredentialExecution {
            tool: &self.tool,
            ciphertext: &self.ciphertext,
        })
    }
}

/// Public facts for the fixed synthetic peer, never deployment authority.
pub fn binding() -> BootCredentialBinding {
    BootCredentialBinding::linux(LinuxCredentialMode::Host, [1; 32], [2; 32], [3; 16]).unwrap()
}

/// Bounded content-free observations from the actual production executor.
pub struct Observation {
    pub outcome: Result<(), NativeFailure>,
    pub destination_wiped: bool,
    pub tool_called: bool,
    pub tool_contract_checked: bool,
    pub tool_reaped: bool,
}

/// Runs only inside the single-threaded guarded fixture retriever. Neither the
/// injected lease nor its resulting permit can escape this test composition.
pub fn exercise(tool: File, ciphertext: File, audit: &File, retirement: Retirement) -> Observation {
    let process = process_identity(std::process::id()).unwrap();
    let binding = binding();
    let context = PendingStartupContext {
        key: AccountBuildKey {
            account_uid: unsafe { libc::getuid() },
            build_identity: BuildIdentity::Development,
        },
        primary_gid: unsafe { libc::getgid() },
        registration_generation: NonZeroU64::new(1).unwrap(),
        lifecycle_generation: NonZeroU64::new(2).unwrap(),
        data_generation: DataGenerationId::new(3).unwrap(),
        installation: InstallationId(NonZeroU64::new(4).unwrap()),
        server_id: [3; 16],
        account_binding_sha256: [2; 32],
        installation_binding: [4; 32],
        server_artifact_sha256: [5; 32],
        manifest_sha256: [6; 32],
        credential_binding_sha256: binding.sha256().unwrap(),
        startup_nonce: [7; 32],
        original_process: process,
        retriever_process: process,
        journal_request: None,
    };
    let permit = CredentialReleasePermit::retain(ReleasePeer {
        context,
        binding,
        tool,
        ciphertext,
        audit: audit.try_clone().unwrap(),
        retirement,
    });
    let mut initial = LockedMemory::<32>::allocate().unwrap();
    getrandom::fill(initial.bytes_mut()).unwrap();
    let mut destination = LockedSecret32::new().unwrap();
    destination.write(initial.bytes()).unwrap();
    let outcome = SystemdBootCredentials.retrieve_for_startup(&permit, &mut destination);
    let destination_wiped = destination.bytes().iter().all(|byte| *byte == 0);
    destination.wipe();
    let mut record = [0; 9];
    let count = audit.read_at(&mut record, 0).unwrap();
    let tool_called = count != 0;
    let tool_contract_checked =
        count == 8 && u32::from_le_bytes(record[4..8].try_into().unwrap()) == 1;
    let tool_reaped = if count == 8 {
        let pid = u32::from_le_bytes(record[..4].try_into().unwrap());
        (unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) }) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
    } else {
        !tool_called
    };
    Observation {
        outcome,
        destination_wiped,
        tool_called,
        tool_contract_checked,
        tool_reaped,
    }
}
