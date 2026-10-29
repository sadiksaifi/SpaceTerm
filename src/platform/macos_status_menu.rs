//! The client's menu bar status item, its menu, and its server alerts, through AppKit.
//!
//! The item lives in the client process beside its windows. Close All Windows leaves it in the
//! menu bar, and only its Quit, or the system's own termination, exits the client.
use super::macos_alert::{AlertActivation, AlertSeverity, application_alert, run_modal_alert};
use crate::status_menu::{
    self,
    model::{Command, Item, Presentation, State},
    worker::{Event, Request, Worker},
};
use block2::RcBlock;
use dispatch2::{DispatchQueue, MainThreadBound};
use gpui::App;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSEvent,
    NSEventMask, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSString, ns_string};
use spaceterm_server::{
    ServerError,
    service::{
        Service,
        control::{Action, Confirmation, Controller},
    },
};
use std::{cell::Cell, ptr::NonNull, rc::Rc};

/// Everything the item reacts to, delivered to the application's event loop.
enum Message {
    Worker(Event),
    Command(Command),
    /// The menu opened, so the item reads the server's state again.
    Refresh,
    /// The person answered a confirmation: the approval to perform, or none.
    Decided(Command, Option<Confirmation>),
}

type Sender = async_channel::Sender<Message>;

thread_local! {
    /// Whether an alert is open. Status updates continue; another alert waits for this one.
    static ALERTING: Cell<bool> = const { Cell::new(false) };
}

