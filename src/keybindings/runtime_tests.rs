use super::*;
use crate::desktop_profile::DesktopPresentation;
use crate::keybindings::{Command, Shortcut};
use crate::platform::application_menu::testing::RecordingApplicationMenuAdapter;
use crate::ui::CreateTab;
use gpui::{Context, IntoElement, KeyBinding, Render, TestAppContext, Window, div};
use std::cell::Cell;
use std::sync::Arc;

struct DispatchView;

impl Render for DispatchView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

#[gpui::test]
fn rebind_dispatches_the_new_shortcut_and_retires_the_old_one(cx: &mut TestAppContext) {
    let dispatches = Rc::new(Cell::new(0));
    let observed = Rc::clone(&dispatches);
    cx.update(|cx| {
        crate::ui::init(cx).unwrap();
        cx.on_action(move |_: &CreateTab, _| observed.set(observed.get() + 1));
    });
    let (_, cx) = cx.add_window_view(|_, _| DispatchView);
    cx.simulate_keystrokes("cmd-t");
    assert_eq!(dispatches.get(), 1);

    cx.update(|_, cx| {
        let mut preferences = KeybindingPreferences::default();
        KeymapRuntime::profile(cx)
            .assign(
                &mut preferences,
                Command::CreateTab,
                Some(Shortcut::parse("cmd-y").unwrap()),
            )
            .unwrap();
        apply(&preferences, cx);
    });
    cx.simulate_keystrokes("cmd-t");
    assert_eq!(dispatches.get(), 1);
    cx.simulate_keystrokes("cmd-y");
    assert_eq!(dispatches.get(), 2);
}

fn bindings(cx: &App) -> Vec<KeyBinding> {
    cx.key_bindings().borrow().bindings().cloned().collect()
}

fn assert_bindings_eq(actual: &[KeyBinding], expected: &[KeyBinding]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.keystrokes(), expected.keystrokes());
        assert_eq!(actual.predicate(), expected.predicate());
        assert_eq!(actual.meta(), expected.meta());
        assert_eq!(actual.action_input(), expected.action_input());
        assert!(actual.action().partial_eq(expected.action()));
    }
}

#[gpui::test]
fn replacement_preserves_untagged_positions_and_default_restoration(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.bind_keys([KeyBinding::new("cmd-y", crate::ui::NewWorkspace, None)]);
        crate::ui::init(cx).unwrap();
        // Controls can add bindings after the desktop profile, including the same chord.
        cx.bind_keys([KeyBinding::new(
            "cmd-y",
            crate::ui::CloseTab,
            Some("Control"),
        )]);
        let baseline = bindings(cx);
        let version = cx.key_bindings().borrow().version();
        let installed = InstalledKeymap::get(cx);
        apply(&KeybindingPreferences::default(), cx);
        assert_bindings_eq(&bindings(cx), &baseline);
        assert!(version == cx.key_bindings().borrow().version());
        assert!(Rc::ptr_eq(&installed, &InstalledKeymap::get(cx)));

        let preferences = serde_json::from_str(r#"{"create_tab":"cmd-y"}"#).unwrap();
        apply(&preferences, cx);
        let mut expected = baseline.clone();
        let index = expected
            .iter()
            .position(|binding| {
                binding.meta() == Some(CUSTOMIZABLE_BINDINGS)
                    && binding.action().partial_eq(&CreateTab)
            })
            .unwrap();
        expected[index] =
            KeyBinding::new("cmd-y", CreateTab, None).with_meta(CUSTOMIZABLE_BINDINGS);
        assert_bindings_eq(&bindings(cx), &expected);
        assert_eq!(
            DesktopPresentation::get(cx).shortcut(&CreateTab).as_deref(),
            Some("Primary+Y")
        );

        apply(&KeybindingPreferences::default(), cx);
        assert_bindings_eq(&bindings(cx), &baseline);
        assert_eq!(InstalledKeymap::get(cx), installed);
    });
}

