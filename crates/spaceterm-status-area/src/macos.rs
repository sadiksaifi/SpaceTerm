//! The menu bar status item, its menu, and its alerts, through AppKit.
use crate::{
    StatusError,
    menu::{Command, Item, Presentation, State},
    worker::{Event, Request, Worker},
};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{
    MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy,
    NSEvent, NSEventMask, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSRunningApplication,
    NSStatusBar, NSStatusItem, NSVariableStatusItemLength, NSWorkspace,
};
use objc2_foundation::{NSBundle, NSString, NSURL, ns_string};
use spaceterm_server::{
    ServerError,
    service::control::{Action, Confirmation, Controller},
};
use std::{
    cell::{Cell, OnceCell},
    path::PathBuf,
    ptr::NonNull,
    rc::Rc,
};

thread_local! {
    static STATUS: OnceCell<Rc<StatusItem>> = const { OnceCell::new() };
}

/// Runs the status item until the person hides it or logs out.
pub fn run(controller: Controller) -> Result<(), StatusError> {
    let mtm = MainThreadMarker::new().ok_or(StatusError::Unavailable)?;
    let application = NSApplication::sharedApplication(mtm);
    if another_instance_runs() {
        return Ok(());
    }
    // An accessory app has a status item and alerts, but no Dock icon or menu bar menus.
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let name = controller.display_name().to_owned();
    let worker = Worker::spawn(controller, |event| {
        DispatchQueue::main().exec_async(move || {
            STATUS.with(|status| {
                if let Some(status) = status.get() {
                    status.handle(event);
                }
            });
        });
    })?;
    let status = StatusItem::new(mtm, name, desktop_bundle(), worker);
    status.render();
    status.worker.send(Request::Refresh);
    STATUS.with(|cell| {
        let _ = cell.set(status);
    });
    application.run();
    Ok(())
}

/// launchd starts one status item per account, but the nested app can also be opened by hand.
fn another_instance_runs() -> bool {
    let Some(identifier) = NSBundle::mainBundle().bundleIdentifier() else {
        return false;
    };
    let ours = std::process::id() as libc::pid_t;
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&identifier)
        .iter()
        .any(|application| application.processIdentifier() != ours)
}

/// The application that contains this status app, at `<X>.app/Contents/Library/LoginItems`.
fn desktop_bundle() -> Option<PathBuf> {
    let bundle = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
    let desktop = bundle
        .parent()
        .filter(|path| path.ends_with("Contents/Library/LoginItems"))?
        .parent()?
        .parent()?
        .parent()?;
    desktop
        .extension()
        .is_some_and(|extension| extension == "app")
        .then(|| desktop.to_owned())
}

struct StatusItem {
    mtm: MainThreadMarker,
    name: String,
    desktop: Option<PathBuf>,
    worker: Worker,
    state: Cell<State>,
    /// Whether an alert is open. Status updates continue; another alert waits for this one.
    alerting: Cell<bool>,
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
    hide: Retained<NSMenuItem>,
}

impl StatusItem {
    fn new(
        mtm: MainThreadMarker,
        name: String,
        desktop: Option<PathBuf>,
        worker: Worker,
    ) -> Rc<Self> {
        let target = MenuTarget::new(mtm);
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
            hide: row(sel!(hide:)),
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
        menu.addItem(&rows.hide);
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        item.setMenu(Some(&menu));
        Rc::new(Self {
            mtm,
            name,
            desktop,
            worker,
            state: Cell::new(State::default()),
            alerting: Cell::new(false),
            item,
            rows,
            _target: target,
            _menu: menu,
        })
    }

