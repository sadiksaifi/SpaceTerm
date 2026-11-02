//! Orders clipboard effects without blocking the worker on the UI thread.
use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;

use super::*;
use crate::terminal::osc52::{MAX_OSC52_CONTENT_BYTES, Osc52Operation};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct ClipboardAuthority {
    connected: AtomicBool,
    epoch: AtomicU64,
    subscription: Mutex<Option<async_channel::Sender<TerminalSessionEvent>>>,
}

impl ClipboardAuthority {
    pub(crate) fn client_focus(&self, focused: Option<bool>) {
        self.connected.store(false, Ordering::Release);
        let _ = self
            .epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some((old.wrapping_add(2) & !1) | u64::from(focused == Some(true)))
            });
        self.connected.store(focused.is_some(), Ordering::Release);
    }
    pub(crate) fn output_epoch(&self) -> Option<u64> {
        self.connected
            .load(Ordering::Acquire)
            .then(|| self.epoch.load(Ordering::Acquire))
    }
    pub(crate) fn native_permits(&self, epoch: u64) -> bool {
        self.output_epoch() == Some(epoch)
    }
    pub(crate) fn native_request(&self, epoch: Option<u64>) -> bool {
        epoch.is_some_and(|epoch| self.native_permits(epoch))
    }

    pub(super) fn focus(&self, focused: bool) {
        // Increment on every transition, including a loss followed by regained focus.
        let _ = self
            .epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                (old & 1 != u64::from(focused))
                    .then_some((old.wrapping_add(2) & !1) | u64::from(focused))
            });
    }
    pub(super) fn observe_subscription(&self, events: async_channel::Sender<TerminalSessionEvent>) {
        self.focus(false);
        if let Ok(mut subscription) = self.subscription.lock() {
            *subscription = Some(events);
        }
    }

    pub(super) fn grant(&self) -> Option<u64> {
        if self
            .subscription
            .lock()
            .ok()?
            .as_ref()
            .is_some_and(async_channel::Sender::is_closed)
        {
            return None;
        }
        if !self.connected.load(Ordering::Acquire) {
            return None;
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        (epoch & 1 == 1).then_some(epoch)
    }
    fn permits(&self, epoch: u64) -> bool {
        self.grant() == Some(epoch)
    }
}

pub enum ClipboardCompletion {
    Denied,
    Written,
    Text(String),
}

impl fmt::Debug for ClipboardCompletion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Denied => f.write_str("Denied"),
            Self::Written => f.write_str("Written"),
            Self::Text(text) => f
                .debug_struct("Text")
                .field("byte_len", &text.len())
                .finish(),
        }
    }
}

/// Receives a relayed request's completion on the client. It must not block.
pub(crate) type ClipboardRelay = Arc<dyn Fn(u64, ClipboardCompletion) + Send + Sync>;

pub struct ClipboardRequest {
    id: u64,
    operation: Osc52Operation,
    authority: Arc<ClipboardAuthority>,
    epoch: u64,
    deadline: Instant,
    commands: Option<CommandSender<Command>>,
    relay: Option<ClipboardRelay>,
}

impl fmt::Debug for ClipboardRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardRequest")
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ClientEffectGrant {
    authority: Arc<ClipboardAuthority>,
    epoch: u64,
    deadline: Instant,
}

impl fmt::Debug for ClientEffectGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClientEffectGrant { .. }")
    }
}

impl ClientEffectGrant {
    #[cfg(feature = "desktop")]
    pub(crate) fn owned(&self) -> bool {
        self.authority.native_permits(self.epoch)
    }
    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero() && self.authority.native_permits(self.epoch))
    }
    pub fn current(&self) -> bool {
        Instant::now() < self.deadline && self.authority.native_permits(self.epoch)
    }
}

#[derive(Clone, Debug)]
pub struct ClientEffectRequest {
    pub effect: spaceterm_server::pane::effects::Effect,
    pub grant: ClientEffectGrant,
}

impl ClientEffectRequest {
    pub(crate) fn new(
        effect: spaceterm_server::pane::effects::Effect,
        authority: Arc<ClipboardAuthority>,
        epoch: u64,
        remaining: Duration,
    ) -> Self {
        Self {
            effect,
            grant: ClientEffectGrant {
                authority,
                epoch,
                deadline: Instant::now() + remaining,
            },
        }
    }
}