#[gpui::test]
fn all_unassigned_commands_restore_at_the_original_segment_position(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::ui::init(cx).unwrap();
        cx.bind_keys([KeyBinding::new(
            "cmd-y",
            crate::app::QuitApplication,
            Some("Control"),
        )]);
        let baseline = bindings(cx);
        let mut preferences = KeybindingPreferences::default();
        for command in Command::ALL {
            preferences.set(command, None);
        }
        apply(&preferences, cx);
        let untagged = baseline
            .iter()
            .filter(|binding| binding.meta() != Some(CUSTOMIZABLE_BINDINGS))
            .cloned()
            .collect::<Vec<_>>();
        assert_bindings_eq(&bindings(cx), &untagged);
        for command in Command::ALL {
            assert_eq!(
                DesktopPresentation::get(cx).shortcut(command.action().as_ref()),
                None
            );
        }

        apply(&KeybindingPreferences::default(), cx);
        assert_bindings_eq(&bindings(cx), &baseline);
    });
}

#[gpui::test]
fn menu_reinstalls_only_when_the_resolved_keymap_changes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::ui::init(cx).unwrap();
        let menu = Rc::new(RecordingApplicationMenuAdapter::default());
        crate::app::init(cx, menu.clone(), Rc::new(
            crate::platform::application_quit::testing::RecordingApplicationQuitAdapter::default(),
        )).unwrap();
        assert_eq!(menu.installs(), 1);
        apply(&KeybindingPreferences::default(), cx);
        assert_eq!(menu.installs(), 1);

        // Close Workspace is already Unassigned by default.
        let same = serde_json::from_str(r#"{"close_workspace":null}"#).unwrap();
        apply(&same, cx);
        assert_eq!(menu.installs(), 1);

        let changed = serde_json::from_str(r#"{"create_tab":"cmd-y"}"#).unwrap();
        apply(&changed, cx);
        assert_eq!(menu.installs(), 2);
        apply(&changed, cx);
        assert_eq!(menu.installs(), 2);
        apply(&KeybindingPreferences::default(), cx);
        assert_eq!(menu.installs(), 3);
    });
}

fn settings() -> UserSettings {
    use crate::platform::app_directories::AppDirectoryEnvironment;
    use crate::platform::app_paths::{AppPathHostFacts, AppPaths};
    use crate::platform::testing::RecordingFilesystem;
    use crate::settings::storage::ConfigSettingsStorage;

    let paths = AppPaths::resolve(
        &AppDirectoryEnvironment {
            home: Some("/home/test".into()),
            ..Default::default()
        },
        &AppPathHostFacts::new("/runtime".into(), 200).unwrap(),
        Arc::new(RecordingFilesystem::default()),
    )
    .unwrap();
    UserSettings::load(Arc::new(ConfigSettingsStorage::new(Arc::new(paths))))
}

#[gpui::test]
fn follow_applies_the_current_candidate_immediately(cx: &mut TestAppContext) {
    let settings = settings();
    let token = settings.begin_preview(0).unwrap();
    let mut candidate = (*settings.snapshot().candidate).clone();
    candidate.keybindings = serde_json::from_str(r#"{"create_tab":"cmd-y"}"#).unwrap();
    settings.update_preview(&token, candidate).unwrap();
    cx.update(|cx| {
        crate::ui::init(cx).unwrap();
        follow(&settings, cx);
        assert_eq!(
            InstalledKeymap::get(cx).shortcut(Command::CreateTab),
            Some(&Shortcut::parse("cmd-y").unwrap())
        );
        assert_eq!(
            DesktopPresentation::get(cx).shortcut(&CreateTab).as_deref(),
            Some("Primary+Y")
        );
    });
}

#[gpui::test]
fn follow_applies_preview_changes_and_reverts_on_cancel_or_drop(cx: &mut TestAppContext) {
    let settings = settings();
    let baseline = cx.update(|cx| {
        crate::ui::init(cx).unwrap();
        follow(&settings, cx);
        bindings(cx)
    });
    let notifications = Rc::new(Cell::new(0));
    let observed = Rc::clone(&notifications);
    let _subscription = cx.update(|cx| {
        cx.observe_global::<InstalledKeymap>(move |cx| {
            observed.set(observed.get() + 1);
            let installed = InstalledKeymap::get(cx);
            let hint = DesktopPresentation::get(cx).shortcut(&CreateTab);
            assert_eq!(
                hint,
                installed
                    .shortcut(Command::CreateTab)
                    .map(|shortcut| DesktopPresentation::get(cx).format(shortcut))
            );
        })
    });
    cx.run_until_parked();
    let initial_notifications = notifications.get();
    for cancel_explicitly in [true, false] {
        let token = settings.begin_preview(0).unwrap();
        let mut candidate = (*settings.snapshot().candidate).clone();
        candidate.keybindings = serde_json::from_str(r#"{"create_tab":"cmd-y"}"#).unwrap();
        settings.update_preview(&token, candidate).unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                InstalledKeymap::get(cx).shortcut(Command::CreateTab),
                Some(&Shortcut::parse("cmd-y").unwrap())
            );
            assert_eq!(
                DesktopPresentation::get(cx).shortcut(&CreateTab).as_deref(),
                Some("Primary+Y")
            );
        });
        if cancel_explicitly {
            settings.cancel_preview(&token).unwrap();
        }
        drop(token);
        cx.run_until_parked();
        cx.update(|cx| assert_bindings_eq(&bindings(cx), &baseline));
    }
    assert_eq!(notifications.get() - initial_notifications, 4);
}