    fn render(&self) {
        let presentation = Presentation::of(&self.name, self.state.get(), self.desktop.is_some());
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
        apply(&rows.hide, &presentation.hide);
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

    fn perform(&self, command: Command) {
        match command {
            Command::Open => self.open(),
            Command::Start => self.begin(Command::Start, Request::Start),
            Command::Stop => self.worker.send(Request::Prepare(Action::Stop)),
            Command::Restart => self.worker.send(Request::Prepare(Action::Restart)),
            // The menu withholds Hide while an operation runs; a stale menu cannot bypass that.
            Command::Hide if self.state.get().working.is_none() => {
                NSApplication::sharedApplication(self.mtm).terminate(None)
            }
            Command::Hide => {}
        }
    }

    fn begin(&self, command: Command, request: Request) {
        let mut state = self.state.get();
        if state.working.is_some() {
            return;
        }
        state.working = Some(command);
        self.state.set(state);
        self.render();
        self.worker.send(request);
    }

    fn handle(&self, event: Event) {
        match event {
            Event::Status(status) => {
                let mut state = self.state.get();
                state.status = Some(status);
                self.state.set(state);
                self.render();
            }
            Event::Prepared(action, prepared) => {
                let command = match action {
                    Action::Stop => Command::Stop,
                    _ => Command::Restart,
                };
                match prepared {
                    Ok(confirmation) => {
                        if self.confirm(&confirmation) {
                            self.begin(command, Request::Confirm(command, confirmation));
                        }
                    }
                    Err(error) => self.report(command, error),
                }
            }
            Event::Finished(command, result) => {
                let mut state = self.state.get();
                state.working = None;
                if let Ok(status) = result {
                    state.status = Some(Ok(status));
                }
                self.state.set(state);
                self.render();
                if let Err(error) = result {
                    self.report(command, error);
                }
            }
        }
    }

    fn open(&self) {
        let Some(desktop) = &self.desktop else {
            return;
        };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&desktop.to_string_lossy()));
        // Opening the desktop never starts the server; the desktop's own launch keeps Stop.
        if !NSWorkspace::sharedWorkspace().openURL(&url) {
            self.alert(
                &format!("{} couldn't be opened.", self.name),
                "Try opening it from the Applications folder.",
                None,
            );
        }
    }

    /// Asks before an operation that ends every client's Terminal Sessions. Cancel is the
    /// default button, so Return never ends Sessions by accident.
    fn confirm(&self, confirmation: &Confirmation) -> bool {
        self.alert(
            &confirmation.message(),
            &confirmation.detail(),
            Some(confirmation.confirm_label()),
        )
    }

    fn report(&self, command: Command, error: ServerError) {
        let operation = match command {
            Command::Start => "start",
            Command::Stop => "stop",
            _ => "restart",
        };
        self.alert(
            &format!("The {} server couldn't {operation}.", self.name),
            &error.to_string(),
            None,
        );
    }

    /// Shows one alert at a time and reports whether the person chose `destructive`.
    fn alert(&self, message: &str, detail: &str, destructive: Option<&str>) -> bool {
        if self.alerting.replace(true) {
            return false;
        }
        let application = NSApplication::sharedApplication(self.mtm);
        let alert = NSAlert::new(self.mtm);
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
        // Returns focus to the app the person was using.
        application.hide(None);
        self.alerting.set(false);
        chosen
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

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "SpaceTermServerStatusTarget"]
    #[thread_kind = MainThreadOnly]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(open:))]
        fn open(&self, _sender: &NSObject) {
            perform(Command::Open);
        }

        #[unsafe(method(start:))]
        fn start(&self, _sender: &NSObject) {
            perform(Command::Start);
        }

        #[unsafe(method(stop:))]
        fn stop(&self, _sender: &NSObject) {
            perform(Command::Stop);
        }

        #[unsafe(method(restart:))]
        fn restart(&self, _sender: &NSObject) {
            perform(Command::Restart);
        }

        #[unsafe(method(hide:))]
        fn hide(&self, _sender: &NSObject) {
            perform(Command::Hide);
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}

    unsafe impl NSMenuDelegate for MenuTarget {
        /// Opening the menu reads the current state. Reading never changes the service.
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            STATUS.with(|status| {
                if let Some(status) = status.get() {
                    status.worker.send(Request::Refresh);
                }
            });
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// Menu actions run after the menu closes, so an alert never opens inside menu tracking.
fn perform(command: Command) {
    DispatchQueue::main().exec_async(move || {
        STATUS.with(|status| {
            if let Some(status) = status.get() {
                status.perform(command);
            }
        });
    });
}