/// Shows this build's status item for as long as the client runs.
pub(crate) fn install(
    cx: &mut App,
    _: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
) {
    let identity = crate::application_identity::ApplicationIdentity::current();
    let Ok(service) = Service::for_identity(identity.directory_name()) else {
        return;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let offered = service.clone();
    let controller = Controller::new(service);
    let name = controller.display_name().to_owned();
    let (sender, messages) = async_channel::unbounded();
    let worker = {
        let sender = sender.clone();
        let update_offered = move || crate::ui::settings_window::update_offered(&offered);
        match Worker::spawn(controller, update_offered, move |event| {
            let _ = sender.send_blocking(Message::Worker(event));
        }) {
            Ok(worker) => worker,
            Err(_) => return,
        }
    };
    status_menu::set_dock(
        cx,
        Rc::new(move |visible| {
            // An accessory application keeps its status item but has no Dock icon.
            NSApplication::sharedApplication(mtm).setActivationPolicy(if visible {
                NSApplicationActivationPolicy::Regular
            } else {
                NSApplicationActivationPolicy::Accessory
            });
        }),
    );
    status_menu::set_available(cx, true);
    let item = StatusItem::new(mtm, name, worker, sender);
    item.render();
    item.worker.send(Request::Refresh);
    cx.spawn(async move |cx| {
        while let Ok(message) = messages.recv().await {
            cx.update(|cx| item.handle(message, cx));
        }
    })
    .detach();
}

struct StatusItem {
    mtm: MainThreadMarker,
    name: String,
    worker: Worker,
    sender: Sender,
    state: Cell<State>,
    activation: AlertActivation,
    item: Retained<NSStatusItem>,
    rows: Rows,
    // Retains the target and menu delegate for the item's lifetime.
    _target: Retained<MenuTarget>,
    _menu: Retained<NSMenu>,
}

struct Rows {
    header: Retained<NSMenuItem>,
    state: Retained<NSMenuItem>,
    open: Retained<NSMenuItem>,
    controls: [Retained<NSMenuItem>; 3],
    update: Retained<NSMenuItem>,
    quit: Retained<NSMenuItem>,
}

impl StatusItem {
    fn new(mtm: MainThreadMarker, name: String, worker: Worker, sender: Sender) -> Rc<Self> {
        let target = MenuTarget::new(mtm, sender.clone());
        let menu = NSMenu::new(mtm);
        // Each row's availability follows the server's state, not AppKit's responder chain.
        menu.setAutoenablesItems(false);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        let row = |selector| {
            // SAFETY: The selector names a method MenuTarget implements, and the menu retains
            // the target through `_target` for as long as the row exists.
            unsafe {
                let row = NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    ns_string!(""),
                    Some(selector),
                    ns_string!(""),
                );
                row.setTarget(Some(&target));
                row
            }
        };
        let rows = Rows {
            header: NSMenuItem::sectionHeaderWithTitle(ns_string!(""), mtm),
            state: {
                let state = NSMenuItem::new(mtm);
                state.setEnabled(false);
                state
            },
            open: row(sel!(open:)),
            controls: [row(sel!(start:)), row(sel!(stop:)), row(sel!(restart:))],
            update: row(sel!(update:)),
            quit: row(sel!(quit:)),
        };
        menu.addItem(&rows.header);
        menu.addItem(&rows.state);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&rows.open);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        for control in &rows.controls {
            menu.addItem(control);
        }
        menu.addItem(&rows.update);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&rows.quit);
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        item.setMenu(Some(&menu));
        Rc::new(Self {
            mtm,
            name,
            worker,
            sender,
            state: Cell::new(State::default()),
            activation: AlertActivation::FORCEFUL,
            item,
            rows,
            _target: target,
            _menu: menu,
        })
    }

    fn render(&self) {
        let presentation = Presentation::of(&self.name, self.state.get());
        let rows = &self.rows;
        rows.header
            .setTitle(&NSString::from_str(&presentation.server));
        rows.state.setTitle(&NSString::from_str(presentation.state));
        rows.state.setSubtitle(
            presentation
                .explanation
                .as_deref()
                .map(NSString::from_str)
                .as_deref(),
        );
        let apply = |row: &NSMenuItem, item: &Item| {
            row.setTitle(&NSString::from_str(&item.label));
            row.setEnabled(item.enabled);
        };
        apply(&rows.open, &presentation.open);
        for (row, item) in rows.controls.iter().zip(&presentation.controls) {
            apply(row, item);
        }
        rows.update.setHidden(presentation.update.is_none());
        if let Some(update) = &presentation.update {
            apply(&rows.update, update);
        }
        apply(&rows.quit, &presentation.quit);
        let Some(button) = self.item.button(self.mtm) else {
            return;
        };
        let summary = NSString::from_str(&presentation.summary);
        let symbol = if presentation.running {
            ns_string!("terminal.fill")
        } else {
            ns_string!("terminal")
        };
        if let Some(image) =
            NSImage::imageWithSystemSymbolName_accessibilityDescription(symbol, Some(&summary))
        {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
        button.setToolTip(Some(&summary));
        // A dimmed icon tells a glance that the server is not running.
        button.setAppearsDisabled(!presentation.running);
    }

    fn handle(&self, message: Message, cx: &mut App) {
        match message {
            Message::Refresh => self.worker.send(Request::Refresh),
            Message::Command(command) => self.perform(command, cx),
            Message::Worker(Event::Status(status)) => {
                let mut state = self.state.get();
                state.status = Some(status);
                self.state.set(state);
                self.render();
            }
            Message::Worker(Event::Update(update)) => {
                let mut state = self.state.get();
                state.update = update;
                self.state.set(state);
                self.render();
            }
            Message::Worker(Event::Prepared(action, prepared)) => {
                let command = match action {
                    Action::Stop => Command::Stop,
                    _ => Command::Restart,
                };
                match prepared {
                    Ok(confirmation) => self.confirm(command, confirmation, cx),
                    Err(error) => self.report(command, error, cx),
                }
            }
            Message::Decided(command, Some(confirmation)) => {
                self.begin(command, Request::Confirm(command, confirmation), cx);
            }
            Message::Decided(_, None) => {}
            Message::Worker(Event::Finished(command, result)) => {
                let mut state = self.state.get();
                state.working = None;
                if let Ok(status) = result {
                    state.status = Some(Ok(status));
                }
                self.state.set(state);
                self.render();
                status_menu::end_operation(cx);
                match result {
                    Ok(status) => status_menu::operation_succeeded(cx, status),
                    Err(error) => self.report(command, error, cx),
                }
            }
        }
    }

    fn perform(&self, command: Command, cx: &mut App) {
        // A menu drawn before the state changed can still deliver a row it no longer offers.
        let presentation = Presentation::of(&self.name, self.state.get());
        let offered = std::iter::once(&presentation.open)
            .chain(&presentation.controls)
            .chain(&presentation.update)
            .chain(std::iter::once(&presentation.quit))
            .any(|item| item.command == command && item.enabled);
        if !offered {
            return;
        }
        match command {
            Command::Open => {
                crate::app::activate_default_window(cx, None);
                cx.activate(true);
            }
            Command::Start => self.begin(Command::Start, Request::Start, cx),
            Command::Stop => self.worker.send(Request::Prepare(Action::Stop)),
            Command::Restart => self.worker.send(Request::Prepare(Action::Restart)),
            // Settings owns the one journey, which reads the server again and asks first.
            Command::Update => {
                crate::app::leave_menu_only(cx);
                crate::ui::settings_window::open_server_update(cx);
                cx.activate(true);
            }
            // The client's own quit policy decides, including its confirmation and saving.
            Command::Quit => cx.dispatch_action(&crate::app::QuitApplication),
        }
    }

    /// Admits one server operation. Exiting the client waits until it finishes.
    fn begin(&self, command: Command, request: Request, cx: &mut App) {
        let mut state = self.state.get();
        if state.working.is_some() {
            return;
        }
        state.working = Some(command);
        self.state.set(state);
        self.render();
        status_menu::begin_operation(cx);
        self.worker.send(request);
    }

    /// Asks before an operation that ends every client's Terminal Sessions. Return chooses
    /// Cancel, so it never ends Sessions by accident.
    fn confirm(&self, command: Command, confirmation: Confirmation, cx: &App) {
        let sender = self.sender.clone();
        let alert = server_alert(
            &confirmation.message(),
            &confirmation.detail(),
            Some(confirmation.confirm_label()),
            self.mtm,
        );
        present(
            alert,
            self.activation,
            cx.windows().is_empty(),
            move |approved| {
                let _ = sender
                    .send_blocking(Message::Decided(command, approved.then_some(confirmation)));
            },
        );
    }

    fn report(&self, command: Command, error: ServerError, cx: &App) {
        let operation = match command {
            Command::Start => "start",
            Command::Stop => "stop",
            _ => "restart",
        };
        let message = format!("The {} server couldn\u{2019}t {operation}.", self.name);
        let alert = server_alert(&message, &error.to_string(), None, self.mtm);
        present(alert, self.activation, cx.windows().is_empty(), |_| {});
    }
}