#[gpui::test]
fn following_another_settings_owner_retires_the_previous_subscription(cx: &mut TestAppContext) {
    let previous = settings();
    let current = settings();
    cx.update(|cx| {
        crate::ui::init(cx).unwrap();
        follow(&previous, cx);
        follow(&current, cx);
    });
    let token = previous.begin_preview(0).unwrap();
    let mut candidate = (*previous.snapshot().candidate).clone();
    candidate.keybindings = serde_json::from_str(r#"{"create_tab":"cmd-y"}"#).unwrap();
    previous.update_preview(&token, candidate).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            InstalledKeymap::get(cx).shortcut(Command::CreateTab),
            Some(&Shortcut::parse("cmd-t").unwrap())
        );
    });
}

#[derive(Debug)]
struct ChangingLayout(std::cell::RefCell<crate::platform::keyboard_layout::KeyboardLayout>);

impl crate::platform::keyboard_layout::KeyboardLayoutAdapter for ChangingLayout {
    fn snapshot(
        &self,
        _: &dyn gpui::PlatformKeyboardLayout,
    ) -> Result<
        crate::platform::keyboard_layout::KeyboardLayout,
        crate::platform::keyboard_layout::KeyboardLayoutUnavailable,
    > {
        Ok(self.0.borrow().clone())
    }
}

