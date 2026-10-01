//! Orders clipboard effects without blocking the worker on the UI thread.
use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;

use super::*;
use crate::terminal::native_services::clipboard::{ClipboardPreferences, TextClipboard};
use crate::terminal::osc52::{MAX_OSC52_CONTENT_BYTES, Osc52Operation};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct ClipboardAuthority(AtomicU64);

impl ClipboardAuthority {
    pub(super) fn focus(&self, focused: bool) {
        // Increment on every transition, including a loss followed by regained focus.
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                (old & 1 != u64::from(focused))
                    .then_some((old.wrapping_add(2) & !1) | u64::from(focused))
            });
    }
    fn grant(&self) -> Option<u64> {
        let epoch = self.0.load(Ordering::Acquire);
        (epoch & 1 == 1).then_some(epoch)
    }
    fn permits(&self, epoch: u64) -> bool {
        self.grant() == Some(epoch)
    }
}

pub(crate) enum ClipboardCompletion {
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

pub(crate) struct ClipboardRequest {
    id: u64,
    operation: Osc52Operation,
    authority: Arc<ClipboardAuthority>,
    epoch: u64,
    deadline: Instant,
    commands: Option<CommandSender<Command>>,
}

impl fmt::Debug for ClipboardRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardRequest")
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

impl ClipboardRequest {
    /// Consults the native clipboard only while the originating focus grant is current.
    pub(crate) fn perform(
        mut self,
        preferences: ClipboardPreferences,
        clipboard: &dyn TextClipboard,
        focused: bool,
        cx: &mut gpui::App,
    ) {
        let completion = if !focused
            || !self.authority.permits(self.epoch)
            || Instant::now() >= self.deadline
        {
            ClipboardCompletion::Denied
        } else {
            match &self.operation {
                Osc52Operation::Write { text, .. } if preferences.allow_write => clipboard
                    .write(text, cx)
                    .map_or(ClipboardCompletion::Denied, |()| {
                        ClipboardCompletion::Written
                    }),
                Osc52Operation::Read { .. } if preferences.allow_read => match clipboard.read(cx) {
                    Ok(text)
                        if text
                            .as_ref()
                            .is_none_or(|text| text.len() <= MAX_OSC52_CONTENT_BYTES) =>
                    {
                        ClipboardCompletion::Text(text.unwrap_or_default())
                    }
                    _ => ClipboardCompletion::Denied,
                },
                _ => ClipboardCompletion::Denied,
            }
        };
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(Command::CompleteClipboard(self.id, completion));
        }
    }
}

impl Drop for ClipboardRequest {
    fn drop(&mut self) {
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(Command::CompleteClipboard(
                self.id,
                ClipboardCompletion::Denied,
            ));
        }
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
    pub(super) effects: VecDeque<Osc52Effect>,
    pub(super) deferred_readers: usize,
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

