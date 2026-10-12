use super::*;
use native_management::{
    cli_sudo::ssh_authorization::{
        NativeSshSudoBoundary, ScopedSshSudoAuthentication, SshSudoAuthenticationPrompt,
        SshSudoManagement,
    },
    facade::{NativeAction, NativeAssessment},
};
use spaceterm_host::native_deployment::contracts::NativeManagementFuture;
use zeroize::Zeroizing;

struct Prompt {
    calls: Arc<Mutex<u32>>,
    failure: Option<NativeFailure>,
    size: usize,
}
impl SshSudoAuthenticationPrompt for Prompt {
    fn respond(&self, _: &ManagementRequest) -> NativeManagementFuture<'_, Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            *self.calls.lock().unwrap() += 1;
            if let Some(failure) = self.failure {
                return Err(failure);
            }
            Ok(Zeroizing::new(vec![0; self.size]))
        })
    }
}
struct Exchange(Arc<Mutex<Vec<&'static str>>>);
impl Drop for Exchange {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("reap");
    }
}
struct SshPeer {
    calls: Arc<Mutex<Vec<&'static str>>>,
    substitute: bool,
    attempts: u8,
}
impl NativeSshSudoBoundary for SshPeer {
    type Exchange = Exchange;
    fn retain(&self, _: &ManagementRequest) -> NativeManagementFuture<'_, Exchange> {
        Box::pin(async move {
            self.calls.lock().unwrap().push("retain");
            Ok(Exchange(self.calls.clone()))
        })
    }
    fn recheck(&self, _: &Exchange, _: &ManagementRequest) -> Result<(), NativeFailure> {
        self.calls.lock().unwrap().push("recheck");
        Ok(())
    }
    fn invoke<'a>(
        &'a self,
        _: &'a mut Exchange,
        invocation: SudoInvocation,
        request: &'a ManagementRequest,
        authentication: &'a ScopedSshSudoAuthentication<'a>,
    ) -> NativeManagementFuture<'a, ManagementProgress> {
        Box::pin(async move {
            assert_eq!(invocation.executable(), "/usr/bin/sudo");
            assert_eq!(invocation.prefix(), &["-k", "--"]);
            assert_eq!(invocation.manager_arguments(), &["--request-stdin"]);
            let mut bytes = request.encode();
            if self.substitute {
                bytes[88] ^= 1;
            }
            let submitted = ManagementRequest::decode(&bytes).unwrap();
            for _ in 0..self.attempts {
                let response = authentication.respond(&submitted).await?;
                assert!(!response.is_empty());
            }
            self.calls.lock().unwrap().push("effect");
            Ok(ManagementProgress {
                request: *request.request_bytes(),
                phase: ManagementPhase::Completed,
                state: ProgressState::Complete,
            })
        })
    }
}
fn confirmed() -> native_management::facade::ConfirmedNativeAction {
    let action = NativeAction::Start;
    let mut bytes = frame(OperationTag::Start);
    bytes[88..120].copy_from_slice(&action.digest());
    NativeAssessment::from_native(
        ManagementRequest::decode(&bytes).unwrap(),
        action,
        std::time::Duration::from_secs(1),
    )
    .unwrap()
    .confirm()
}
#[tokio::test]
async fn ssh_bootstrap_scoped_sudo_authorization_zeroizes_response_and_never_caches() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let prompts = Arc::new(Mutex::new(0));
    let management = SshSudoManagement::new(
        SshPeer {
            calls: calls.clone(),
            substitute: false,
            attempts: 1,
        },
        Prompt {
            calls: prompts.clone(),
            failure: None,
            size: 32,
        },
    );
    management.execute(confirmed()).await.unwrap();
    management.execute(confirmed()).await.unwrap();
    assert_eq!(*prompts.lock().unwrap(), 2);
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "retain", "recheck", "effect", "recheck", "reap", "retain", "recheck", "effect",
            "recheck", "reap"
        ]
    );
}
#[tokio::test]
async fn ssh_sudo_substituted_scope_never_receives_an_authentication_response() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let prompts = Arc::new(Mutex::new(0));
    let management = SshSudoManagement::new(
        SshPeer {
            calls: calls.clone(),
            substitute: true,
            attempts: 1,
        },
        Prompt {
            calls: prompts.clone(),
            failure: None,
            size: 32,
        },
    );
    assert_eq!(
        management.execute(confirmed()).await,
        Err(NativeFailure::IdentityMismatch)
    );
    assert_eq!(*prompts.lock().unwrap(), 0);
    assert_eq!(*calls.lock().unwrap(), ["retain", "recheck", "reap"]);
}
#[tokio::test]
async fn ssh_sudo_denial_cancellation_and_response_limits_have_zero_effects() {
    for (failure, size, attempts, expected, prompt_count) in [
        (
            Some(NativeFailure::AuthorizationDenied),
            32,
            1,
            NativeFailure::AuthorizationDenied,
            1,
        ),
        (
            Some(NativeFailure::AuthorizationCancelled),
            32,
            1,
            NativeFailure::AuthorizationCancelled,
            1,
        ),
        (
            Some(NativeFailure::AuthorizationUnavailable),
            32,
            1,
            NativeFailure::AuthorizationUnavailable,
            1,
        ),
        (None, 16 * 1024 + 1, 1, NativeFailure::LimitExceeded, 1),
        (None, 32, 4, NativeFailure::LimitExceeded, 3),
    ] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let prompts = Arc::new(Mutex::new(0));
        let management = SshSudoManagement::new(
            SshPeer {
                calls: calls.clone(),
                substitute: false,
                attempts,
            },
            Prompt {
                calls: prompts.clone(),
                failure,
                size,
            },
        );
        assert_eq!(management.execute(confirmed()).await, Err(expected));
        assert_eq!(*prompts.lock().unwrap(), prompt_count);
        assert_eq!(*calls.lock().unwrap(), ["retain", "recheck", "reap"]);
    }
}