#[gpui::test]
fn layout_changes_refresh_dispatch_reservations_hints_and_menus_without_editing_settings(
    cx: &mut TestAppContext,
) {
    use crate::keybindings::{KeybindingState, SystemReservation, SystemReserved};
    let layout = Rc::new(ChangingLayout(std::cell::RefCell::new(
        crate::platform::keyboard_layout::testing::us()
            .snapshot(&crate::platform::keyboard_layout::testing::UnknownLayout)
            .unwrap(),
    )));
    let preferences: KeybindingPreferences =
        serde_json::from_str(r#"{"create_tab":"shift-cmd-7","new_workspace":"cmd-§"}"#).unwrap();
    let retained = serde_json::to_value(&preferences).unwrap();
    let menu = Rc::new(RecordingApplicationMenuAdapter::default());
    let dispatches = Rc::new(Cell::new(0));
    let observed = dispatches.clone();
    cx.update(|cx| {
        crate::ui::init(cx).unwrap();
        let mut profile = crate::desktop_profile::default_keymap::profile(
            layout.clone(),
            vec![SystemReserved {
                shortcut: Shortcut::parse("shift-cmd-3").unwrap(),
                reason: SystemReservation::Screenshot,
            }],
        )
        .unwrap();
        profile.refresh_layout(cx.keyboard_layout()).unwrap();
        install(profile, cx);
        attach_application_menu(menu.clone(), cx);
        apply(&preferences, cx);
        cx.on_action(move |_: &CreateTab, _| observed.set(observed.get() + 1));
    });
    let (_, cx) = cx.add_window_view(|_, _| DispatchView);
    cx.simulate_keystrokes("cmd-&");
    assert_eq!(dispatches.get(), 1);
    // Replace the host snapshot, preserving the retained spellings.
    let mut changed = crate::platform::keyboard_layout::KeyboardLayout::default();
    changed.insert(true, "7", "/");
    changed.insert(true, "3", "§");
    changed.insert(true, "=", "*");
    *layout.0.borrow_mut() = changed;
    cx.update(|_, cx| {
        refresh_layout(cx);
        let resolved = InstalledKeymap::get(cx);
        assert_eq!(
            resolved.state(Command::NewWorkspace),
            KeybindingState::Blocked(SystemReservation::Screenshot)
        );
        assert_eq!(
            KeymapRuntime::profile(cx).check(&Shortcut::parse("cmd-§").unwrap()),
            Err(crate::keybindings::Reservation::System(
                SystemReservation::Screenshot
            ))
        );
        assert_eq!(
            DesktopPresentation::get(cx).shortcut(&CreateTab).as_deref(),
            Some("Primary+/")
        );
        assert!(
            resolved
                .shortcuts(Command::IncreaseTerminalFontSize)
                .contains(&Shortcut::parse("cmd-*").unwrap())
        );
        assert_eq!(
            serde_json::to_value(&cx.global::<KeymapRuntime>().applied).unwrap(),
            retained
        );
        assert_eq!(menu.installs(), 2);
        refresh_layout(cx);
        assert_eq!(menu.installs(), 2);
    });
    cx.simulate_keystrokes("cmd-&");
    assert_eq!(dispatches.get(), 1);
    cx.simulate_keystrokes("cmd-/");
    assert_eq!(dispatches.get(), 2);
    *layout.0.borrow_mut() = crate::platform::keyboard_layout::testing::us()
        .snapshot(&crate::platform::keyboard_layout::testing::UnknownLayout)
        .unwrap();
    cx.update(|_, cx| {
        refresh_layout(cx);
        assert_eq!(
            InstalledKeymap::get(cx).state(Command::NewWorkspace),
            KeybindingState::Overridden
        );
    });
    cx.simulate_keystrokes("cmd-/");
    assert_eq!(dispatches.get(), 2);
    cx.simulate_keystrokes("cmd-&");
    assert_eq!(dispatches.get(), 3);
}

#[gpui::test]
fn layout_changes_replace_fixed_and_control_bindings_without_changing_their_positions(
    cx: &mut TestAppContext,
) {
    use crate::keybindings::{SystemReservation, SystemReserved, TerminalConventions};
    use crate::platform::keyboard_layout::KeyboardLayout;
    let layout = Rc::new(ChangingLayout(std::cell::RefCell::new(
        KeyboardLayout::us_english(),
    )));
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    let menu = Rc::new(RecordingApplicationMenuAdapter::default());
    cx.update(|cx| {
        crate::ui::init(cx).unwrap();
        let profile = KeymapProfile::new(
            layout.clone(),
            TerminalConventions::ControlShiftShortcuts,
            [],
            vec![SystemReserved {
                shortcut: Shortcut::parse("ctrl-shift-,").unwrap(),
                reason: SystemReservation::Settings,
            }],
            vec![KeyBinding::new(
                "ctrl-shift-,",
                crate::ui::settings_window::OpenSettings,
                None,
            )],
            vec![KeyBinding::new(
                "ctrl-shift-.",
                crate::ui::CloseTerminalFind,
                None,
            )],
        )
        .unwrap();
        cx.clear_key_bindings();
        cx.bind_keys([KeyBinding::new("ctrl-shift-a", CreateTab, None)]);
        cx.bind_keys(profile.control_bindings().iter().cloned());
        cx.bind_keys(profile.fixed_bindings().iter().cloned());
        cx.bind_keys([KeyBinding::new("ctrl-shift-b", CreateTab, None)]);
        install(profile, cx);
        attach_application_menu(menu.clone(), cx);
        cx.on_action(move |_: &crate::ui::settings_window::OpenSettings, _| {
            observed.set(observed.get() + 1)
        });
    });
    let (_, cx) = cx.add_window_view(|_, _| DispatchView);
    cx.simulate_keystrokes("ctrl-<");
    assert_eq!(calls.get(), 1);
    let mut german = KeyboardLayout::default();
    german.insert(false, ",", ";");
    german.insert(false, ".", ":");
    *layout.0.borrow_mut() = german;
    cx.update(|_, cx| {
        refresh_layout(cx);
        let keys = bindings(cx)
            .iter()
            .map(|binding| binding.keystrokes()[0].inner().key.clone())
            .collect::<Vec<_>>();
        assert_eq!(keys, ["a", ":", ";", "b"]);
        assert_eq!(
            DesktopPresentation::get(cx)
                .shortcut(&crate::ui::settings_window::OpenSettings)
                .as_deref(),
            Some("Ctrl+;")
        );
    });
    cx.simulate_keystrokes("ctrl-<");
    assert_eq!(calls.get(), 1);
    cx.simulate_keystrokes("ctrl-;");
    assert_eq!(calls.get(), 2);
    assert_eq!(
        menu.installs(),
        1,
        "a fixed-only layout change refreshes the native menu"
    );
}
