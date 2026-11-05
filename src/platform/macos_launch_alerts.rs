//! The launch questions about retained data as AppKit alerts that run by themselves, before any
//! SpaceTerm window opens.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dispatch2::{DispatchQueue, MainThreadBound};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSModalResponseAbort, NSView,
};
use objc2_foundation::NSString;

use super::macos_alert::{AlertActivation, AlertSeverity, application_alert, run_modal_alert};
use crate::ui::data_reset::{LaunchAlerts, LaunchButtonRole, LaunchChoice, LaunchPrompt};

/// Runs each launch question as an application-modal `NSAlert`.
#[derive(Default)]
pub(crate) struct MacosLaunchAlerts {
    /// An alert's modal loop is running, so the application can't terminate until it ends.
    running: Arc<AtomicBool>,
}

impl LaunchAlerts for MacosLaunchAlerts {
    fn dismiss(&self, _: &mut gpui::App) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        if !self.running.load(Ordering::Acquire) {
            return false;
        }
        // The menu's Quit reaches this from inside the alert's modal loop, where stopModal ends
        // the loop without the exception abortModal raises through these Rust frames.
        // runModal then returns NSModalResponseAbort, which answers Quit.
        NSApplication::sharedApplication(mtm).stopModalWithCode(NSModalResponseAbort);
        true
    }

    fn ask(&self, prompt: LaunchPrompt, cx: &mut gpui::App) -> gpui::Task<LaunchChoice> {
        let (answered, answers) = async_channel::bounded(1);
        let task = cx.spawn(async move |_| answers.recv().await.unwrap_or(LaunchChoice::Quit));
        let Some(mtm) = MainThreadMarker::new() else {
            let _ = answered.try_send(LaunchChoice::Quit);
            return task;
        };
        let alert = MainThreadBound::new(launch_alert(&prompt, mtm), mtm);
        let running = Arc::clone(&self.running);
        // The alert runs its own modal loop outside GPUI's current dispatch.
        DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                let _ = answered.try_send(LaunchChoice::Quit);
                return;
            };
            let roles: Vec<_> = prompt.buttons.iter().map(|button| button.role).collect();
            running.store(true, Ordering::Release);
            let response =
                run_modal_alert(alert.get(mtm), AlertActivation::FORCEFUL, move |alert| {
                    bind_keys(alert, &roles);
                });
            running.store(false, Ordering::Release);
            let choice = usize::try_from(response - NSAlertFirstButtonReturn)
                .ok()
                .and_then(|index| prompt.buttons.get(index))
                .map_or(LaunchChoice::Quit, |button| button.choice);
            let _ = answered.try_send(choice);
        });
        task
    }
}

/// Builds the alert: the caution symbol for a choice that can lose data, the buttons in AppKit
/// order, a destructive Reset styled as one, and focus on the safe choice.
fn launch_alert(prompt: &LaunchPrompt, mtm: MainThreadMarker) -> Retained<NSAlert> {
    let alert = application_alert(
        if prompt.caution {
            AlertSeverity::Caution
        } else {
            AlertSeverity::Standard
        },
        mtm,
    );
    alert.setMessageText(&NSString::from_str(prompt.title));
    alert.setInformativeText(&NSString::from_str(&prompt.detail));
    let destructive = prompt
        .buttons
        .iter()
        .any(|button| button.role == LaunchButtonRole::Destructive);
    for button in &prompt.buttons {
        let added = alert.addButtonWithTitle(&NSString::from_str(button.label));
        if button.role == LaunchButtonRole::Destructive {
            added.setHasDestructiveAction(true);
        }
        let focused = if destructive {
            button.role == LaunchButtonRole::Cancel
        } else {
            button.role == LaunchButtonRole::Default
        };
        if focused {
            let focused: &NSView = &added;
            alert.window().setInitialFirstResponder(Some(focused));
        }
    }
    alert
}

/// Return chooses the default and Escape the Cancel. A destructive button takes no key, so
/// Return never resets data. AppKit's layout can clear the default's Return, so the keys are
/// bound once the alert runs.
fn bind_keys(alert: &NSAlert, roles: &[LaunchButtonRole]) {
    for (button, role) in alert.buttons().iter().zip(roles) {
        button.setKeyEquivalent(&NSString::from_str(match role {
            LaunchButtonRole::Default => "\r",
            LaunchButtonRole::Cancel => "\u{1b}",
            LaunchButtonRole::Destructive => "",
        }));
    }
}