/// Builds a server alert. A confirmation offers Cancel, then its `destructive` button; a report
/// offers OK.
fn server_alert(
    message: &str,
    detail: &str,
    destructive: Option<&str>,
    mtm: MainThreadMarker,
) -> Retained<NSAlert> {
    // Stop and Restart, the only confirmations, end every Terminal Session and the programs in
    // them. A failure report loses nothing more.
    let alert = application_alert(
        if destructive.is_some() {
            AlertSeverity::Caution
        } else {
            AlertSeverity::Standard
        },
        mtm,
    );
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(detail));
    match destructive {
        Some(label) => {
            alert.addButtonWithTitle(ns_string!("Cancel"));
            alert
                .addButtonWithTitle(&NSString::from_str(label))
                .setHasDestructiveAction(true);
        }
        None => {
            alert.addButtonWithTitle(ns_string!("OK"));
        }
    }
    alert
}

/// Shows `alert` one at a time and passes `answered` whether the person chose a confirmation's
/// destructive button. The alert runs its own modal loop, so it starts from the run loop rather
/// than inside the application's event handling. It takes the keyboard through `activation`
/// first. A windowless client hides afterwards, returning focus to the app the person was using.
fn present(
    alert: Retained<NSAlert>,
    activation: AlertActivation,
    windowless: bool,
    answered: impl FnOnce(bool) + Send + 'static,
) {
    let mtm = alert.mtm();
    let alert = MainThreadBound::new(alert, mtm);
    DispatchQueue::main().exec_async(move || {
        let Some(mtm) = MainThreadMarker::new() else {
            return answered(false);
        };
        if ALERTING.with(|alerting| alerting.replace(true)) {
            return answered(false);
        }
        let alert = alert.get(mtm);
        // A confirmation offers Cancel and its destructive button; a report offers only OK.
        let confirming = alert.buttons().count() > 1;
        let application = NSApplication::sharedApplication(mtm);
        let escape = confirming.then(cancel_on_escape).flatten();
        let response = run_modal_alert(alert, activation, bind_server_keys);
        if let Some(escape) = escape {
            // SAFETY: The monitor came from addLocalMonitorForEventsMatchingMask and is removed
            // once.
            unsafe { NSEvent::removeMonitor(&escape) };
        }
        if windowless {
            application.hide(None);
        }
        ALERTING.with(|alerting| alerting.set(false));
        answered(confirming && response == NSAlertFirstButtonReturn + 1);
    });
}

