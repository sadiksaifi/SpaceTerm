//! Owns live keymap replacement, presentation hints, and application menu refreshes.

use std::rc::Rc;

use gpui::{App, BorrowAppContext, Global, Subscription, Task};

use super::{CUSTOMIZABLE_BINDINGS, KeybindingPreferences, KeymapProfile, ResolvedKeymap};
use crate::desktop_profile::DesktopPresentation;
use crate::platform::application_menu::ApplicationMenuAdapter;
use crate::settings::Settings;

pub(crate) struct KeymapRuntime {
    profile: Rc<KeymapProfile>,
    applied: KeybindingPreferences,
    menu: Option<Rc<dyn ApplicationMenuAdapter>>,
    task: Option<Task<()>>,
    _layout_subscription: Subscription,
    // Retain the insertion point when every customizable Command is Unassigned.
    segment_start: usize,
}
impl Global for KeymapRuntime {}

impl KeymapRuntime {
    pub(crate) fn profile(cx: &App) -> Rc<KeymapProfile> {
        Rc::clone(&cx.global::<Self>().profile)
    }
}

pub(crate) struct InstalledKeymap(Rc<ResolvedKeymap>);
impl Global for InstalledKeymap {}

impl InstalledKeymap {
    pub(crate) fn get(cx: &App) -> Rc<ResolvedKeymap> {
        Rc::clone(&cx.global::<Self>().0)
    }
}

/// Adopt the default segment already bound by `DesktopProfile::install`.
pub(crate) fn install(profile: KeymapProfile, cx: &mut App) {
    let applied = KeybindingPreferences::default();
    let segment_start = {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        keymap
            .bindings()
            .position(|binding| binding.meta() == Some(CUSTOMIZABLE_BINDINGS))
            .unwrap_or_else(|| {
                keymap.bindings().len()
                    - profile.control_bindings().len()
                    - profile.fixed_bindings().len()
            })
    };
    cx.set_global(InstalledKeymap(Rc::new(profile.resolve(&applied))));
    let layout_subscription = cx.on_keyboard_layout_change(refresh_layout);
    cx.set_global(KeymapRuntime {
        profile: Rc::new(profile),
        applied,
        menu: None,
        task: None,
        _layout_subscription: layout_subscription,
        segment_start,
    });
}

pub(crate) fn attach_application_menu(menu: Rc<dyn ApplicationMenuAdapter>, cx: &mut App) {
    cx.global_mut::<KeymapRuntime>().menu = Some(menu);
}

pub(crate) fn follow(settings: &Settings, cx: &mut App) {
    // Subscribe before reading so a concurrent settings change cannot be missed.
    let changed = settings.subscribe();
    apply(&settings.snapshot().candidate.keybindings, cx);
    let settings = settings.clone();
    let task = cx.spawn(async move |cx| {
        while changed.recv().await.is_ok() {
            while changed.try_recv().is_ok() {}
            cx.update(|cx| apply(&settings.snapshot().candidate.keybindings, cx));
        }
    });
    cx.global_mut::<KeymapRuntime>().task = Some(task);
}

fn refresh_layout(cx: &mut App) {
    let mut profile = (*KeymapRuntime::profile(cx)).clone();
    match profile.refresh_layout(cx.keyboard_layout()) {
        Ok(false) => return,
        Err(error) => {
            eprintln!("failed to refresh shortcuts: {error}");
            return;
        }
        Ok(true) => {}
    }
    let runtime = cx.global_mut::<KeymapRuntime>();
    runtime.profile = Rc::new(profile);
    let preferences = runtime.applied.clone();
    replace(&preferences, true, cx);
}

fn apply(preferences: &KeybindingPreferences, cx: &mut App) {
    if cx.global::<KeymapRuntime>().applied != *preferences {
        replace(preferences, false, cx);
    }
}

fn replace(preferences: &KeybindingPreferences, layout_changed: bool, cx: &mut App) {
    let runtime = cx.global::<KeymapRuntime>();
    let resolved = runtime.profile.resolve(preferences);
    let effective_changed = *InstalledKeymap::get(cx) != resolved;
    let menu = runtime.menu.clone();
    let controls = runtime.profile.control_bindings().to_vec();
    let fixed = runtime.profile.fixed_bindings().to_vec();
    let mut bindings = cx
        .key_bindings()
        .borrow()
        .bindings()
        .cloned()
        .collect::<Vec<_>>();
    let segment_start = bindings
        .iter()
        .position(|binding| binding.meta() == Some(CUSTOMIZABLE_BINDINGS))
        .unwrap_or(runtime.segment_start);
    bindings.retain(|binding| binding.meta() != Some(CUSTOMIZABLE_BINDINGS));
    bindings.splice(segment_start..segment_start, resolved.key_bindings());
    if layout_changed {
        for (tag, replacement) in [
            (super::keymap::CONTROL_BINDINGS, controls),
            (super::keymap::FIXED_BINDINGS, fixed),
        ] {
            if let Some(index) = bindings
                .iter()
                .position(|binding| binding.meta() == Some(tag))
            {
                bindings.retain(|binding| binding.meta() != Some(tag));
                bindings.splice(index..index, replacement);
            }
        }
    }
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
    cx.update_global::<DesktopPresentation, _>(|presentation, cx| presentation.refresh(cx));
    let runtime = cx.global_mut::<KeymapRuntime>();
    runtime.applied = preferences.clone();
    runtime.segment_start = segment_start;
    cx.set_global(InstalledKeymap(Rc::new(resolved)));
    if (effective_changed || layout_changed)
        && let Some(menu) = menu
        && let Err(error) = menu.install(cx)
    {
        eprintln!("failed to install the application menu: {error}");
    }
    cx.refresh_windows();
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