    pub(super) fn begin(&mut self, operation: Osc52Operation) -> Option<Vec<u8>> {
        let read = match &operation {
            Osc52Operation::Read { target, terminator } => Some((*target, *terminator)),
            Osc52Operation::Write { .. } => None,
        };
        let denied = || {
            read.map(|(target, terminator)| {
                crate::terminal::osc52::read_response(target, terminator, "")
            })
        };
        let (Some(requests), Some(commands), Some(epoch)) =
            (&self.requests, &self.commands, self.authority.grant())
        else {
            return denied();
        };
        self.next_id = self.next_id.wrapping_add(1);
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let request = ClipboardRequest {
            id: self.next_id,
            operation,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::native_services::clipboard::ClipboardError;
    use crate::terminal::osc52::{Osc52Target, Osc52Terminator};
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct RecordingClipboard {
        reads: Cell<usize>,
        writes: RefCell<Vec<String>>,
        text: RefCell<Option<String>>,
        unavailable: Cell<bool>,
    }
    impl TextClipboard for RecordingClipboard {
        fn read(&self, _: &mut gpui::App) -> Result<Option<String>, ClipboardError> {
            self.reads.set(self.reads.get() + 1);
            if self.unavailable.get() {
                Err(ClipboardError::Unavailable)
            } else {
                Ok(self.text.borrow().clone())
            }
        }
        fn write(&self, text: &str, _: &mut gpui::App) -> Result<(), ClipboardError> {
            self.writes.borrow_mut().push(text.to_owned());
            if self.unavailable.get() {
                Err(ClipboardError::Unavailable)
            } else {
                Ok(())
            }
        }
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
        CommandReceiver<Command>,
    ) {
        let (sender, commands) = mpsc::channel();
        let (worker, requests) = WorkerClipboard::connect(sender);
        worker.authority.focus(true);
        (worker, requests, commands)
    }
    fn complete(
        worker: &mut WorkerClipboard,
        commands: &CommandReceiver<Command>,
    ) -> Option<Vec<u8>> {
        let Command::CompleteClipboard(id, completion) = commands.try_recv().unwrap() else {
            panic!("clipboard completion expected");
        };
        worker.complete(Some(id), completion).1
    }

    #[gpui::test]
    fn clipboard_policy_defaults_write_and_opt_in_reads(cx: &mut gpui::TestAppContext) {
        let (mut worker, requests, commands) = connected();
        let clipboard = RecordingClipboard::default();
        *clipboard.text.borrow_mut() = Some("secret".into());
        worker.begin(read());
        cx.update(|cx| {
            requests.try_recv().unwrap().perform(
                ClipboardPreferences::default(),
                &clipboard,
                true,
                cx,
            )
        });
        assert_eq!(clipboard.reads.get(), 0);
        assert_eq!(
            complete(&mut worker, &commands).unwrap(),
            b"\x1b]52;p;\x1b\\"
        );

        worker.begin(Osc52Operation::Write {
            target: Osc52Target::Standard,
            text: "copied".into(),
        });
        cx.update(|cx| {
            requests.try_recv().unwrap().perform(
                ClipboardPreferences::default(),
                &clipboard,
                true,
                cx,
            )
        });
        complete(&mut worker, &commands);
        assert_eq!(&*clipboard.writes.borrow(), &["copied"]);

        worker.begin(read());
        cx.update(|cx| {
            requests.try_recv().unwrap().perform(
                ClipboardPreferences {
                    allow_read: true,
                    ..ClipboardPreferences::default()
                },
                &clipboard,
                true,
                cx,
            )
        });
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
            worker.begin(Osc52Operation::Write {
                target: Osc52Target::Standard,
                text: "secret".into(),
            });
            let mut request = requests.try_recv().unwrap();
            if case == 0 {
                worker.authority.focus(false);
                worker.authority.focus(true);
            } else if case == 1 {
                request.deadline = Instant::now();
            }
            let preferences = ClipboardPreferences {
                allow_write: case != 3,
                ..ClipboardPreferences::default()
            };
            cx.update(|cx| request.perform(preferences, &clipboard, case != 2, cx));
            complete(&mut worker, &commands);
            assert!(clipboard.writes.borrow().is_empty());
        }
    }

    #[gpui::test]
    fn clipboard_unavailable_and_oversized_reads_reply_empty(cx: &mut gpui::TestAppContext) {
        for unavailable in [true, false] {
            let (mut worker, requests, commands) = connected();
            let clipboard = RecordingClipboard::default();
            clipboard.unavailable.set(unavailable);
            *clipboard.text.borrow_mut() = Some("x".repeat(MAX_OSC52_CONTENT_BYTES + 1));
            worker.begin(read());
            cx.update(|cx| {
                requests.try_recv().unwrap().perform(
                    ClipboardPreferences {
                        allow_read: true,
                        ..ClipboardPreferences::default()
                    },
                    &clipboard,
                    true,
                    cx,
                )
            });
            assert_eq!(
                complete(&mut worker, &commands).unwrap(),
                b"\x1b]52;p;\x1b\\"
            );
        }
    }

    #[test]
    fn clipboard_timeout_and_dropped_requests_complete_once() {
        let (mut worker, requests, commands) = connected();
        worker.begin(read());
        let request = requests.try_recv().unwrap();
        worker.pending.as_mut().unwrap().deadline = Instant::now();
        assert!(worker.expired());
        assert_eq!(
            worker.complete(None, ClipboardCompletion::Denied),
            (true, Some(b"\x1b]52;p;\x1b\\".to_vec()))
        );
        drop(request);
        assert!(complete(&mut worker, &commands).is_none());
        assert!(!worker.pending());
    }
}
