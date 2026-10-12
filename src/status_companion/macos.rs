//! The macOS menu bar item. AppKit objects stay on the main thread; the controller runs
//! on its own thread and hands over only the newest model and the companion's exit code.

use std::cell::RefCell;
use std::sync::Arc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem,
    NSRunningApplication, NSStatusBar, NSStatusItem, NSStatusItemBehavior,
    NSVariableStatusItemLength, NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_foundation::{NSBundle, NSObject, NSObjectProtocol, NSOperationQueue, NSString, NSURL};

use super::controller::{
    ActionIngress, CompanionAction, CompanionController, CompanionDependencies, CompanionFailure,
    DesktopActivation, StatusArea, StatusSource,
};
use super::model::{CompanionModel, MenuEntry};
use super::native::{ModelSlot, TokioTimer, UnavailableNativeStatus, enclosing_desktop_bundle};
use super::{CompanionIdentity, report};

struct MenuTargetIvars {
    actions: Arc<ActionIngress>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and define_class! drops the ivars.
    #[unsafe(super(NSObject))]
    #[name = "SpaceTermStatusMenuTarget"]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(openDesktop:))]
        fn open_desktop(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().actions.push(CompanionAction::OpenDesktop);
        }

        #[unsafe(method(retryConnection:))]
        fn retry_connection(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().actions.push(CompanionAction::RetryConnection);
        }

        #[unsafe(method(quitCompanion:))]
        fn quit_companion(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().actions.push(CompanionAction::Quit);
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, actions: Arc<ActionIngress>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { actions });
        // SAFETY: NSObject's init is its designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// The main thread's status item. Only main-queue blocks read it.
struct MenuBar {
    identity: CompanionIdentity,
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    target: Retained<MenuTarget>,
}

thread_local! {
    static MENU_BAR: RefCell<Option<MenuBar>> = const { RefCell::new(None) };
}

impl MenuBar {
    /// Adds the item. A status bar that gives the item no button cannot show it.
    fn install(
        mtm: MainThreadMarker,
        identity: CompanionIdentity,
        actions: Arc<ActionIngress>,
    ) -> Result<(), CompanionFailure> {
        let status_bar = NSStatusBar::systemStatusBar();
        let item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
        if item.button(mtm).is_none() {
            status_bar.removeStatusItem(&item);
            return Err(CompanionFailure::StatusAreaUnavailable);
        }
        item.setAutosaveName(Some(&NSString::from_str(&identity.application_id())));
        item.setBehavior(
            NSStatusItemBehavior::RemovalAllowed | NSStatusItemBehavior::TerminationOnRemoval,
        );
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        item.setMenu(Some(&menu));
        item.setVisible(true);
        let bar = Self {
            identity,
            item,
            menu,
            target: MenuTarget::new(mtm, actions),
        };
        bar.render(mtm, &CompanionModel::connecting());
        MENU_BAR.with_borrow_mut(|slot| *slot = Some(bar));
        Ok(())
    }

    /// Names the item by state in text and symbol shape, then rebuilds the menu: status
    /// lines as a section header and disabled items, then the commands with key equivalents.
    fn render(&self, mtm: MainThreadMarker, model: &CompanionModel) {
        let label = NSString::from_str(&model.accessibility_label(self.identity));
        if let Some(button) = self.item.button(mtm) {
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(model.symbol().macos()),
                Some(&label),
            );
            match image {
                Some(image) => {
                    image.setTemplate(true);
                    button.setImage(Some(&image));
                    button.setTitle(&NSString::new());
                }
                None => {
                    button.setImage(None);
                    button.setTitle(&NSString::from_str(model.state_text()));
                }
            }
            button.setToolTip(Some(&NSString::from_str(&model.tooltip(self.identity))));
            button.setAccessibilityLabel(Some(&label));
        }
        self.menu.removeAllItems();
        for entry in model.menu(self.identity) {
            let item = match entry {
                MenuEntry::Header { text, .. } => {
                    NSMenuItem::sectionHeaderWithTitle(&NSString::from_str(&text), mtm)
                }
                MenuEntry::Information { text, .. } => {
                    let item = NSMenuItem::new(mtm);
                    item.setTitle(&NSString::from_str(&text));
                    item.setEnabled(false);
                    item
                }
                MenuEntry::Separator { .. } => NSMenuItem::separatorItem(mtm),
                MenuEntry::Action {
                    action, title, key, ..
                } => {
                    let selector = match action {
                        CompanionAction::OpenDesktop => sel!(openDesktop:),
                        CompanionAction::RetryConnection => sel!(retryConnection:),
                        CompanionAction::Quit => sel!(quitCompanion:),
                    };
                    let target: &AnyObject = &self.target;
                    // SAFETY: the selector names a method MenuTarget defines with one object
                    // argument, and the target outlives the menu because MenuBar owns both.
                    let item = unsafe {
                        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                            NSMenuItem::alloc(mtm),
                            &NSString::from_str(&title),
                            Some(selector),
                            &NSString::from_str(&key.to_string()),
                        );
                        item.setTarget(Some(target));
                        item
                    };
                    item.setEnabled(true);
                    item
                }
            };
            self.menu.addItem(&item);
        }
    }
}

