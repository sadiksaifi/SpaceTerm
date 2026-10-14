//! Adapts the existing window-owned, zeroized AskPass broker to native SSH tunnels.

use crate::{
    platform::app_paths::AppPaths,
    ssh::askpass::{AskPassAttemptFactory, AskPassBrokerLease, AskPassWindowFactory},
};
use spaceterm_host::{
    ssh_tunnel::{AskPassAuthorityCheck, AskPassHandoff, TunnelAskPass},
    transport::TransportError,
};
use std::sync::{Arc, Mutex, Weak};

pub(crate) struct DesktopTunnelAskPass {
    paths: Arc<AppPaths>,
    windows: Arc<dyn AskPassWindowFactory>,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    attempts: Option<Arc<dyn AskPassAttemptFactory>>,
    active: Vec<Weak<AttemptBroker>>,
}

/// The handoff, then the owned SSH start, holds the only strong broker owner.
struct AttemptBroker(AskPassBrokerLease);

impl Drop for AttemptBroker {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl DesktopTunnelAskPass {
    pub(crate) fn new(paths: Arc<AppPaths>, windows: Arc<dyn AskPassWindowFactory>) -> Arc<Self> {
        Arc::new(Self {
            paths,
            windows,
            state: Mutex::new(State::default()),
        })
    }
    pub(crate) fn bind_window(&self, window: &gpui::Window, cx: &mut gpui::App) {
        if let Ok(mut state) = self.state.lock() {
            for broker in state.active.drain(..).filter_map(|broker| broker.upgrade()) {
                broker.0.cancel();
            }
            state.attempts = self.windows.create(window, cx).ok();
        }
    }

    fn start_handoff(
        &self,
        authority: Option<AskPassAuthorityCheck>,
    ) -> Result<AskPassHandoff, TransportError> {
        let mut state = self.state.lock().map_err(|_| TransportError::Unavailable)?;
        let factory = state.attempts.as_ref().ok_or(TransportError::Unavailable)?;
        let attempt = match authority {
            Some(authority) => factory.start_authorized_attempt(&self.paths, authority),
            None => factory.start_attempt(&self.paths),
        }
        .map_err(|_| TransportError::Unavailable)?;
        let broker = Arc::new(AttemptBroker(attempt.lease));
        state.active.retain(|broker| broker.strong_count() != 0);
        state.active.push(Arc::downgrade(&broker));
        drop(state);
        let mut helper = None;
        let mut endpoint = None;
        for (name, value) in broker.0.ordinary_spawn_entries() {
            match name {
                "SSH_ASKPASS" => helper = Some(std::path::PathBuf::from(value)),
                "SPACETERM_SSH_ASKPASS_SOCKET" => endpoint = Some(value.to_owned()),
                _ => {}
            }
        }
        let capability = broker.0.capability_copy_for_spawn();
        Ok(AskPassHandoff::new(
            helper.ok_or(TransportError::Unavailable)?,
            endpoint.ok_or(TransportError::Unavailable)?,
            zeroize::Zeroizing::new(capability.as_bytes().to_vec()),
        )?
        .with_attempt_lifetime(move || drop(broker)))
    }
}
impl Drop for DesktopTunnelAskPass {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            for broker in state.active.drain(..).filter_map(|broker| broker.upgrade()) {
                broker.0.cancel();
            }
        }
    }
}

impl TunnelAskPass for DesktopTunnelAskPass {
    fn handoff(&self) -> Result<AskPassHandoff, TransportError> {
        self.start_handoff(None)
    }