impl ClipboardRequest {
    #[cfg(feature = "desktop")]
    pub(crate) fn relayed(
        id: u64,
        operation: Osc52Operation,
        authority: Arc<ClipboardAuthority>,
        epoch: u64,
        remaining: Duration,
        relay: ClipboardRelay,
    ) -> Self {
        Self {
            id,
            operation,
            authority,
            epoch,
            deadline: Instant::now() + remaining,
            commands: None,
            relay: Some(relay),
        }
    }

    /// Returns the current operation without granting native clipboard authority.
    pub fn operation(&self) -> &Osc52Operation {
        &self.operation
    }

    /// Revoked or expired requests cannot consult a client clipboard.
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero() && self.authority.permits(self.epoch))
    }

    pub fn complete(&mut self, completion: ClipboardCompletion) {
        let completion = if self.remaining().is_some() {
            completion
        } else {
            ClipboardCompletion::Denied
        };
        if let Some(relay) = self.relay.take() {
            relay(self.id, completion);
            return;
        }
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(Command::CompleteClipboard(self.id, completion));
        }
    }
}

impl Drop for ClipboardRequest {
    fn drop(&mut self) {
        self.complete(ClipboardCompletion::Denied);
    }
}

struct PendingClipboard {
    id: u64,
    read: Option<(
        crate::terminal::osc52::Osc52Target,
        crate::terminal::osc52::Osc52Terminator,
    )>,
    epoch: u64,
    deadline: Instant,
}

#[derive(Default)]
pub(super) struct WorkerClipboard {
    requests: Option<async_channel::Sender<ClipboardRequest>>,
    commands: Option<CommandSender<Command>>,
    pub(super) authority: Arc<ClipboardAuthority>,
    pending: Option<PendingClipboard>,
    next_id: u64,
    pub(super) effects: VecDeque<(Osc52Effect, Option<u64>)>,
    pub(super) deferred_readers: VecDeque<Option<u64>>,
    pub(super) reader_stop: Option<Option<crate::platform::native_pty::NativePtyReadFailure>>,
}

impl WorkerClipboard {
    pub(super) fn connect(
        commands: CommandSender<Command>,
    ) -> (Self, async_channel::Receiver<ClipboardRequest>) {
        let (requests, receiver) = async_channel::bounded(1);
        (
            Self {
                requests: Some(requests),
                commands: Some(commands),
                ..Self::default()
            },
            receiver,
        )
    }

    pub(super) fn replace_requests(&mut self, requests: async_channel::Sender<ClipboardRequest>) {
        self.requests = Some(requests);
    }

    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }
    pub(super) fn expired(&self) -> bool {
        self.deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub(super) fn enqueue(
        &mut self,
        effects: impl IntoIterator<Item = (Osc52Effect, Option<u64>)>,
    ) {
        self.effects.extend(effects);
    }

    pub(super) fn begin(
        &mut self,
        operation: Osc52Operation,
        epoch: Option<u64>,
    ) -> Option<Vec<u8>> {
        let read = match &operation {
            Osc52Operation::Read { target, terminator } => Some((*target, *terminator)),
            Osc52Operation::Write { .. } => None,
        };
        let denied = || {
            read.map(|(target, terminator)| {
                crate::terminal::osc52::read_response(target, terminator, "")
            })
        };
        let (Some(requests), Some(commands), Some(epoch)) = (
            &self.requests,
            &self.commands,
            epoch.filter(|epoch| self.authority.permits(*epoch)),
        ) else {
            return denied();
        };
        self.next_id = self.next_id.wrapping_add(1);
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let request = ClipboardRequest {
            id: self.next_id,
            operation,
            relay: None,
            authority: Arc::clone(&self.authority),
            epoch,
            deadline,
            commands: Some(commands.clone()),
        };
        if requests.try_send(request).is_err() {
            return denied();
        }
        self.pending = Some(PendingClipboard {
            id: self.next_id,
            read,
            epoch,
            deadline,
        });
        None
    }

    pub(super) fn complete(
        &mut self,
        id: Option<u64>,
        completion: ClipboardCompletion,
    ) -> (bool, Option<Vec<u8>>) {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| id.is_some_and(|id| pending.id != id))
        {
            return (false, None);
        }
        let Some(pending) = self.pending.take() else {
            return (false, None);
        };
        let text = match completion {
            ClipboardCompletion::Text(text)
                if self.authority.permits(pending.epoch)
                    && Instant::now() < pending.deadline
                    && text.len() <= MAX_OSC52_CONTENT_BYTES =>
            {
                text
            }
            _ => String::new(),
        };
        (
            true,
            pending.read.map(|(target, terminator)| {
                crate::terminal::osc52::read_response(target, terminator, &text)
            }),
        )
    }
}

