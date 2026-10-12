use super::*;
use native_management::cli_sudo::{NativeSudoBoundary, RetainedSudoExecution, reply};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

struct Fixture {
    _directory: tempfile::TempDir,
    program: PathBuf,
}
fn fixture(mode: u8) -> Arc<Fixture> {
    let directory = tempfile::tempdir().unwrap();
    let program = directory.path().join("broker-fixture");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/native_management_composition/broker_fixture.c");
    let status = Command::new("/usr/bin/cc")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg(format!("-DOUTPUT_MODE={mode}"))
        .arg(source)
        .arg("-o")
        .arg(&program)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "fixture compiler refused");
    Arc::new(Fixture {
        _directory: directory,
        program,
    })
}
struct Custody {
    fixture: Arc<Fixture>,
    observations: Arc<Mutex<(u32, u32, u32)>>,
    replace_in_prepare: bool,
}
struct RetainedClosure {
    fixture: Arc<Fixture>,
    observations: Arc<Mutex<(u32, u32, u32)>>,
    replaced: bool,
}
impl Drop for RetainedClosure {
    fn drop(&mut self) {
        self.observations.lock().unwrap().2 += 1;
    }
}
impl RetainedSudoExecution for Custody {
    type Helper = RetainedClosure;
    fn retain(&self, _: &ManagementRequest) -> Result<Self::Helper, NativeFailure> {
        self.observations.lock().unwrap().0 += 1;
        Ok(RetainedClosure {
            fixture: self.fixture.clone(),
            observations: self.observations.clone(),
            replaced: false,
        })
    }
    fn recheck(&self, helper: &mut Self::Helper) -> Result<(), NativeFailure> {
        self.observations.lock().unwrap().1 += 1;
        if helper.replaced {
            return Err(NativeFailure::ExecutionBindingUnproven);
        }
        Ok(())
    }
    fn prepare(
        &self,
        helper: &mut Self::Helper,
        invocation: SudoInvocation,
    ) -> Result<Command, NativeFailure> {
        assert_eq!(
            self.observations.lock().unwrap().2,
            0,
            "retained closure dropped before exec"
        );
        helper.replaced = self.replace_in_prepare;
        let mut command = Command::new(&helper.fixture.program);
        command
            .args(invocation.prefix())
            .arg("fixture-manager")
            .args(invocation.manager_arguments());
        // Synthetic injection markers, never passwords, signing material, or credentials.
        for key in [
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "SSH_ASKPASS",
            "SUDO_ASKPASS",
            "SPACETERM_SIGNING_KEYCHAIN",
        ] {
            command.env(key, "fixture-injection-marker");
        }
        Ok(command)
    }
}
#[test]
fn native_sudo_exec_retains_protected_manager_and_system_closure() {
    let program = fixture(0);
    for mode in [SudoMode::Interactive, SudoMode::NonInteractive] {
        let observations = Arc::new(Mutex::new((0, 0, 0)));
        let custody = Custody {
            fixture: program.clone(),
            observations: observations.clone(),
            replace_in_prepare: false,
        };
        let adapter = SudoAdapter::new(NativeSudoBoundary::new(custody));
        let result = adapter
            .execute(&request(OperationTag::Start), mode)
            .unwrap();
        assert_eq!(result.state, ProgressState::Complete);
        assert_eq!(result.request, [1; 16]);
        assert_eq!(*observations.lock().unwrap(), (1, 2, 1));
    }
}
#[test]
fn native_sudo_rechecks_replacement_between_preparation_and_exec() {
    let observations = Arc::new(Mutex::new((0, 0, 0)));
    let adapter = SudoAdapter::new(NativeSudoBoundary::new(Custody {
        fixture: fixture(0),
        observations: observations.clone(),
        replace_in_prepare: true,
    }));
    assert_eq!(
        adapter.execute(&request(OperationTag::Start), SudoMode::Interactive),
        Err(NativeFailure::ExecutionBindingUnproven)
    );
    assert_eq!(*observations.lock().unwrap(), (1, 2, 1));
}
#[test]
fn native_sudo_bounds_reply_discards_stderr_and_does_not_trust_exit_or_stdout_alone() {
    for (mode, expected) in [
        (1, NativeFailure::LimitExceeded),
        (2, NativeFailure::AuthorizationCancelled),
        (3, NativeFailure::InvalidRecord),
        (4, NativeFailure::AuthorizationUnavailable),
        (5, NativeFailure::CustodyUnavailable),
        (6, NativeFailure::AuthorizationUnavailable),
    ] {
        let observations = Arc::new(Mutex::new((0, 0, 0)));
        let adapter = SudoAdapter::new(NativeSudoBoundary::new(Custody {
            fixture: fixture(mode),
            observations: observations.clone(),
            replace_in_prepare: false,
        }));
        assert_eq!(
            adapter.execute(&request(OperationTag::Start), SudoMode::NonInteractive),
            Err(expected)
        );
        assert_eq!(observations.lock().unwrap().2, 1);
    }
}
#[test]
fn manager_reply_rejects_unknown_reserved_truncated_or_additional_records() {
    for bytes in [
        b"classification=47\n".as_slice(),
        b"classification=0\n",
        b"classification=17",
        b"classification=17\nclassification=17\n",
        b"progress=01010101010101010101010101010101,1,2\n",
        b"progress=01010101010101010101010101010101,8,1\n",
    ] {
        assert_eq!(reply::decode(bytes), Err(NativeFailure::InvalidRecord));
    }
    let progress = ManagementProgress {
        request: [1; 16],
        phase: ManagementPhase::Completed,
        state: ProgressState::Complete,
    };
    assert_eq!(
        reply::decode(reply::encode(progress).as_bytes()),
        Ok(Ok(progress))
    );
}