/// Return chooses the first button, Cancel or OK. A confirmation's destructive button has no
/// key, so it needs a deliberate click or Space.
fn bind_server_keys(alert: &NSAlert) {
    for (index, button) in alert.buttons().iter().enumerate() {
        button.setKeyEquivalent(if index == 0 {
            ns_string!("\r")
        } else {
            ns_string!("")
        });
    }
}

/// A button holds one key equivalent and Cancel holds Return, so Escape stops the modal alert
/// with Cancel's response through a monitor that lasts only while the alert is open.
fn cancel_on_escape() -> Option<Retained<AnyObject>> {
    const ESCAPE: u16 = 53;
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a live event for the duration of the call.
        let key = unsafe { event.as_ref() }.keyCode();
        match MainThreadMarker::new() {
            Some(mtm) if key == ESCAPE => {
                NSApplication::sharedApplication(mtm).stopModalWithCode(NSAlertFirstButtonReturn);
                std::ptr::null_mut()
            }
            _ => event.as_ptr(),
        }
    });
    // SAFETY: The handler returns the event it received or null to consume it.
    unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) }
}

struct MenuTargetIvars {
    sender: Sender,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and define_class! drops the sender ivar.
    #[unsafe(super(NSObject))]
    #[name = "SpaceTermStatusMenuTarget"]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(open:))]
        fn open(&self, _sender: &NSObject) {
            self.send(Message::Command(Command::Open));
        }

        #[unsafe(method(start:))]
        fn start(&self, _sender: &NSObject) {
            self.send(Message::Command(Command::Start));
        }

        #[unsafe(method(stop:))]
        fn stop(&self, _sender: &NSObject) {
            self.send(Message::Command(Command::Stop));
        }

        #[unsafe(method(restart:))]
        fn restart(&self, _sender: &NSObject) {
            self.send(Message::Command(Command::Restart));
        }

        #[unsafe(method(update:))]
        fn update(&self, _sender: &NSObject) {
            self.send(Message::Command(Command::Update));
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &NSObject) {
            self.send(Message::Command(Command::Quit));
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}

    unsafe impl NSMenuDelegate for MenuTarget {
        /// Opening the menu reads the current state. Reading never changes the service.
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.send(Message::Refresh);
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, sender: Sender) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { sender });
        // SAFETY: NSObject's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Menu actions reach the application after the menu closes, so an alert never opens
    /// inside menu tracking.
    fn send(&self, message: Message) {
        let _ = self.ivars().sender.try_send(message);
    }
}

