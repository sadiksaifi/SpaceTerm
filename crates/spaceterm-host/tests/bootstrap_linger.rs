use spaceterm_core::bootstrap::{BootstrapLinger, BootstrapOs, BootstrapSudoAvailability};
use spaceterm_host::bootstrap::{
    linger::{
        BootstrapLingerFailure, BootstrapLingerTransport, LingerAuthenticationPrompt, LingerFuture,
        LingerInvocation, ScopedLingerAuthentication, enable_linger_if_needed,
    },
    probe::ReadOnlyHostProbe,
    process::{BootstrapProbeFailure, BootstrapProbeLease},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use zeroize::Zeroizing;

fn host() -> ReadOnlyHostProbe {
    ReadOnlyHostProbe::parse(b"STBP2\nuid=1000\nos=Linux\narch=x86_64\nversion=0.0.0\nlibc=glibc 2.35\nkernel=5.15.0\nmanager=systemd-user\nmanager_version=249\nmanager_reachable=yes\nlinger=no\nsudo=available\n", 1000).unwrap()
}
#[derive(Default)]
struct State {
    changed: AtomicBool,
    prompts: AtomicUsize,
    retains: AtomicUsize,
    invocations: AtomicUsize,
    observations: AtomicUsize,
    retired: AtomicBool,
}
struct Lease(Arc<State>);
impl BootstrapProbeLease for Lease {
    fn account_uid(&self) -> u32 {
        1000
    }
    fn revalidate(&self) -> Result<(), BootstrapProbeFailure> {
        if self.0.changed.load(Ordering::Acquire) {
            Err(BootstrapProbeFailure::RouteChanged)
        } else {
            Ok(())
        }
    }
}
struct Prompt {
    state: Arc<State>,
    response: Vec<u8>,
    change_route: bool,
}
impl LingerAuthenticationPrompt for Prompt {
    fn revalidate(&self, uid: u32) -> Result<(), BootstrapLingerFailure> {
        if uid != 1000 || self.state.changed.load(Ordering::Acquire) {
            Err(BootstrapLingerFailure::RouteChanged)
        } else {
            Ok(())
        }
    }
    fn respond(&self, _: u32) -> LingerFuture<'_, Zeroizing<Vec<u8>>> {
        Box::pin(async move {
            self.state.prompts.fetch_add(1, Ordering::SeqCst);
            if self.change_route {
                self.state.changed.store(true, Ordering::Release);
            }
            Ok(Zeroizing::new(self.response.clone()))
        })
    }
}
struct Exchange(Arc<State>);
impl Drop for Exchange {
    fn drop(&mut self) {
        self.0.retired.store(true, Ordering::Release);
    }
}
struct Transport {
    state: Arc<State>,
    attempts: usize,
    uid: u32,
    observed: BootstrapLinger,
    denied: bool,
    pending: bool,
}
impl BootstrapLingerTransport for Transport {
    type Exchange = Exchange;
    fn retain<'a>(
        &'a self,
        ssh: &'a dyn BootstrapProbeLease,
        invocation: &'a LingerInvocation,
    ) -> LingerFuture<'a, Exchange> {
        Box::pin(async move {
            ssh.revalidate()
                .map_err(|_| BootstrapLingerFailure::RouteChanged)?;
            assert_eq!(invocation.executable(), "/usr/bin/sudo");
            assert_eq!(
                invocation.arguments(),
                [
                    "-k",
                    "-p",
                    "[SpaceTerm linger password]",
                    "--",
                    "/usr/bin/loginctl",
                    "enable-linger",
                    "--",
                    "1000"
                ]
                .map(str::to_owned)
            );
            assert_eq!(invocation.account_uid(), 1000);
            self.state.retains.fetch_add(1, Ordering::SeqCst);
            Ok(Exchange(self.state.clone()))
        })
    }
    fn revalidate(&self, _: &Exchange) -> Result<(), BootstrapLingerFailure> {
        if self.state.changed.load(Ordering::Acquire) {
            Err(BootstrapLingerFailure::RouteChanged)
        } else {
            Ok(())
        }
    }
    fn invoke<'a>(
        &'a self,
        _: &'a mut Exchange,
        _: &'a LingerInvocation,
        authentication: &'a ScopedLingerAuthentication<'a>,
    ) -> LingerFuture<'a, ()> {
        Box::pin(async move {
            self.state.invocations.fetch_add(1, Ordering::SeqCst);
            if self.pending {
                return std::future::pending().await;
            }
            for _ in 0..self.attempts {
                let _response = authentication.respond(self.uid).await?;
            }
            if self.denied {
                Err(BootstrapLingerFailure::Denied)
            } else {
                Ok(())
            }
        })
    }
    fn observe<'a>(&'a self, _: &'a Exchange, uid: u32) -> LingerFuture<'a, BootstrapLinger> {
        Box::pin(async move {
            assert_eq!(uid, 1000);
            self.state.observations.fetch_add(1, Ordering::SeqCst);
            Ok(self.observed)
        })
    }
}
fn setup() -> (Arc<State>, Lease, Transport, Prompt) {
    let state = Arc::new(State::default());
    (
        state.clone(),
        Lease(state.clone()),
        Transport {
            state: state.clone(),
            attempts: 1,
            uid: 1000,
            observed: BootstrapLinger::Enabled,
            denied: false,
            pending: false,
        },
        Prompt {
            state,
            response: b"test-password".to_vec(),
            change_route: false,
        },
    )
}

