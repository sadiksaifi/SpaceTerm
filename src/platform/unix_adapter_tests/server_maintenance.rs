use super::*;

#[gpui::test]
#[ignore = "build isolated previous-policy executables with mise run server:ssh:trust:test"]
fn previous_policy_executable_update_transition(cx: &mut gpui::TestAppContext) {
    use crate::ssh::server_process::testing::backend_with_mechanisms;
    use std::{
        io::Write,
        process::{Command, Stdio},
        sync::atomic::{AtomicBool, Ordering},
    };
    let root = std::path::PathBuf::from(
        std::env::var_os("SPACETERM_SSH_TRUST_FIXTURE").expect("run server:ssh:trust:test"),
    );
    let home = root.join("home");
    let data = home.join(".local/share").join(build()).join("server");
    let policy = std::fs::read(data.join("service.json")).unwrap();
    let identity = std::fs::read(data.join("identity.json")).unwrap();
    let previous = serde_json::from_slice::<serde_json::Value>(&policy).unwrap()["installation"]
        .as_str()
        .unwrap()
        .to_owned();
    let server: [u8; 32] =
        serde_json::from_slice::<serde_json::Value>(&identity).unwrap()["server"]
            .as_array()
            .unwrap()
            .iter()
            .map(|byte| byte.as_u64().unwrap() as u8)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
    let old_executable = data
        .join("installations")
        .join(&previous)
        .join("bin/spaceterm-server");
    let refused = Arc::new(AtomicBool::new(false));
    let refused_copy = refused.clone();
    let corrupt_description = Arc::new(AtomicBool::new(true));
    let corrupt_copy = corrupt_description.clone();
    let home_copy = home.clone();
    let account = answering(move |script, sent| {
        let mut process = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .env("HOME", &home_copy)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        process.stdin.take().unwrap().write_all(sent).unwrap();
        let output = process.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "fixture command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if script.contains("'ssh-stage'") {
            let Reply::Staged { candidate } = serde_json::from_slice(&output.stdout).unwrap()
            else {
                panic!("not staged")
            };
            let mut previous = Command::new(&old_executable)
                .args(["ssh-maintain", &build()])
                .env("HOME", &home_copy)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            previous
                .stdin
                .take()
                .unwrap()
                .write_all(
                    &serde_json::to_vec(&Request::PrepareUpdate {
                        candidate: candidate.digest,
                    })
                    .unwrap(),
                )
                .unwrap();
            let previous = previous.wait_with_output().unwrap();
            assert!(previous.status.success());
            assert_eq!(
                serde_json::from_slice::<Reply>(&previous.stdout).unwrap(),
                Reply::Refused {
                    refusal: MaintenanceRefusal::CandidateUnavailable
                }
            );
            refused_copy.store(true, Ordering::SeqCst);
        }
        match serde_json::from_slice::<Request>(sent) {
            Ok(Request::PrepareUpdate { .. }) if corrupt_copy.swap(false, Ordering::SeqCst) => {
                let Reply::Prepared(mut change) = serde_json::from_slice(&output.stdout).unwrap()
                else {
                    return (output.stdout, 0);
                };
                change.candidate.as_mut().unwrap().digest = "b".repeat(64);
                (encoded(&Reply::Prepared(change)), 0)
            }
            Ok(Request::ApplyUpdate { operation, .. }) if operation == OPERATION => (Vec::new(), 1),
            _ => (output.stdout, 0),
        }
    });
    let new_backend = |artifacts| {
        backend_with_mechanisms(
            cx.background_executor.clone(),
            account.clone(),
            artifacts,
            Arc::new(crate::platform::unix_host_config_filesystem::UnixHostConfigFilesystem),
            Arc::new(crate::platform::unix_ssh_process::UnixSshProcessAdapter),
        )
        .0
    };
    let backend = new_backend(SshServerArtifacts::new(Some(root.join("catalog"))));
    let target = target(SshServerContinuity {
        server,
        account: unsafe { libc::geteuid() },
        ..bound()
    });
    let cancellation = SshCancellationToken::default();
    assert_eq!(
        backend.prepare_update(
            &target,
            MaintenanceAuthority::Requested,
            &cancellation,
            &silent
        ),
        Err(SshMaintenanceFailure::Refused(
            MaintenanceRefusal::CandidateChanged
        ))
    );
    let change = backend
        .prepare_update(
            &target,
            MaintenanceAuthority::Requested,
            &cancellation,
            &silent,
        )
        .unwrap()
        .unwrap();
    assert!(
        refused.load(Ordering::SeqCst),
        "the previous executable must reject the new trust"
    );
    assert!(change.stopped && !change.running);
    assert_eq!(std::fs::read(data.join("service.json")).unwrap(), policy);
    let kind = SubmittedKind::Update {
        candidate: change.candidate.as_ref().unwrap().digest.clone(),
    };
    let refused = backend
        .apply(
            &target,
            &OperationSubmission {
                operation: [5; 32],
                kind: kind.clone(),
                confirmation: Some("0".repeat(64)),
            },
            &cancellation,
            &silent,
        )
        .unwrap();
    assert_eq!(
        refused.resolution,
        OperationResolution::Refused(MaintenanceRefusal::Changed)
    );
    assert_eq!(std::fs::read(data.join("service.json")).unwrap(), policy);
    std::fs::write(data.join("interrupt-replace"), b"once").unwrap();
    assert_eq!(
        backend.apply(
            &target,
            &OperationSubmission {
                operation: OPERATION,
                kind: kind.clone(),
                confirmation: Some(change.confirmation)
            },
            &cancellation,
            &silent
        ),
        Err(SshMaintenanceFailure::Unconfirmed)
    );
    let interrupted = std::fs::read(data.join("service.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&interrupted).unwrap()["installation"],
        previous
    );
    // A relaunch has no candidate catalog. The persisted original supplies its exact executor;
    // neither an older approved verifier nor a newer artifact may take over that original.
    let recovered = new_backend(SshServerArtifacts::new(None));
    let changed = MaintenanceTarget {
        continuity: SshServerContinuity {
            server: [8; 32],
            ..target.continuity.clone()
        },
        ..target.clone()
    };
    assert_eq!(
        recovered.resolve(
            &changed,
            OPERATION,
            &kind,
            MaintenanceAuthority::Requested,
            &cancellation,
            &silent
        ),
        Err(SshMaintenanceFailure::Changed(
            ContinuityChange::ServerChanged
        ))
    );
    assert_eq!(
        std::fs::read(data.join("service.json")).unwrap(),
        interrupted
    );
    let result = recovered
        .resolve(
            &target,
            OPERATION,
            &kind,
            MaintenanceAuthority::Quiet,
            &cancellation,
            &silent,
        )
        .unwrap();
    assert_eq!(result.resolution, OperationResolution::Completed);
    assert!(result.status.unwrap().stopped);
    assert_eq!(std::fs::read(data.join("identity.json")).unwrap(), identity);
    let final_policy: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("service.json")).unwrap()).unwrap();
    assert_eq!(
        final_policy["installation"],
        change.candidate.unwrap().digest
    );
    assert_eq!(asked(&account).iter().filter(|request| matches!(request, Request::ApplyUpdate { operation, .. } if *operation == OPERATION)).count(), 1);
}