fn render_newest(slot: &ModelSlot) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(model) = slot.take() else {
        return;
    };
    MENU_BAR.with_borrow(|bar| {
        if let Some(bar) = bar {
            bar.render(mtm, &model);
        }
    });
}

/// Schedules `work` on the main queue.
fn on_main_queue(work: impl Fn() + Send + 'static) {
    let block = RcBlock::new(work);
    // SAFETY: the block may run on the main thread after this thread returns, so it
    // captures only Send data, and it touches AppKit only through a MainThreadMarker.
    unsafe { NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
}

/// The controller's view of the menu bar item.
struct MenuBarArea {
    slot: Arc<ModelSlot>,
    actions: Arc<ActionIngress>,
}

impl StatusArea for MenuBarArea {
    fn present(&mut self, model: &CompanionModel) -> Result<(), CompanionFailure> {
        if self.slot.replace(model.clone()) {
            let slot = self.slot.clone();
            on_main_queue(move || render_newest(&slot));
        }
        Ok(())
    }

    fn next_action(&mut self) -> Result<Option<CompanionAction>, CompanionFailure> {
        Ok(self.actions.pop())
    }
}

/// Opens the desktop bundle that contains this companion, or the installed application
/// registered for the desktop identifier. Neither path comes from a server.
struct WorkspaceDesktop {
    identity: CompanionIdentity,
}

impl WorkspaceDesktop {
    fn installation(&self) -> Option<String> {
        let expected = self.identity.desktop_id();
        let executable = std::env::current_exe().ok()?;
        let executable = std::fs::canonicalize(executable).ok()?;
        if let Some(bundle) = enclosing_desktop_bundle(&executable)
            && let Some(path) = bundle.to_str()
        {
            let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path), true);
            let identifier =
                NSBundle::bundleWithURL(&url).and_then(|bundle| bundle.bundleIdentifier());
            if identifier.is_some_and(|identifier| identifier.to_string() == expected) {
                return Some(path.to_owned());
            }
        }
        let url = NSWorkspace::sharedWorkspace()
            .URLForApplicationWithBundleIdentifier(&NSString::from_str(expected))?;
        url.path().map(|path| path.to_string())
    }
}

impl DesktopActivation for WorkspaceDesktop {
    fn open(&self) -> Result<(), CompanionFailure> {
        let path = self
            .installation()
            .ok_or(CompanionFailure::DesktopUnavailable)?;
        on_main_queue(move || {
            let url = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(&path), true);
            let configuration = NSWorkspaceOpenConfiguration::configuration();
            configuration.setActivates(true);
            NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(
                &url,
                &configuration,
                None,
            );
        });
        Ok(())
    }
}

fn another_instance_running(identity: CompanionIdentity) -> bool {
    let own = std::process::id();
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(
        &identity.application_id(),
    ))
    .iter()
    .any(|application| u32::try_from(application.processIdentifier()).is_ok_and(|pid| pid != own))
}

/// Exits from the main thread after removing the item, so the menu bar never shows a
/// companion that stopped answering.
fn finish(code: u8) {
    on_main_queue(move || {
        MENU_BAR.with_borrow_mut(|bar| {
            if let Some(bar) = bar.take() {
                NSStatusBar::systemStatusBar().removeStatusItem(&bar.item);
            }
        });
        std::process::exit(i32::from(code));
    });
}

pub(super) fn run(identity: CompanionIdentity) -> Result<(), CompanionFailure> {
    let mtm = MainThreadMarker::new().ok_or(CompanionFailure::MainThreadRequired)?;
    if another_instance_running(identity) {
        return Err(CompanionFailure::AlreadyRunning);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| CompanionFailure::RuntimeUnavailable)?;
    let application = NSApplication::sharedApplication(mtm);
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let actions = Arc::new(ActionIngress::new());
    MenuBar::install(mtm, identity, actions.clone())?;
    let dependencies = CompanionDependencies {
        status: StatusSource::Unbound,
        native: Arc::new(UnavailableNativeStatus),
        desktop: Arc::new(WorkspaceDesktop { identity }),
        area: Box::new(MenuBarArea {
            slot: Arc::new(ModelSlot::new()),
            actions: actions.clone(),
        }),
        actions,
        timer: Arc::new(TokioTimer),
    };
    std::thread::Builder::new()
        .name("status-companion".to_owned())
        .spawn(move || {
            let result = runtime.block_on(CompanionController::new(dependencies).run());
            let (code, message) = report(identity, result);
            if let Some(message) = message {
                eprintln!("{message}");
            }
            finish(code);
        })
        .map_err(|_| CompanionFailure::RuntimeUnavailable)?;
    application.run();
    Ok(())
}