    fn handoff_authorized(
        &self,
        authority: AskPassAuthorityCheck,
    ) -> Result<AskPassHandoff, TransportError> {
        if !authority() {
            return Err(TransportError::PeerAccountRejected);
        }
        self.start_handoff(Some(authority))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::askpass::prompt::{AskPassPromptKind, AskPassRequest};
    use crate::ssh::askpass::{
        AskPassAttempt, AskPassAttemptObservation, AskPassHelperReply, AskPassLocalAccept,
        AskPassLocalIpc, AskPassLocalListener, AskPassLocalStream, AskPassUnavailable,
        BoundAskPassEndpoint, GpuiAskPassBrokerFactory, read_reply, write_request,
    };
    use gpui::{Render, TestAppContext, VisualTestContext, div};
    use std::io::{Cursor, Read, Write};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::{Duration, Instant};

    type ConnectionSender = mpsc::Sender<Box<dyn AskPassLocalStream>>;

    #[derive(Default)]
    struct ControlledIpc {
        connections: Mutex<Vec<ConnectionSender>>,
    }

    struct ControlledListener(Mutex<mpsc::Receiver<Box<dyn AskPassLocalStream>>>);

    impl AskPassLocalListener for ControlledListener {
        fn accept_authenticated(&self) -> AskPassLocalAccept {
            match self.0.lock().unwrap().try_recv() {
                Ok(stream) => AskPassLocalAccept::Connected(stream),
                Err(mpsc::TryRecvError::Empty) => AskPassLocalAccept::Pending,
                Err(mpsc::TryRecvError::Disconnected) => AskPassLocalAccept::Failed,
            }
        }
    }

    impl AskPassLocalIpc for ControlledIpc {
        fn bind(&self, _paths: &AppPaths) -> Result<BoundAskPassEndpoint, AskPassUnavailable> {
            let (sender, receiver) = mpsc::channel();
            let mut connections = self.connections.lock().unwrap();
            let address = format!("00000123:/test/broker-{}", connections.len()).into();
            connections.push(sender);
            Ok(BoundAskPassEndpoint::new(
                address,
                Box::new(ControlledListener(Mutex::new(receiver))),
            ))
        }
    }

    struct ControlledStream {
        input: Cursor<Vec<u8>>,
        output: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for ControlledStream {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(bytes)
        }
    }

    impl Write for ControlledStream {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.output.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Clone)]
    struct RecordedAttempt {
        lease: AskPassBrokerLease,
        observation: AskPassAttemptObservation,
        connections: ConnectionSender,
    }

    impl RecordedAttempt {
        fn request(&self) -> Arc<Mutex<Vec<u8>>> {
            let mut input = Vec::new();
            write_request(
                &mut input,
                self.lease.capability_copy_for_spawn().as_bytes(),
                &AskPassRequest::new("Password:".to_owned(), AskPassPromptKind::Secret).unwrap(),
            )
            .unwrap();
            let output = Arc::new(Mutex::new(Vec::new()));
            assert!(
                self.connections
                    .send(Box::new(ControlledStream {
                        input: Cursor::new(input),
                        output: Arc::clone(&output),
                    }))
                    .is_ok()
            );
            output
        }
    }

    struct RecordingFactory {
        inner: GpuiAskPassBrokerFactory,
        ipc: Arc<ControlledIpc>,
        attempts: Arc<Mutex<Vec<RecordedAttempt>>>,
    }

    impl RecordingFactory {
        fn record(&self, attempt: AskPassAttempt) -> AskPassAttempt {
            self.attempts.lock().unwrap().push(RecordedAttempt {
                lease: attempt.lease.clone(),
                observation: attempt.observation.clone(),
                connections: self.ipc.connections.lock().unwrap().last().unwrap().clone(),
            });
            attempt
        }
    }

    impl AskPassAttemptFactory for RecordingFactory {
        fn start_attempt(&self, paths: &AppPaths) -> Result<AskPassAttempt, AskPassUnavailable> {
            Ok(self.record(self.inner.start_attempt(paths)?))
        }

        fn start_authorized_attempt(
            &self,
            paths: &AppPaths,
            authority: AskPassAuthorityCheck,
        ) -> Result<AskPassAttempt, AskPassUnavailable> {
            Ok(self.record(self.inner.start_authorized_attempt(paths, authority)?))
        }
    }

    #[derive(Default)]
    struct TestWindows {
        ipc: Arc<ControlledIpc>,
        attempts: Arc<Mutex<Vec<RecordedAttempt>>>,
    }

    impl AskPassWindowFactory for TestWindows {
        fn create(
            &self,
            window: &gpui::Window,
            cx: &mut gpui::App,
        ) -> Result<Arc<dyn AskPassAttemptFactory>, AskPassUnavailable> {
            Ok(Arc::new(RecordingFactory {
                inner: GpuiAskPassBrokerFactory::new(
                    window,
                    cx,
                    self.ipc.clone(),
                    "/test/spaceterm".into(),
                )?,
                ipc: self.ipc.clone(),
                attempts: self.attempts.clone(),
            }))
        }
    }

    struct Harness;

