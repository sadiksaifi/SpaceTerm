//! The client's menu bar status item, its menu, and its server alerts, through AppKit.
//!
//! The item lives in the client process beside its windows. Close All Windows leaves it in the
//! menu bar, and only its Quit, or the system's own termination, exits the client.
use crate::status_menu::{
    self,
    model::{Command, Item, Presentation, State},
    worker::{Event, Request, Worker},
};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use gpui::App;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy,
    NSEvent, NSEventMask, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
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
    let controller = Controller::new(service);
    let name = controller.display_name().to_owned();
    let (sender, messages) = async_channel::unbounded();
    let worker = {
        let sender = sender.clone();
        match Worker::spawn(controller, move |event| {
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

    /// Asks before an operation that ends every client's Terminal Sessions. Cancel is the
    /// default button, so Return never ends Sessions by accident.
    fn confirm(&self, command: Command, confirmation: Confirmation, cx: &App) {
        let sender = self.sender.clone();
        let windowless = cx.windows().is_empty();
        let message = confirmation.message();
        let detail = confirmation.detail();
        let label = confirmation.confirm_label();
        // The alert runs its own modal loop, so it starts from the run loop rather than inside
        // the application's event handling.
        DispatchQueue::main().exec_async(move || {
            let approved = alert(&message, &detail, Some(label), windowless);
            let _ =
                sender.send_blocking(Message::Decided(command, approved.then_some(confirmation)));
        });
    }

    fn report(&self, command: Command, error: ServerError, cx: &App) {
        let operation = match command {
            Command::Start => "start",
            Command::Stop => "stop",
            _ => "restart",
        };
        let message = format!("The {} server couldn't {operation}.", self.name);
        let detail = error.to_string();
        let windowless = cx.windows().is_empty();
        DispatchQueue::main().exec_async(move || {
            alert(&message, &detail, None, windowless);
        });
    }
}

/// Shows one alert at a time and reports whether the person chose `destructive`. A windowless
/// client hides afterwards, returning focus to the app the person was using.
fn alert(message: &str, detail: &str, destructive: Option<&str>, windowless: bool) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    if ALERTING.with(|alerting| alerting.replace(true)) {
        return false;
    }
    let application = NSApplication::sharedApplication(mtm);
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(if destructive.is_some() {
        NSAlertStyle::Warning
    } else {
        NSAlertStyle::Informational
    });
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(detail));
    let chosen = match destructive {
        Some(label) => {
            let cancel = alert.addButtonWithTitle(ns_string!("Cancel"));
            // Return chooses Cancel. The destructive button needs a deliberate click or Space.
            cancel.setKeyEquivalent(ns_string!("\r"));
            let confirm = alert.addButtonWithTitle(&NSString::from_str(label));
            confirm.setHasDestructiveAction(true);
            confirm.setKeyEquivalent(ns_string!(""));
            application.activate();
            let escape = cancel_on_escape();
            let response = alert.runModal();
            if let Some(escape) = escape {
                // SAFETY: The monitor came from addLocalMonitorForEventsMatchingMask and is
                // removed once.
                unsafe { NSEvent::removeMonitor(&escape) };
            }
            response == NSAlertFirstButtonReturn + 1
        }
        None => {
            alert.addButtonWithTitle(ns_string!("OK"));
            application.activate();
            alert.runModal();
            false
        }
    };
    if windowless {
        application.hide(None);
    }
    ALERTING.with(|alerting| alerting.set(false));
    chosen
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
