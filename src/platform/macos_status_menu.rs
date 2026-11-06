//! The client's menu bar extra: a native NSMenu modeled on Apple's first-party menu bar extras,
//! and its native alerts.
//!
//! The item lives in the client process beside its windows. Close All Windows leaves it in the
//! menu bar, and only its Quit, or the system's own termination, exits the client.
use super::macos_alert::{AlertActivation, AlertSeverity, application_alert, run_modal_alert};
use crate::status_menu::{
    self, Alert, StatusHost, StatusMenu,
    model::{Command, HEADER_TITLE, MachineKind, MachineRow, Menu, QUIT_TITLE, SETTINGS_TITLE},
};
use crate::updates::presentation::{ExtraItem, ExtraTile, Tone};
use block2::RcBlock;
use dispatch2::{DispatchQueue, MainThreadBound};
use gpui::App;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject, NSObjectProtocol},
    sel,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy, NSColor,
    NSEvent, NSEventMask, NSEventModifierFlags, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSMutableAttributedString, NSRange, NSString, ns_string};
use std::{
    cell::{Cell, RefCell},
    ptr::NonNull,
    rc::Rc,
};

type Sender = async_channel::Sender<Command>;

/// The section label over the Machine rows, as Bluetooth labels "Devices".
const MACHINES_TITLE: &str = "Machines";
const RECENT_TITLE: &str = "Recent Updates";

/// The brand's 16-point menu bar glyph, and the variant that marks the outer orbit when an item
/// needs attention. Both are templates, so AppKit tints them for the menu bar's appearance.
const GLYPH: &[u8] = include_bytes!("../../assets/brand/macos/menu-bar/SpaceTermTemplate.svg");
const ATTENTION_GLYPH: &[u8] =
    include_bytes!("../../assets/brand/macos/menu-bar/SpaceTermAttentionTemplate.svg");

thread_local! {
    /// Whether an alert is open. Status updates continue; another alert waits for this one.
    static ALERTING: Cell<bool> = const { Cell::new(false) };
}

/// Shows this build's menu bar extra for as long as the client runs.
pub(crate) fn install(
    cx: &mut App,
    _: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let (sender, messages) = async_channel::unbounded();
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
    let item = Rc::new(StatusItem::new(mtm, sender));
    let menu = StatusMenu::install(item, cx);
    cx.spawn(async move |cx| {
        while let Ok(command) = messages.recv().await {
            cx.update(|cx| menu.perform(command, cx));
        }
    })
    .detach();
}

struct StatusItem {
    mtm: MainThreadMarker,
    activation: AlertActivation,
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    target: Retained<MenuTarget>,
    glyph: Option<Retained<NSImage>>,
    attention_glyph: Option<Retained<NSImage>>,
}

/// Decodes one embedded SVG glyph as a template image at the size its document declares.
fn template_image(svg: &[u8]) -> Option<Retained<NSImage>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(svg))?;
    image.setTemplate(true);
    Some(image)
}

impl StatusItem {
    fn new(mtm: MainThreadMarker, sender: Sender) -> Self {
        let target = MenuTarget::new(mtm, sender);
        let menu = NSMenu::new(mtm);
        // Each row's availability follows the presentation, not AppKit's responder chain.
        menu.setAutoenablesItems(false);
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        item.setMenu(Some(&menu));
        Self {
            mtm,
            activation: AlertActivation::FORCEFUL,
            item,
            menu,
            target,
            glyph: template_image(GLYPH),
            attention_glyph: template_image(ATTENTION_GLYPH),
        }
    }
}

impl StatusHost for StatusItem {
    fn render(&self, presentation: &Menu) {
        let commands = Builder {
            mtm: self.mtm,
            target: &self.target,
            commands: Vec::new(),
        }
        .build(&self.menu, presentation);
        *self.target.ivars().commands.borrow_mut() = commands;
        let Some(button) = self.item.button(self.mtm) else {
            return;
        };
        let summary = NSString::from_str(&presentation.summary);
        let glyph = if presentation.attention {
            &self.attention_glyph
        } else {
            &self.glyph
        };
        if let Some(image) = glyph {
            image.setAccessibilityDescription(Some(&summary));
            button.setImage(Some(image));
        }
        button.setToolTip(Some(&summary));
    }

    fn alert(&self, alert: Alert, answered: Box<dyn FnOnce(bool) + Send>, cx: &mut App) {
        let alert = native_alert(&alert, self.mtm);
        present(alert, self.activation, cx.windows().is_empty(), answered);
    }
}

/// Builds the menu from one presentation and records the command each tagged row performs.
struct Builder<'a> {
    mtm: MainThreadMarker,
    target: &'a MenuTarget,
    commands: Vec<Command>,
}

