//! What a cached view redraws for besides its own notifications.
//!
//! GPUI reuses a cached view's last frame until the view notifies. Replacing a global notifies no
//! view, so each cached view observes the globals its frame reads.

use gpui::Context;

use super::appearance::InstalledChrome;
use super::appearance_runtime::InstalledAppearance;
use crate::desktop_profile::DesktopPresentation;

/// Redraws the view when the appearance, its Chrome and control themes, or the desktop
/// presentation with its shortcut labels changes.
///
/// An appearance change replaces the Chrome and control themes before it installs the
/// appearance, so observing the appearance covers them. The Chrome is also observed because
/// it can be installed alone.
pub(crate) fn redraw_on_presentation_change<T: 'static>(cx: &mut Context<T>) {
    cx.observe_global::<InstalledAppearance>(|_, cx| cx.notify())
        .detach();
    cx.observe_global::<InstalledChrome>(|_, cx| cx.notify())
        .detach();
    cx.observe_global::<DesktopPresentation>(|_, cx| cx.notify())
        .detach();
}