#[cfg(all(test, feature = "desktop"))]
mod tests {
    use super::*;
    use crate::terminal::native_services::clipboard::{
        ClipboardError, ClipboardPreferences, ClipboardRead, TextClipboard, TextClipboardTarget,
    };
    use crate::terminal::osc52::{Osc52Target, Osc52Terminator};
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct RecordingClipboard {
        reads: Cell<usize>,
        resolutions: Cell<usize>,
        writes: RefCell<Vec<String>>,
        text: RefCell<Option<String>>,
        unavailable: Cell<bool>,
    }
    impl TextClipboard for RecordingClipboard {
        fn resolve(&self, _: Osc52Target) -> TextClipboardTarget {
            self.resolutions.set(self.resolutions.get() + 1);
            TextClipboardTarget::Clipboard
        }
        fn read(&self, _: TextClipboardTarget, _: &mut gpui::App) -> ClipboardRead<Option<String>> {
            self.reads.set(self.reads.get() + 1);
            Box::pin(std::future::ready(if self.unavailable.get() {
                Err(ClipboardError::Unavailable)
            } else {
                Ok(self.text.borrow().clone())
            }))
        }
        fn write(
            &self,
            _: TextClipboardTarget,
            text: &str,
            _: &mut gpui::App,
        ) -> Result<(), ClipboardError> {
            self.writes.borrow_mut().push(text.to_owned());
            if self.unavailable.get() {
                Err(ClipboardError::Unavailable)
            } else {
                Ok(())
            }
        }
    }
    /// Starts a request's native effect and runs it until the test clipboard answers.
    fn perform(
        request: ClipboardRequest,
        preferences: ClipboardPreferences,
        clipboard: &dyn TextClipboard,
        focused: bool,
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| request.perform(preferences, clipboard, focused, cx))
            .detach();
        cx.run_until_parked();
    }

    fn read() -> Osc52Operation {
        Osc52Operation::Read {
            target: Osc52Target::Primary,
            terminator: Osc52Terminator::StringTerminator,
        }
    }
    fn connected() -> (
        WorkerClipboard,
        async_channel::Receiver<ClipboardRequest>,
        mpsc::Receiver<Command>,
    ) {
        let (sender, commands) = mpsc::channel();
        let (worker, requests) = WorkerClipboard::connect(sender);
        worker.authority.client_focus(Some(true));
        (worker, requests, commands)
    }
    fn complete(
        worker: &mut WorkerClipboard,
        commands: &mpsc::Receiver<Command>,
    ) -> Option<Vec<u8>> {
        let Command::CompleteClipboard(id, completion) = commands.try_recv().unwrap() else {
            panic!("clipboard completion expected");
        };
        worker.complete(Some(id), completion).1
    }

    struct IndependentClipboard {
        clipboard: RefCell<String>,
        primary: RefCell<String>,
        primary_available: bool,
        accesses: RefCell<Vec<TextClipboardTarget>>,
    }
    impl TextClipboard for IndependentClipboard {
        fn resolve(&self, target: Osc52Target) -> TextClipboardTarget {
            match target {
                Osc52Target::Default | Osc52Target::Standard => TextClipboardTarget::Clipboard,
                Osc52Target::Primary | Osc52Target::Selection => TextClipboardTarget::Primary,
            }
        }
        fn read(
            &self,
            target: TextClipboardTarget,
            _: &mut gpui::App,
        ) -> ClipboardRead<Option<String>> {
            self.accesses.borrow_mut().push(target);
            Box::pin(std::future::ready(match target {
                TextClipboardTarget::Clipboard => Ok(Some(self.clipboard.borrow().clone())),
                TextClipboardTarget::Primary if self.primary_available => {
                    Ok(Some(self.primary.borrow().clone()))
                }
                TextClipboardTarget::Primary => Err(ClipboardError::Unavailable),
            }))
        }
        fn write(
            &self,
            target: TextClipboardTarget,
            text: &str,
            _: &mut gpui::App,
        ) -> Result<(), ClipboardError> {
            self.accesses.borrow_mut().push(target);
            match target {
                TextClipboardTarget::Clipboard => *self.clipboard.borrow_mut() = text.to_owned(),
                TextClipboardTarget::Primary if self.primary_available => {
                    *self.primary.borrow_mut() = text.to_owned()
                }
                TextClipboardTarget::Primary => return Err(ClipboardError::Unavailable),
            }
            Ok(())
        }
    }

    #[gpui::test]
    fn clipboard_requests_use_injected_target_resolution_without_unavailable_selection_fallback(
        cx: &mut gpui::TestAppContext,
    ) {
        for primary_available in [true, false] {
            for (target, selector, selected, encoded) in [
                (
                    Osc52Target::Default,
                    "",
                    TextClipboardTarget::Clipboard,
                    "Y2xpcGJvYXJk",
                ),
                (
                    Osc52Target::Standard,
                    "c",
                    TextClipboardTarget::Clipboard,
                    "Y2xpcGJvYXJk",
                ),
                (
                    Osc52Target::Primary,
                    "p",
                    TextClipboardTarget::Primary,
                    "cHJpbWFyeQ==",
                ),
                (
                    Osc52Target::Selection,
                    "s",
                    TextClipboardTarget::Primary,
                    "cHJpbWFyeQ==",
                ),
            ] {
                for terminator in [Osc52Terminator::Bell, Osc52Terminator::StringTerminator] {
                    let (mut worker, requests, commands) = connected();
                    let clipboard = IndependentClipboard {
                        clipboard: RefCell::new("clipboard".into()),
                        primary: RefCell::new("primary".into()),
                        primary_available,
                        accesses: RefCell::new(Vec::new()),
                    };
                    let preferences = ClipboardPreferences {
                        allow_write: true,
                        allow_read: true,
                    };
                    worker.begin(
                        Osc52Operation::Read { target, terminator },
                        worker.authority.grant(),
                    );
                    perform(
                        requests.try_recv().unwrap(),
                        preferences,
                        &clipboard,
                        true,
                        cx,
                    );
                    let unavailable =
                        selected == TextClipboardTarget::Primary && !primary_available;
                    let encoded = if unavailable { "" } else { encoded };
                    let terminator = match terminator {
                        Osc52Terminator::Bell => "\x07",
                        Osc52Terminator::StringTerminator => "\x1b\\",
                    };
                    assert_eq!(
                        complete(&mut worker, &commands).unwrap(),
                        format!("\x1b]52;{selector};{encoded}{terminator}").as_bytes()
                    );
                    worker.begin(
                        Osc52Operation::Write {
                            target,
                            text: "replacement".into(),
                        },
                        worker.authority.grant(),
                    );
                    perform(
                        requests.try_recv().unwrap(),
                        preferences,
                        &clipboard,
                        true,
                        cx,
                    );
                    let Command::CompleteClipboard(id, completion) = commands.try_recv().unwrap()
                    else {
                        panic!("clipboard completion expected");
                    };
                    if unavailable {
                        assert!(matches!(completion, ClipboardCompletion::Denied));
                    } else {
                        assert!(matches!(completion, ClipboardCompletion::Written));
                    }
                    worker.complete(Some(id), completion);
                    assert_eq!(*clipboard.accesses.borrow(), [selected, selected]);
                    assert_eq!(
                        clipboard.clipboard.borrow().as_str(),
                        if selected == TextClipboardTarget::Clipboard {
                            "replacement"
                        } else {
                            "clipboard"
                        }
                    );
                    assert_eq!(
                        clipboard.primary.borrow().as_str(),
                        if selected == TextClipboardTarget::Primary && primary_available {
                            "replacement"
                        } else {
                            "primary"
                        }
                    );
                }
            }
        }
    }

    #[gpui::test]
    fn clipboard_policy_defaults_write_and_opt_in_reads(cx: &mut gpui::TestAppContext) {
        let (mut worker, requests, commands) = connected();
        let clipboard = RecordingClipboard::default();
        *clipboard.text.borrow_mut() = Some("secret".into());
        worker.begin(read(), worker.authority.grant());
        perform(
            requests.try_recv().unwrap(),
            ClipboardPreferences::default(),
            &clipboard,
            true,
            cx,
        );
        assert_eq!(clipboard.reads.get(), 0);
        assert_eq!(clipboard.resolutions.get(), 0);
        assert_eq!(
            complete(&mut worker, &commands).unwrap(),
            b"\x1b]52;p;\x1b\\"
        );

        worker.begin(
            Osc52Operation::Write {
                target: Osc52Target::Standard,
                text: "copied".into(),
            },
            worker.authority.grant(),
        );
        perform(
            requests.try_recv().unwrap(),
            ClipboardPreferences::default(),
            &clipboard,
            true,
            cx,
        );
        complete(&mut worker, &commands);
        assert_eq!(&*clipboard.writes.borrow(), &["copied"]);

        worker.begin(read(), worker.authority.grant());
        perform(
            requests.try_recv().unwrap(),
            ClipboardPreferences {
                allow_read: true,
                ..ClipboardPreferences::default()
            },
            &clipboard,
            true,
            cx,
        );
        assert_eq!(
            complete(&mut worker, &commands).unwrap(),
            b"\x1b]52;p;c2VjcmV0\x1b\\"
        );
    }

    #[gpui::test]
    fn clipboard_revocation_and_expiry_never_touch_native_clipboard(cx: &mut gpui::TestAppContext) {
        for case in 0..4 {
            let (mut worker, requests, commands) = connected();
            let clipboard = RecordingClipboard::default();
            worker.begin(
                Osc52Operation::Write {
                    target: Osc52Target::Standard,
                    text: "secret".into(),
                },
                worker.authority.grant(),
            );
            let mut request = requests.try_recv().unwrap();
            if case == 0 {
                worker.authority.client_focus(Some(false));
                worker.authority.client_focus(Some(true));
            } else if case == 1 {
                request.deadline = Instant::now();
            }
            let preferences = ClipboardPreferences {
                allow_write: case != 3,
                ..ClipboardPreferences::default()
            };
            perform(request, preferences, &clipboard, case != 2, cx);
            complete(&mut worker, &commands);
            assert!(clipboard.writes.borrow().is_empty());
            assert_eq!(clipboard.resolutions.get(), 0);
        }
    }

    #[gpui::test]
    fn clipboard_unavailable_and_oversized_reads_reply_empty(cx: &mut gpui::TestAppContext) {
        for unavailable in [true, false] {
            let (mut worker, requests, commands) = connected();
            let clipboard = RecordingClipboard::default();
            clipboard.unavailable.set(unavailable);
            *clipboard.text.borrow_mut() = Some("x".repeat(MAX_OSC52_CONTENT_BYTES + 1));
            worker.begin(read(), worker.authority.grant());
            perform(
                requests.try_recv().unwrap(),
                ClipboardPreferences {
                    allow_read: true,
                    ..ClipboardPreferences::default()
                },
                &clipboard,
                true,
                cx,
            );
            assert_eq!(
                complete(&mut worker, &commands).unwrap(),
                b"\x1b]52;p;\x1b\\"
            );
        }
    }

    /// A clipboard owner that answers each read only when the test does.
    #[derive(Default)]
    struct SlowClipboard {
        answers: RefCell<VecDeque<async_channel::Sender<Option<String>>>>,
    }
    impl SlowClipboard {
        /// Reports whether a pending read was still waiting for this answer.
        fn answer(&self, text: &str) -> bool {
            let sender = self.answers.borrow_mut().pop_front().unwrap();
            sender.try_send(Some(text.to_owned())).is_ok()
        }
    }
    impl TextClipboard for SlowClipboard {
        fn resolve(&self, _: Osc52Target) -> TextClipboardTarget {
            TextClipboardTarget::Clipboard
        }
        fn read(&self, _: TextClipboardTarget, _: &mut gpui::App) -> ClipboardRead<Option<String>> {
            let (sender, receiver) = async_channel::bounded(1);
            self.answers.borrow_mut().push_back(sender);
            Box::pin(async move {
                receiver
                    .recv()
                    .await
                    .map_err(|_| ClipboardError::Unavailable)
            })
        }
        fn write(
            &self,
            _: TextClipboardTarget,
            _: &str,
            _: &mut gpui::App,
        ) -> Result<(), ClipboardError> {
            Ok(())
        }
    }
    const ALLOW_READ: ClipboardPreferences = ClipboardPreferences {
        allow_write: true,
        allow_read: true,
    };

    #[gpui::test]
    fn slow_owner_reads_leave_the_ui_free_and_reply_when_answered(cx: &mut gpui::TestAppContext) {
        let (mut worker, requests, commands) = connected();
        let clipboard = SlowClipboard::default();
        worker.begin(read(), worker.authority.grant());
        perform(
            requests.try_recv().unwrap(),
            ALLOW_READ,
            &clipboard,
            true,
            cx,
        );
        // Performing returned while the owner has not answered, and nothing has replied yet.
        assert!(commands.try_recv().is_err());
        assert!(worker.pending());

        assert!(clipboard.answer("slow"));
        cx.run_until_parked();
        assert_eq!(
            complete(&mut worker, &commands).unwrap(),
            b"\x1b]52;p;c2xvdw==\x1b\\"
        );
    }

    #[gpui::test]
    fn reads_past_the_request_deadline_reply_empty_and_discard_late_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let (mut worker, requests, commands) = connected();
        let clipboard = SlowClipboard::default();
        worker.begin(read(), worker.authority.grant());
        perform(
            requests.try_recv().unwrap(),
            ALLOW_READ,
            &clipboard,
            true,
            cx,
        );
        cx.executor().advance_clock(REQUEST_TIMEOUT);
        cx.run_until_parked();
        let Command::CompleteClipboard(id, completion) = commands.try_recv().unwrap() else {
            panic!("clipboard completion expected");
        };
        assert!(matches!(completion, ClipboardCompletion::Denied));
        assert_eq!(
            worker.complete(Some(id), completion).1.unwrap(),
            b"\x1b]52;p;\x1b\\"
        );

        assert!(!clipboard.answer("late"), "the timed-out read is discarded");
        cx.run_until_parked();
        assert!(commands.try_recv().is_err());
    }

    #[gpui::test]
    fn reads_whose_owner_is_gone_are_discarded_and_reply_once(cx: &mut gpui::TestAppContext) {
        let (mut worker, requests, commands) = connected();
        let clipboard = SlowClipboard::default();
        worker.begin(read(), worker.authority.grant());
        let request = requests.try_recv().unwrap();
        let effect = cx.update(|cx| request.perform(ALLOW_READ, &clipboard, true, cx));
        // The terminal or Terminal Session that awaited the read closed.
        drop(effect);
        cx.run_until_parked();
        assert!(
            !clipboard.answer("late"),
            "the read is discarded with its owner"
        );
        assert_eq!(
            complete(&mut worker, &commands).unwrap(),
            b"\x1b]52;p;\x1b\\"
        );
        assert!(commands.try_recv().is_err());

        // A Terminal Session that closed first receives nothing at all.
        worker.begin(read(), worker.authority.grant());
        let request = requests.try_recv().unwrap();
        drop((worker, commands));
        perform(request, ALLOW_READ, &clipboard, true, cx);
        assert!(clipboard.answer("late"));
        cx.run_until_parked();
    }
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    use crate::terminal::osc52::{Osc52Target, Osc52Terminator};

    #[test]
    fn closed_subscription_revokes_pending_clipboard_without_waiting_for_worker() {
        let (sender, commands) = mpsc::channel();
        let (mut worker, requests) = WorkerClipboard::connect(sender);
        let (events, subscription) = async_channel::bounded(1);
        worker.authority.observe_subscription(events);
        worker.authority.client_focus(Some(true));
        worker.begin(
            Osc52Operation::Read {
                target: Osc52Target::Primary,
                terminator: Osc52Terminator::StringTerminator,
            },
            worker.authority.grant(),
        );
        let request = requests.try_recv().unwrap();
        assert!(request.remaining().is_some());

        drop(subscription);

        assert!(request.remaining().is_none());
        drop(request);
        let Command::CompleteClipboard(id, completion) = commands.try_recv().unwrap() else {
            panic!("clipboard completion expected");
        };
        assert_eq!(
            worker.complete(Some(id), completion).1.unwrap(),
            b"\x1b]52;p;\x1b\\"
        );
    }

    #[test]
    fn clipboard_timeout_and_dropped_requests_complete_once() {
        let (sender, commands) = mpsc::channel();
        let (mut worker, requests) = WorkerClipboard::connect(sender);
        worker.authority.client_focus(Some(true));
        worker.begin(
            Osc52Operation::Read {
                target: Osc52Target::Primary,
                terminator: Osc52Terminator::StringTerminator,
            },
            worker.authority.grant(),
        );
        let request = requests.try_recv().unwrap();
        worker.pending.as_mut().unwrap().deadline = Instant::now();
        assert!(worker.expired());
        assert_eq!(
            worker.complete(None, ClipboardCompletion::Denied),
            (true, Some(b"\x1b]52;p;\x1b\\".to_vec()))
        );
        drop(request);
        let Command::CompleteClipboard(id, completion) = commands.try_recv().unwrap() else {
            panic!("clipboard completion expected");
        };
        assert!(worker.complete(Some(id), completion).1.is_none());
        assert!(!worker.pending());
    }
}