impl Builder<'_> {
    fn build(mut self, menu: &NSMenu, presentation: &Menu) -> Vec<Command> {
        menu.removeAllItems();
        let header = &presentation.header;
        menu.addItem(
            &self.line_row(
                HEADER_TITLE,
                &header.line,
                header
                    .state
                    .as_deref()
                    .filter(|_| header.tone == Tone::Problem),
                true,
            ),
        );
        if !presentation.machines.is_empty() {
            menu.addItem(&NSMenuItem::separatorItem(self.mtm));
            menu.addItem(&NSMenuItem::sectionHeaderWithTitle(
                &NSString::from_str(MACHINES_TITLE),
                self.mtm,
            ));
            for machine in &presentation.machines {
                let item = self.machine_row(machine);
                menu.addItem(&item);
            }
        }
        if !presentation.recent.is_empty() {
            menu.addItem(&NSMenuItem::separatorItem(self.mtm));
            menu.addItem(&self.recent(&presentation.recent));
        }
        if !presentation.actions.is_empty() {
            menu.addItem(&NSMenuItem::separatorItem(self.mtm));
            self.items(menu, &presentation.actions);
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
        menu.addItem(&self.action(SETTINGS_TITLE, None, Command::Settings));
        let quit = self.action(QUIT_TITLE, None, Command::Quit);
        quit.setKeyEquivalent(ns_string!("q"));
        quit.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
        menu.addItem(&quit);
        self.commands
    }

    fn items(&mut self, menu: &NSMenu, items: &[ExtraItem]) {
        for item in items {
            match item {
                ExtraItem::Action {
                    title,
                    subtitle,
                    command,
                } => menu.addItem(&self.action(
                    title,
                    subtitle.as_deref(),
                    Command::Update(command.clone()),
                )),
                ExtraItem::Separator => menu.addItem(&NSMenuItem::separatorItem(self.mtm)),
            }
        }
    }

    /// A row that performs `command` through the target, by its tag.
    fn action(
        &mut self,
        title: &str,
        subtitle: Option<&str>,
        command: Command,
    ) -> Retained<NSMenuItem> {
        // SAFETY: The selector names a method MenuTarget implements, and the status item retains
        // the target for as long as the row exists.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(title),
                Some(sel!(choose:)),
                ns_string!(""),
            )
        };
        // SAFETY: The target is a live object that responds to the row's action.
        unsafe { item.setTarget(Some(self.target)) };
        item.setSubtitle(subtitle.map(NSString::from_str).as_deref());
        item.setTag(isize::try_from(self.commands.len()).unwrap_or(isize::MAX));
        self.commands.push(command);
        item
    }

    /// A row that reads but performs nothing.
    fn text(&self, text: &str) -> Retained<NSMenuItem> {
        let item = NSMenuItem::new(self.mtm);
        item.setTitle(&NSString::from_str(text));
        item.setEnabled(false);
        item
    }

    /// A title over its status line. `warning` is the state word drawn in orange.
    fn line_row(
        &self,
        title: &str,
        line: &str,
        warning: Option<&str>,
        bold: bool,
    ) -> Retained<NSMenuItem> {
        let item = NSMenuItem::new(self.mtm);
        item.setTitle(&NSString::from_str(title));
        item.setAttributedTitle(Some(&attributed_line(title, line, warning, bold)));
        item
    }

    fn machine_row(&mut self, machine: &MachineRow) -> Retained<NSMenuItem> {
        let row = &machine.row;
        let warning = (row.tone == Tone::Problem).then_some(row.state.as_ref());
        let item = self.line_row(&row.name, &row.line, warning, false);
        item.setImage(symbol(machine).as_deref());
        item.setToolTip(Some(&NSString::from_str(&row.sentence)));
        // An offline Machine offers nothing, so its row dims.
        item.setEnabled(row.tile != ExtraTile::Offline);
        if let Some(items) = &row.items {
            let menu =
                NSMenu::initWithTitle(NSMenu::alloc(self.mtm), &NSString::from_str(&row.name));
            menu.setAutoenablesItems(false);
            if let Some(info) = &row.info {
                menu.addItem(&self.text(info));
                menu.addItem(&NSMenuItem::separatorItem(self.mtm));
            }
            self.items(&menu, items);
            item.setSubmenu(Some(&menu));
        }
        item
    }

    fn recent(&self, recent: &[String]) -> Retained<NSMenuItem> {
        let item = NSMenuItem::new(self.mtm);
        item.setTitle(&NSString::from_str(RECENT_TITLE));
        let menu =
            NSMenu::initWithTitle(NSMenu::alloc(self.mtm), &NSString::from_str(RECENT_TITLE));
        menu.setAutoenablesItems(false);
        for line in recent {
            menu.addItem(&self.text(line));
        }
        item.setSubmenu(Some(&menu));
        item
    }
}