    impl Render for Harness {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            use gpui::prelude::*;
            spaceterm_ui::ModalLayer::new(div().size_full())
        }
    }

    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        Arc<DesktopTunnelAskPass>,
        Arc<TestWindows>,
        &mut VisualTestContext,
    ) {
        use crate::platform::{
            app_directories::AppDirectoryEnvironment,
            testing::{RecordingFilesystem, resolve_app_paths},
        };
        // Native broker threads wake the foreground executor through the UI channel.
        cx.executor().allow_parking();
        cx.update(crate::ui::init).unwrap();
        let paths = Arc::new(
            resolve_app_paths(
                &AppDirectoryEnvironment {
                    home: Some("/test/home".into()),
                    ..Default::default()
                },
                Some("/test/runtime".into()),
                104,
                Arc::new(RecordingFilesystem::default()),
            )
            .unwrap(),
        );
        let windows = Arc::new(TestWindows::default());
        let adapter = DesktopTunnelAskPass::new(paths, windows.clone());
        let (_, cx) = cx.add_window_view(|_, _| Harness);
        cx.update(|window, cx| {
            window.activate_window();
            adapter.bind_window(window, cx);
        });
        (adapter, windows, cx)
    }

    #[track_caller]
    fn wait_until(
        cx: &mut VisualTestContext,
        mut condition: impl FnMut(&mut VisualTestContext) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            cx.run_until_parked();
            if condition(cx) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "AskPass fixture watchdog expired"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    struct HeldForegroundDial {
        adapter: Arc<DesktopTunnelAskPass>,
        cleanup: Arc<tokio::sync::Semaphore>,
    }

    impl spaceterm_client::ports::RouteConnector for HeldForegroundDial {
        fn open<'a>(
            &'a self,
            _: &'a spaceterm_client::api::ConnectionTarget,
            _: spaceterm_client::ports::ConnectionPurpose,
        ) -> spaceterm_client::ports::ClientFuture<'a, spaceterm_client::ports::ConnectedRoute>
        {
            Box::pin(async {
                Err(spaceterm_client::failure::ClientFailure::new(
                    spaceterm_client::failure::ClientFailureClass::Unavailable,
                ))
            })
        }

        fn open_foreground<'a>(
            &'a self,
            _: &'a spaceterm_client::api::ConnectionTarget,
            cancellation: spaceterm_client::ports::ForegroundCancellation,
        ) -> spaceterm_client::ports::ClientFuture<'a, spaceterm_client::ports::ConnectedRoute>
        {
            Box::pin(async move {
                let handoff = self
                    .adapter
                    .handoff_authorized(Arc::new(move || !cancellation.is_cancelled()))
                    .unwrap();
                // Native cleanup remains owned after the actor abandons this dial.
                // Prompt dismissal must not depend on releasing this gate.
                self.cleanup.acquire().await.unwrap().forget();
                drop(handoff);
                Err(spaceterm_client::failure::ClientFailure::new(
                    spaceterm_client::failure::ClientFailureClass::Unavailable,
                ))
            })
        }
    }

    struct NoAuthenticatedMessages;
    impl spaceterm_client::ports::OrganizationDecoder for NoAuthenticatedMessages {
        fn decode(
            &self,
            _: spaceterm_protocol::codec::Payload,
        ) -> Result<
            spaceterm_client::ports::OrganizationPublication,
            spaceterm_client::failure::ClientFailure,
        > {
            panic!("an unfinished native dial cannot decode protocol messages")
        }
    }
    impl spaceterm_client::ports::ConnectionDelegate for NoAuthenticatedMessages {
        fn invalidate(
            &self,
            _: &spaceterm_client::api::PairedServerBinding,
            _: spaceterm_client::ports::ConnectionInvalidation,
        ) {
        }
        fn reconcile<'a>(
            &'a self,
            _: spaceterm_client::api::ConnectedInfo,
            _: &'a spaceterm_client::replica::ReplicaSnapshot,
            _: &'a mut dyn spaceterm_client::ports::ProtocolChannel,
        ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
            Box::pin(async { panic!("an unfinished native dial cannot reconcile") })
        }
        fn publication(
            &self,
            _: spaceterm_client::api::ConnectedInfo,
            _: &spaceterm_client::replica::ReplicaSnapshot,
        ) -> Result<(), spaceterm_client::failure::ClientFailure> {
            panic!("an unfinished native dial cannot publish")
        }
        fn admit(
            &self,
            _: spaceterm_client::api::ConnectedInfo,
            _: &spaceterm_protocol::codec::Payload,
        ) -> Result<spaceterm_client::ports::OutboundClass, spaceterm_client::failure::ClientFailure>
        {
            panic!("an unfinished native dial cannot admit effects")
        }
        fn receive<'a>(
            &'a self,
            _: spaceterm_client::api::ConnectedInfo,
            _: spaceterm_protocol::codec::Payload,
            _: &'a mut dyn spaceterm_client::ports::ProtocolChannel,
        ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
            Box::pin(async { panic!("an unfinished native dial cannot receive") })
        }
    }

    fn abandoned_actor_dismisses_prompt_before_cleanup(cx: &mut TestAppContext, replace: bool) {
        use spaceterm_client::{
            limits::ClientLimits,
            ports::ClientServices,
            registry::ClientRegistry,
            testing::{FakePaneConnection, FakeServer},
        };
        let (adapter, windows, cx) = setup(cx);
        let executor = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let cleanup = Arc::new(tokio::sync::Semaphore::new(0));
        let (registry, runtime) = executor.block_on(async {
            let server = FakeServer::new(1, vec![2, 1]).unwrap();
            let peer = FakePaneConnection::new(server.clone(), 1, 2).await.unwrap();
            ClientRegistry::recover(ClientServices {
                routes: Arc::new(HeldForegroundDial {
                    adapter,
                    cleanup: cleanup.clone(),
                }),
                credentials: server,
                journal: peer.journal().clone(),
                organization: Arc::new(NoAuthenticatedMessages),
                delegate: Arc::new(NoAuthenticatedMessages),
                discovery: None,
                limits: ClientLimits::default(),
                additional_capabilities: vec![],
            })
            .await
            .unwrap()
        });
        let mut runtime = Some(runtime);
        let server = registry.server(registry.servers()[0].server).unwrap();
        executor.block_on(server.retry()).unwrap();
        wait_until(cx, |_| !windows.attempts.lock().unwrap().is_empty());
        let original = windows.attempts.lock().unwrap()[0].clone();
        let reply = original.request();
        wait_until(cx, |cx| {
            cx.debug_bounds("ssh-askpass-secret-input").is_some()
        });
        cx.simulate_input("late reply must be rejected");
        let shutdown = if replace {
            executor.block_on(server.retry()).unwrap();
            None
        } else {
            Some(executor.spawn(runtime.take().unwrap().shutdown()))
        };
        wait_until(cx, |cx| {
            original.observation.cancelled()
                && cx.debug_bounds("ssh-askpass-secret-input").is_none()
        });
        cx.simulate_keystrokes("enter");
        wait_until(cx, |_| reply.lock().unwrap().len() == 5);
        assert!(matches!(
            read_reply(&mut Cursor::new(reply.lock().unwrap().clone())).unwrap(),
            AskPassHelperReply::Cancelled
        ));
        assert_eq!(cleanup.available_permits(), 0);
        assert_eq!(windows.attempts.lock().unwrap().len(), 1);
        if let Some(shutdown) = shutdown {
            assert!(!shutdown.is_finished(), "native cleanup must remain owned");
            cleanup.add_permits(1);
            executor.block_on(shutdown).unwrap().unwrap();
        } else {
            cleanup.add_permits(1);
            wait_until(cx, |_| windows.attempts.lock().unwrap().len() == 2);
            let retry = windows.attempts.lock().unwrap()[1].clone();
            assert!(!retry.observation.cancelled());
            let retry_reply = retry.request();
            wait_until(cx, |cx| {
                cx.debug_bounds("ssh-askpass-secret-input").is_some()
            });
            let shutdown = executor.spawn(runtime.take().unwrap().shutdown());
            wait_until(cx, |cx| {
                retry.observation.cancelled()
                    && cx.debug_bounds("ssh-askpass-secret-input").is_none()
            });
            wait_until(cx, |_| retry_reply.lock().unwrap().len() == 5);
            assert!(matches!(
                read_reply(&mut Cursor::new(retry_reply.lock().unwrap().clone())).unwrap(),
                AskPassHelperReply::Cancelled
            ));
            assert!(!shutdown.is_finished());
            cleanup.add_permits(1);
            executor.block_on(shutdown).unwrap().unwrap();
        }
    }

    #[gpui::test]
    fn actor_shutdown_dismisses_foreground_prompt_before_native_cleanup(cx: &mut TestAppContext) {
        abandoned_actor_dismisses_prompt_before_cleanup(cx, false);
    }

    #[gpui::test]
    fn actor_retry_dismisses_original_prompt_before_native_cleanup(cx: &mut TestAppContext) {
        abandoned_actor_dismisses_prompt_before_cleanup(cx, true);
    }

    #[gpui::test]
    fn cancelled_tunnel_attempt_dismisses_active_prompt_and_retry_gets_an_independent_broker(
        cx: &mut TestAppContext,
    ) {
        let (adapter, windows, cx) = setup(cx);
        let handoff = adapter.handoff().unwrap();
        let original = windows.attempts.lock().unwrap()[0].clone();
        let original_reply = original.request();
        wait_until(cx, |cx| {
            cx.debug_bounds("ssh-askpass-secret-input").is_some()
        });
        assert!(original.observation.prompt_active());
        assert!(original_reply.lock().unwrap().is_empty());

        // This is the handoff lifetime that native timeout/cancellation releases.
        // Retained observations and lease clones must not keep its prompt alive.
        drop(handoff);
        wait_until(cx, |cx| {
            original.observation.cancelled()
                && cx.debug_bounds("ssh-askpass-secret-input").is_none()
        });
        let retry_handoff = adapter.handoff().unwrap();
        let retry = windows.attempts.lock().unwrap()[1].clone();
        assert!(
            original.lease.capability_copy_for_spawn().as_bytes()
                != retry.lease.capability_copy_for_spawn().as_bytes()
        );
        let retry_reply = retry.request();
        wait_until(cx, |cx| {
            cx.debug_bounds("ssh-askpass-secret-input").is_some()
        });
        assert!(retry.observation.prompt_active());
        assert!(!retry.observation.cancelled());
        cx.simulate_input("fresh response");
        cx.simulate_keystrokes("enter");
        wait_until(cx, |_| {
            retry_reply.lock().unwrap().len() == 5 + b"fresh response".len()
        });
        assert!(matches!(
            read_reply(&mut Cursor::new(retry_reply.lock().unwrap().clone())).unwrap(),
            AskPassHelperReply::Secret(secret) if secret.as_slice() == b"fresh response"
        ));
        wait_until(cx, |_| original_reply.lock().unwrap().len() == 5);
        assert!(matches!(
            read_reply(&mut Cursor::new(original_reply.lock().unwrap().clone())).unwrap(),
            AskPassHelperReply::Cancelled
        ));
        drop(retry_handoff);
    }

    #[gpui::test]
    fn revoked_foreground_generation_dismisses_active_prompt_and_rejects_another_handoff(
        cx: &mut TestAppContext,
    ) {
        let (adapter, windows, cx) = setup(cx);
        let valid = Arc::new(AtomicBool::new(true));
        let authority: AskPassAuthorityCheck = {
            let valid = Arc::clone(&valid);
            Arc::new(move || valid.load(Ordering::Acquire))
        };
        let handoff = adapter.handoff_authorized(authority.clone()).unwrap();
        let original = windows.attempts.lock().unwrap()[0].clone();
        let reply = original.request();
        wait_until(cx, |cx| {
            cx.debug_bounds("ssh-askpass-secret-input").is_some()
        });
        cx.simulate_input("must not leave the revoked attempt");
        // The credential adapter supplies this check from CredentialUse::validate.
        // Self-unpair seals that exact generation while the handoff remains alive.
        valid.store(false, Ordering::Release);
        wait_until(cx, |cx| {
            original.observation.cancelled()
                && cx.debug_bounds("ssh-askpass-secret-input").is_none()
        });
        wait_until(cx, |_| reply.lock().unwrap().len() == 5);
        assert!(matches!(
            read_reply(&mut Cursor::new(reply.lock().unwrap().clone())).unwrap(),
            AskPassHelperReply::Cancelled
        ));
        assert!(matches!(
            adapter.handoff_authorized(authority),
            Err(TransportError::PeerAccountRejected)
        ));
        assert_eq!(windows.attempts.lock().unwrap().len(), 1);
        drop(handoff);
    }
}