#[tokio::test]
async fn enabled_linger_and_macos_never_retain_sudo_or_present_a_prompt() {
    for macos in [false, true] {
        let (state, ssh, transport, prompt) = setup();
        let mut host = host();
        host.sudo = BootstrapSudoAvailability::Unavailable;
        host.linger = BootstrapLinger::Enabled;
        if macos {
            host.os = BootstrapOs::MacOs;
            host.linger = BootstrapLinger::NotApplicable;
        }
        enable_linger_if_needed(&transport, &ssh, &host, &prompt)
            .await
            .unwrap();
        assert_eq!(state.retains.load(Ordering::SeqCst), 0);
        assert_eq!(state.prompts.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn disabled_linger_authorizes_only_fixed_loginctl_and_verifies_observation() {
    let (state, ssh, transport, prompt) = setup();
    enable_linger_if_needed(&transport, &ssh, &host(), &prompt)
        .await
        .unwrap();
    assert_eq!(state.retains.load(Ordering::SeqCst), 1);
    assert_eq!(state.prompts.load(Ordering::SeqCst), 1);
    assert_eq!(state.observations.load(Ordering::SeqCst), 1);
    assert!(state.retired.load(Ordering::Acquire));
}
#[tokio::test]
async fn unavailable_linger_is_not_disabled_and_never_authorizes() {
    let (state, ssh, transport, prompt) = setup();
    let mut host = host();
    host.linger = BootstrapLinger::Unavailable;
    assert_eq!(
        enable_linger_if_needed(&transport, &ssh, &host, &prompt).await,
        Err(BootstrapLingerFailure::StateUnavailable)
    );
    assert_eq!(state.retains.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn fourth_password_request_is_refused_before_prompting() {
    let (state, ssh, mut transport, prompt) = setup();
    transport.attempts = 4;
    assert_eq!(
        enable_linger_if_needed(&transport, &ssh, &host(), &prompt).await,
        Err(BootstrapLingerFailure::LimitExceeded)
    );
    assert_eq!(state.prompts.load(Ordering::SeqCst), 3);
    assert!(state.retired.load(Ordering::Acquire));
}
#[tokio::test]
async fn changed_account_cannot_receive_the_password() {
    let (state, ssh, mut transport, prompt) = setup();
    transport.uid = 1001;
    assert_eq!(
        enable_linger_if_needed(&transport, &ssh, &host(), &prompt).await,
        Err(BootstrapLingerFailure::RouteChanged)
    );
    assert_eq!(state.prompts.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn route_change_while_prompting_discards_response_and_retires_exchange() {
    let (state, ssh, transport, mut prompt) = setup();
    prompt.change_route = true;
    assert_eq!(
        enable_linger_if_needed(&transport, &ssh, &host(), &prompt).await,
        Err(BootstrapLingerFailure::RouteChanged)
    );
    assert_eq!(state.observations.load(Ordering::SeqCst), 0);
    assert!(state.retired.load(Ordering::Acquire));
}
#[tokio::test]
async fn terminal_controls_and_unbounded_passwords_are_never_delivered() {
    for (response, expected) in [
        (
            b"password\ncommand".to_vec(),
            BootstrapLingerFailure::InvalidResponse,
        ),
        (
            b"password\x03".to_vec(),
            BootstrapLingerFailure::InvalidResponse,
        ),
        (
            vec![b'x'; 16 * 1024 + 1],
            BootstrapLingerFailure::LimitExceeded,
        ),
        (Vec::new(), BootstrapLingerFailure::LimitExceeded),
    ] {
        let (state, ssh, transport, mut prompt) = setup();
        prompt.response = response;
        assert_eq!(
            enable_linger_if_needed(&transport, &ssh, &host(), &prompt).await,
            Err(expected)
        );
        assert!(state.retired.load(Ordering::Acquire));
        assert!(!format!("{expected:?}").contains("password"));
    }
}
#[tokio::test]
async fn authorization_denial_retires_without_observing_success() {
    let (state, ssh, mut transport, prompt) = setup();
    transport.denied = true;
    assert_eq!(
        enable_linger_if_needed(&transport, &ssh, &host(), &prompt).await,
        Err(BootstrapLingerFailure::Denied)
    );
    assert!(state.retired.load(Ordering::Acquire));
    assert_eq!(state.observations.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn successful_sudo_exit_is_not_an_enabled_linger_certificate() {
    let (_, ssh, mut transport, prompt) = setup();
    transport.observed = BootstrapLinger::Disabled;
    assert_eq!(
        enable_linger_if_needed(&transport, &ssh, &host(), &prompt).await,
        Err(BootstrapLingerFailure::StateUnavailable)
    );
}
#[tokio::test(start_paused = true)]
async fn linger_deadline_retires_exact_exchange_without_a_password_prompt() {
    let (state, ssh, mut transport, prompt) = setup();
    transport.pending = true;
    let host = host();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(121),
        enable_linger_if_needed(&transport, &ssh, &host, &prompt),
    )
    .await;
    assert!(
        matches!(result, Ok(Err(BootstrapLingerFailure::TimedOut))),
        "linger exchange did not enforce its deadline"
    );
    assert!(state.retired.load(Ordering::Acquire));
    assert_eq!(state.prompts.load(Ordering::SeqCst), 0);
}