/// `title` in the menu font, then `line` in the smaller secondary style. A warning colors only
/// its state word, which ends the line.
fn attributed_line(
    title: &str,
    line: &str,
    warning: Option<&str>,
    bold: bool,
) -> Retained<NSMutableAttributedString> {
    let string = NSMutableAttributedString::initWithString(
        NSMutableAttributedString::alloc(),
        &NSString::from_str(&format!("{title}\n{line}")),
    );
    let utf16 = |text: &str| text.encode_utf16().count();
    let title_range = NSRange::new(0, utf16(title));
    let line_start = utf16(title) + 1;
    let line_range = NSRange::new(line_start, utf16(line));
    let size = NSFont::systemFontSize();
    let title_font = if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::menuFontOfSize(size)
    };
    let line_font = NSFont::systemFontOfSize(NSFont::smallSystemFontSize());
    // SAFETY: Each key is AppKit's constant and each value has the type its key expects. Every
    // range lies inside this live string.
    unsafe {
        string.addAttribute_value_range(NSFontAttributeName, &title_font, title_range);
        string.addAttribute_value_range(NSFontAttributeName, &line_font, line_range);
        string.addAttribute_value_range(
            NSForegroundColorAttributeName,
            &NSColor::secondaryLabelColor(),
            line_range,
        );
        if let Some(state) = warning.filter(|state| line.ends_with(state)) {
            let state_range = NSRange::new(line_start + utf16(line) - utf16(state), utf16(state));
            string.addAttribute_value_range(
                NSForegroundColorAttributeName,
                &NSColor::systemOrangeColor(),
                state_range,
            );
        }
    }
    string
}

/// The SF Symbol for a Machine row: a spinner while busy, otherwise its kind of computer.
fn symbol(machine: &MachineRow) -> Option<Retained<NSImage>> {
    let name = if machine.row.tile == ExtraTile::Busy {
        ns_string!("progress.indicator")
    } else {
        match machine.kind {
            MachineKind::Local => ns_string!("laptopcomputer"),
            MachineKind::Mac => ns_string!("macmini"),
            MachineKind::Linux => ns_string!("server.rack"),
        }
    };
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(name, None)?;
    image.setTemplate(true);
    Some(image)
}

/// Builds the alert for `alert`. A confirmation offers Cancel, then its destructive button; a
/// report offers OK.
fn native_alert(alert: &Alert, mtm: MainThreadMarker) -> Retained<NSAlert> {
    // Restart and Stop, the only confirmations, close Terminal Sessions and the programs in
    // them. A failure report loses nothing more.
    let native = application_alert(
        if alert.confirm.is_some() {
            AlertSeverity::Caution
        } else {
            AlertSeverity::Standard
        },
        mtm,
    );
    native.setMessageText(&NSString::from_str(&alert.title));
    native.setInformativeText(&NSString::from_str(&alert.message));
    match &alert.confirm {
        Some(label) => {
            native.addButtonWithTitle(ns_string!("Cancel"));
            native
                .addButtonWithTitle(&NSString::from_str(label))
                .setHasDestructiveAction(true);
        }
        None => {
            native.addButtonWithTitle(ns_string!("OK"));
        }
    }
    native
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
        let response = run_modal_alert(alert, activation, bind_alert_keys);
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
fn bind_alert_keys(alert: &NSAlert) {
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
    /// The command each tagged row performs, from the latest render.
    commands: RefCell<Vec<Command>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and define_class! drops the ivars.
    #[unsafe(super(NSObject))]
    #[name = "SpaceTermStatusMenuTarget"]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(choose:))]
        fn choose(&self, sender: &NSMenuItem) {
            let command = usize::try_from(sender.tag())
                .ok()
                .and_then(|tag| self.ivars().commands.borrow().get(tag).cloned());
            if let Some(command) = command {
                self.send(command);
            }
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, sender: Sender) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars {
            sender,
            commands: RefCell::default(),
        });
        // SAFETY: NSObject's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Menu actions reach the application after the menu closes, so an alert never opens
    /// inside menu tracking.
    fn send(&self, command: Command) {
        let _ = self.ivars().sender.try_send(command);
    }
}

#[cfg(all(test, feature = "native-tests"))]
pub(in crate::platform) mod tests {
    use super::super::macos_alert::testing::{
        Answering, RECORDED_ACTIVATION, answer_inside_modal_loop, expected_keys,
        settle_pending_checks,
    };
    use super::*;
    use crate::updates::UpdateState;
    use crate::updates::machines::{MachineLink, MachineUpdate};
    use crate::updates::presentation::{DesktopFacts, Dismissed, Presentation};
    use spaceterm_core::protocol::capability::{Capabilities, Capability};
    use spaceterm_core::update::facts::{Platform, UpdateCheck, UpdateFacts, Version};

