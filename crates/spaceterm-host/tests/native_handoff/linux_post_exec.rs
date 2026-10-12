//! Real pidfd/executable observations with an injected retained custody issuer.

use std::{
    io::{BufRead, BufReader, Write},
    num::NonZeroU64,
    os::fd::FromRawFd,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use super::*;
use crate::{
    native_deployment::contracts::DataGenerationId,
    startup::{AccountBuildKey, BuildIdentity, InstallationId},
    startup_exchange::unix::process_identity,
};

struct CustodyPeer {
    context: PendingStartupContext,
    server: File,
    revoked: Arc<AtomicU32>,
}

impl RetainedLinuxExecution for CustodyPeer {
    fn context(&self) -> &PendingStartupContext {
        &self.context
    }
    fn server(&self) -> &File {
        &self.server
    }
    fn recheck(&self, executable: &File, user: &File, mount: &File) -> Result<(), NativeFailure> {
        match self.revoked.load(Ordering::SeqCst) {
            0 => {}
            1 => return Err(NativeFailure::CustodyUnavailable),
            _ => return Err(NativeFailure::LinuxMountWitnessUnavailable),
        }
        if file_binding(executable)? != file_binding(&self.server)?
            || inode(user)? != inode(&File::open("/proc/self/ns/user").unwrap())?
            || inode(mount)? != inode(&File::open("/proc/self/ns/mnt").unwrap())?
        {
            return Err(NativeFailure::CustodyUnavailable);
        }
        Ok(())
    }
}

fn context() -> PendingStartupContext {
    let process = process_identity(std::process::id()).unwrap();
    PendingStartupContext {
        key: AccountBuildKey {
            account_uid: unsafe { libc::getuid() },
            build_identity: BuildIdentity::Development,
        },
        primary_gid: unsafe { libc::getgid() },
        registration_generation: NonZeroU64::new(1).unwrap(),
        lifecycle_generation: NonZeroU64::new(2).unwrap(),
        data_generation: DataGenerationId::new(3).unwrap(),
        installation: InstallationId(NonZeroU64::new(4).unwrap()),
        server_id: [1; 16],
        account_binding_sha256: [2; 32],
        installation_binding: [3; 32],
        server_artifact_sha256: [4; 32],
        manifest_sha256: [5; 32],
        credential_binding_sha256: [6; 32],
        startup_nonce: [7; 32],
        original_process: process,
        retriever_process: crate::startup::ProcessIdentity {
            pid: process.pid + 1,
            start_seconds: 1,
            start_microseconds: 0,
        },
        journal_request: None,
    }
}

fn pidfd(pid: u32) -> OwnedFd {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    assert!(fd >= 0);
    unsafe { OwnedFd::from_raw_fd(fd) }
}

// The only privileged peer is injected here. Production reads all four native
// UID/GID slots, supplementary groups and capability sets from retained procfs.
struct CredentialsPeer;
impl ProcessCredentials for CredentialsPeer {
    fn recheck(
        &self,
        _: &File,
        context: &PendingStartupContext,
        _: ExchangeDeadline,
    ) -> Result<(), NativeFailure> {
        let uid = context.key.account_uid;
        let gid = context.primary_gid;
        let status = format!(
            "Uid: {uid} {uid} {uid} {uid}\nGid: {gid} {gid} {gid} {gid}\nGroups:\nCapInh: 0\nCapPrm: 0\nCapEff: 0\nCapAmb: 0\n"
        );
        require_credentials(status.as_bytes(), context)
    }
}

#[test]
fn retained_linux_post_exec_issues_context_bound_proof() {
    let context = context();
    let custody = CustodyPeer {
        context: context.clone(),
        server: File::open("/proc/self/exe").unwrap(),
        revoked: Arc::new(AtomicU32::new(0)),
    };
    let lease = verify_retained_post_exec(
        pidfd(context.original_process.pid),
        Box::new(custody),
        Box::new(CredentialsPeer),
    );
    assert!(lease.is_ok(), "{:?}", lease.as_ref().err());
    assert_eq!(lease.unwrap().into_proof().recheck(&context), Ok(()));
}

fn peer(context: PendingStartupContext) -> CustodyPeer {
    CustodyPeer {
        server: File::open(format!("/proc/{}/exe", context.original_process.pid)).unwrap(),
        context,
        revoked: Arc::new(AtomicU32::new(0)),
    }
}

#[test]
fn pidfd_and_start_binding_reject_another_process_or_reused_incarnation() {
    let mut context = context();
    let mut child = OwnedChild(
        Command::new("/usr/bin/sleep")
            .arg("10")
            .env_clear()
            .spawn()
            .unwrap(),
    );
    let result = verify_retained_post_exec(
        pidfd(child.0.id()),
        Box::new(peer(context.clone())),
        Box::new(CredentialsPeer),
    );
    assert_eq!(result.err(), Some(NativeFailure::ProcessIdentityMismatch));
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    context.original_process.start_seconds += 1;
    let result = verify_retained_post_exec(
        pidfd(std::process::id()),
        Box::new(peer(context)),
        Box::new(CredentialsPeer),
    );
    assert_eq!(result.err(), Some(NativeFailure::ProcessIdentityMismatch));
}

#[test]
fn matching_mount_queries_never_replace_retained_publication_authority() {
    let context = context();
    let custody = peer(context.clone());
    let revoked = custody.revoked.clone();
    let proof = verify_retained_post_exec(
        pidfd(std::process::id()),
        Box::new(custody),
        Box::new(CredentialsPeer),
    )
    .unwrap()
    .into_proof();
    revoked.store(1, Ordering::SeqCst);
    assert_eq!(
        proof.recheck(&context),
        Err(NativeFailure::CustodyUnavailable)
    );
    revoked.store(2, Ordering::SeqCst);
    assert_eq!(
        proof.recheck(&context),
        Err(NativeFailure::LinuxMountWitnessUnavailable)
    );
}

#[test]
fn changed_executable_or_retained_namespace_refuses_execution_binding() {
    let context = context();
    let mut custody = peer(context.clone());
    custody.server = File::open("/usr/bin/sleep").unwrap();
    let result = verify_retained_post_exec(
        pidfd(std::process::id()),
        Box::new(custody),
        Box::new(CredentialsPeer),
    );
    assert_eq!(result.err(), Some(NativeFailure::ExecutionBindingUnproven));
    let mut lease = verify_retained_post_exec(
        pidfd(std::process::id()),
        Box::new(peer(context.clone())),
        Box::new(CredentialsPeer),
    )
    .unwrap();
    // A substituted namespace pin must not inherit a previously issued lease.
    lease.user_namespace = File::open("/proc/self/ns/pid").unwrap();
    assert_eq!(
        lease.into_proof().recheck(&context),
        Err(NativeFailure::ExecutionBindingUnproven)
    );
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn retained_pidfd_recheck_rejects_same_pid_exec_substitution_and_exit() {
    let mut child = OwnedChild(
        Command::new("/bin/sh")
            .env_clear()
            .args([
                "-c",
                "printf 'ready\\n'; read -r token; exec /usr/bin/sleep 10",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut ready = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    let mut context = context();
    context.original_process = process_identity(child.0.id()).unwrap();
    let custody = peer(context.clone());
    let old_executable = inode(&custody.server).unwrap();
    let proof = verify_retained_post_exec(
        pidfd(child.0.id()),
        Box::new(custody),
        Box::new(CredentialsPeer),
    )
    .unwrap()
    .into_proof();
    child
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(b"continue\n")
        .unwrap();
    let limit = Instant::now() + Duration::from_secs(1);
    while inode(&File::open(format!("/proc/{}/exe", child.0.id())).unwrap()).unwrap()
        == old_executable
    {
        assert!(Instant::now() < limit);
        std::thread::yield_now();
    }
    assert_eq!(
        process_identity(child.0.id()).unwrap(),
        context.original_process
    );
    assert_eq!(
        proof.recheck(&context),
        Err(NativeFailure::ExecutionBindingUnproven)
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert_eq!(
        proof.recheck(&context),
        Err(NativeFailure::ProcessIdentityMismatch)
    );
}

#[test]
fn retained_proof_binds_exact_context_and_never_extends_its_deadline() {
    let context = context();
    let lease = verify_retained_post_exec(
        pidfd(std::process::id()),
        Box::new(peer(context.clone())),
        Box::new(CredentialsPeer),
    )
    .unwrap();
    let deadline = lease.deadline;
    let proof = lease.into_proof();
    let mut changed = context.clone();
    changed.startup_nonce[0] ^= 1;
    assert_eq!(
        proof.recheck(&changed),
        Err(NativeFailure::ProcessIdentityMismatch)
    );
    changed = context.clone();
    changed.lifecycle_generation = NonZeroU64::new(3).unwrap();
    assert_eq!(
        proof.recheck(&changed),
        Err(NativeFailure::ProcessIdentityMismatch)
    );
    assert_eq!(proof.recheck(&context), Ok(()));
    std::thread::sleep(deadline.remaining().unwrap() + Duration::from_millis(1));
    assert_eq!(proof.recheck(&context), Err(NativeFailure::TimedOut));
}

#[test]
fn native_privilege_status_requires_all_four_ids_empty_groups_and_no_capabilities() {
    let context = context();
    let uid = context.key.account_uid;
    let gid = context.primary_gid;
    let valid = format!(
        "Uid: {uid} {uid} {uid} {uid}\nGid: {gid} {gid} {gid} {gid}\nGroups:\nCapInh: 0\nCapPrm: 0\nCapEff: 0\nCapAmb: 0\n"
    );
    assert_eq!(require_credentials(valid.as_bytes(), &context), Ok(()));
    for (old, new) in [
        (
            format!("Uid: {uid} {uid} {uid} {uid}"),
            format!("Uid: {uid} {uid} 0 {uid}"),
        ),
        (
            format!("Gid: {gid} {gid} {gid} {gid}"),
            format!("Gid: {gid} {gid} {gid} 0"),
        ),
        ("Groups:".to_owned(), "Groups: 0".to_owned()),
        ("CapAmb: 0".to_owned(), "CapAmb: 1".to_owned()),
        ("CapEff: 0".to_owned(), "CapEff: 0\nCapEff: 0".to_owned()),
        ("CapPrm: 0".to_owned(), "CapPrm: invalid".to_owned()),
        ("CapInh: 0\n".to_owned(), String::new()),
    ] {
        assert_eq!(
            require_credentials(valid.replace(&old, &new).as_bytes(), &context),
            Err(NativeFailure::PrivilegeDropUnproven)
        );
    }
}