#[cfg(all(test, feature = "native-tests"))]
pub(in crate::platform) mod tests {
    use super::super::macos_alert::testing::{
        Answering, RECORDED_ACTIVATION, answer_inside_modal_loop, expected_keys,
        settle_pending_checks,
    };
    use super::*;

    /// Each alert's button keys and focused button, and whether each answer approves it. Return
    /// chooses Cancel or OK, focus rests on it for Space, and Escape cancels a confirmation.
    /// `None` leaves the alert open.
    type AnswerCase = (
        Option<&'static str>,
        &'static [(&'static str, &'static str)],
        usize,
        &'static [(Answering, Option<bool>)],
    );

    const ANSWER_CASES: [AnswerCase; 3] = [
        (
            Some("Stop Server"),
            &[("Cancel", "\r"), ("Stop Server", "")],
            0,
            &[
                (Answering::Click(0), Some(false)),
                (Answering::Click(1), Some(true)),
                (Answering::Key("\r", 36), Some(false)),
                (Answering::Key(" ", 49), Some(false)),
                (Answering::PostedKey("\u{1b}", 53), Some(false)),
                (Answering::Stop, Some(false)),
            ],
        ),
        (
            Some("Restart Server"),
            &[("Cancel", "\r"), ("Restart Server", "")],
            0,
            &[
                (Answering::Click(0), Some(false)),
                (Answering::Click(1), Some(true)),
                (Answering::Key("\r", 36), Some(false)),
                (Answering::Key(" ", 49), Some(false)),
                (Answering::PostedKey("\u{1b}", 53), Some(false)),
                (Answering::Stop, Some(false)),
            ],
        ),
        (
            None,
            &[("OK", "\r")],
            0,
            &[
                (Answering::Click(0), Some(false)),
                (Answering::Key("\r", 36), Some(false)),
                (Answering::Stop, Some(false)),
            ],
        ),
    ];

    /// Presents the Stop and Restart confirmations and a failure report as the status item does:
    /// queued on the main queue, with the test driving the run loop until the alert returns. Each
    /// alert asks for the keyboard before its modal loop starts. The test records that request
    /// instead of activating, so the test process never takes the keyboard. Inside each alert's
    /// modal loop the test answers it by a click or a key. The alert stays transparent, so
    /// nothing appears on screen.
    pub(in crate::platform) fn server_alerts_take_the_keyboard_and_bind_their_keys() {
        let mtm = MainThreadMarker::new().expect("native test must run on the main thread");
        for (destructive, keys, focused, answers) in ANSWER_CASES {
            for &(answering, expected) in answers {
                let alert = server_alert(
                    "Stop the server?",
                    "Stopping ends every Terminal Session.",
                    destructive,
                    mtm,
                );
                alert.window().setAlphaValue(0.0);
                let (answered, approvals) = std::sync::mpsc::channel();
                let asking = answer_inside_modal_loop(&alert, answering, || {
                    present(alert.clone(), RECORDED_ACTIVATION, false, move |approved| {
                        let _ = answered.send(approved);
                    });
                });
                assert!(asking.modal, "the alert runs its own modal loop");
                assert_eq!(
                    asking.activations,
                    [true],
                    "the alert asks once for the keyboard, before its modal loop starts"
                );
                assert!(!asking.active, "asking never activates the test process");
                assert_eq!(
                    asking.laid_out,
                    (expected_keys(keys), Some(focused)),
                    "{destructive:?} binds its keys inside its modal loop"
                );
                assert_eq!(
                    asking.left_open,
                    expected.is_none(),
                    "{destructive:?} {answering:?}"
                );
                assert_eq!(
                    approvals.try_recv().ok(),
                    Some(expected.unwrap_or(false)),
                    "{destructive:?} {answering:?}"
                );
                assert!(
                    NSApplication::sharedApplication(mtm)
                        .modalWindow()
                        .is_none(),
                    "no modal loop remains"
                );
            }
        }
        settle_pending_checks();
    }
}