    /// AppKit decodes both embedded glyphs as 16-point templates, which the menu bar tints for
    /// its appearance. An undecodable glyph would leave the status item without an image.
    #[test]
    fn both_menu_bar_glyphs_decode_as_sixteen_point_templates() {
        for svg in [GLYPH, ATTENTION_GLYPH] {
            let image = template_image(svg).expect("AppKit decodes the brand glyph");
            assert!(image.isTemplate());
            let size = image.size();
            assert_eq!((size.width, size.height), (16.0, 16.0));
        }
    }

    /// Every row title at every depth, and the footer rows' commands and keys.
    fn titles(menu: &NSMenu, into: &mut Vec<String>) {
        for item in menu.itemArray().iter() {
            into.push(item.title().to_string());
            if let Some(submenu) = item.submenu() {
                titles(&submenu, into);
            }
        }
    }

    /// Builds the native menu for This Mac and penguin as the status item does, then reads the
    /// NSMenu back: Settings and Quit end it, Quit answers Command-Q, and no row opens a window.
    pub(in crate::platform) fn the_native_menu_ends_with_settings_and_quit_and_offers_no_open() {
        let mtm = MainThreadMarker::new().expect("native test must run on the main thread");
        let machine = |id: u8, name: &'static str| MachineUpdate {
            machine: [id; 32],
            name: name.into(),
            local: id == 1,
            link: MachineLink::Connected,
            facts: Some(UpdateFacts {
                revision: 1,
                platform: Platform::LinuxX86_64,
                running: Version::parse("0.9.0").unwrap(),
                pending: None,
                sessions: 2,
                operation: None,
                check: UpdateCheck::Never,
                skipped: None,
                last_failure: None,
            }),
            capabilities: Capabilities::fake(&Capability::ALL),
            push: None,
        };
        let machines = [machine(1, "This Mac"), machine(2, "penguin")];
        let update = Presentation::new(
            DesktopFacts {
                state: &UpdateState::UpToDate,
                pending: None,
                stage: Default::default(),
                notes_unread: false,
                running: env!("SPACETERM_VERSION"),
            },
            &machines,
            &Dismissed::new(),
            crate::updates::now(),
        );
        let presentation = Menu::new(&update, &machines, Vec::new());
        let (sender, _commands) = async_channel::unbounded();
        let target = MenuTarget::new(mtm, sender);
        let menu = NSMenu::new(mtm);
        let commands = Builder {
            mtm,
            target: &target,
            commands: Vec::new(),
        }
        .build(&menu, &presentation);

        let items = menu.itemArray();
        let count = items.count();
        let row = |index: usize| items.objectAtIndex(index);
        let (settings, quit) = (row(count - 2), row(count - 1));
        assert_eq!(settings.title().to_string(), "SpaceTerm Settings\u{2026}");
        assert_eq!(quit.title().to_string(), "Quit SpaceTerm");
        assert_eq!(quit.keyEquivalent().to_string(), "q");
        assert_eq!(
            quit.keyEquivalentModifierMask(),
            NSEventModifierFlags::Command
        );
        let command =
            |item: &NSMenuItem| commands.get(usize::try_from(item.tag()).unwrap()).cloned();
        assert_eq!(command(&settings), Some(Command::Settings));
        assert_eq!(command(&quit), Some(Command::Quit));

        let mut all = Vec::new();
        titles(&menu, &mut all);
        assert!(
            all.iter().any(|title| title.starts_with("This Mac")),
            "{all:?}"
        );
        assert!(
            !all.iter().any(|title| title.starts_with("Open")),
            "the menu bar extra never offers Open SpaceTerm: {all:?}"
        );
    }

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
            Some("Stop"),
            &[("Cancel", "\r"), ("Stop", "")],
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
            Some("Restart"),
            &[("Cancel", "\r"), ("Restart", "")],
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
    pub(in crate::platform) fn machine_alerts_take_the_keyboard_and_bind_their_keys() {
        let mtm = MainThreadMarker::new().expect("native test must run on the main thread");
        for (confirm, keys, focused, answers) in ANSWER_CASES {
            for &(answering, expected) in answers {
                let alert = native_alert(
                    &Alert {
                        title: "Restart SpaceTerm Core on penguin?".to_owned(),
                        message: "This will close your 3 running sessions.".to_owned(),
                        confirm: confirm.map(str::to_owned),
                    },
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
                    "{confirm:?} binds its keys inside its modal loop"
                );
                assert_eq!(
                    asking.left_open,
                    expected.is_none(),
                    "{confirm:?} {answering:?}"
                );
                assert_eq!(
                    approvals.try_recv().ok(),
                    Some(expected.unwrap_or(false)),
                    "{confirm:?} {answering:?}"
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
