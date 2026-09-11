use std::{cell::RefCell, rc::Rc, sync::Arc, time::Duration};

use gpui::prelude::*;
use gpui::{Context, Entity, Modifiers, Render, TestAppContext, VisualTestContext, Window, div};
use spaceterm_ui::{
    Alert, AlertOutcome, Dialog, DialogCloseDecision, DialogInitialFocus, DialogOutcome,
    ModalAction, ModalActionRole, ModalId, ModalPresentationId, ProgressCancelDecision,
    ProgressCancellation, ProgressDialog, ProgressDialogOutcome, ProgressState, TextInput,
    TextInputContentMode, TextInputVariant,
};

use crate::ui::appearance::{ChromeAppearance, InstalledChrome};

const INPUT_VALUE: &str = "nonsecret-sample";

struct ReadOnlyExerciserStorage;

impl crate::settings::storage::SettingsStorage for ReadOnlyExerciserStorage {
    fn read(
        &self,
    ) -> Result<
        Option<crate::platform::secure_filesystem::PrivateFileSnapshot>,
        crate::settings::storage::StorageError,
    > {
        Ok(None)
    }

    fn write(
        &self,
        _: &[u8],
        _: Option<&crate::platform::secure_filesystem::SecureEntryIdentity>,
    ) -> Result<crate::settings::storage::StorageCommit, crate::settings::storage::StorageError>
    {
        panic!("exerciser preview must not write")
    }
}

#[gpui::test]
fn exerciser_diagnostics_repaint_for_terminal_only_system_changes(cx: &mut TestAppContext) {
    use crate::appearance::{Appearance, AppearanceDocument, SchemeId, SchemeSelection};
    use crate::platform::appearance::testing::RecordingAppearancePlatform;
    use crate::ui::appearance_runtime;

    let (settings, changed) =
        crate::settings::UserSettings::load(Arc::new(ReadOnlyExerciserStorage));
    let platform = RecordingAppearancePlatform::default();
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.update(|cx| {
        appearance_runtime::install(settings.clone(), changed, Rc::new(platform.clone()), cx)
            .unwrap();
        crate::ui::init(cx).unwrap();
    });
    let (_exerciser, cx) = cx.add_window_view(super::AppearanceExerciser::new);
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("appearance-diagnostics-generation-0")
            .is_some()
    );

    let token = settings.begin_preview(0).unwrap();
    let mut candidate = AppearanceDocument::default();
    candidate.preferences.terminal.scheme = SchemeSelection::System {
        light: SchemeId::new("builtin.spaceterm.terminal.light").unwrap(),
        dark: SchemeId::new("builtin.vague-pro.terminal.dark").unwrap(),
    };
    settings.update_preview(&token, candidate).unwrap();
    cx.run_until_parked();
    let chrome = cx.update(|_, cx| Arc::clone(&cx.global::<InstalledChrome>().0));
    platform.set_system_appearance(Some(Appearance::Light));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("appearance-diagnostics-generation-1")
            .is_some()
    );
    cx.update(|_, cx| assert!(Arc::ptr_eq(&chrome, &cx.global::<InstalledChrome>().0)));
    platform.set_system_appearance(Some(Appearance::Dark));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("appearance-diagnostics-generation-2")
            .is_some()
    );
}

struct ModalAppearanceRegressionFixture {
    input: Entity<TextInput>,
}

impl ModalAppearanceRegressionFixture {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            input: cx.new(|cx| {
                TextInput::new(
                    "appearance-modal-regression-input",
                    "Obscured input fixture",
                    INPUT_VALUE,
                    window,
                    cx,
                )
                .variant(TextInputVariant::Standard)
                .content_mode(TextInputContentMode::Obscured)
            }),
        }
    }
}

impl Render for ModalAppearanceRegressionFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
        spaceterm_ui::ModalLayer::new(div().size_full().child(self.input.clone()))
    }
}

fn fixture_window(
    cx: &mut TestAppContext,
) -> (
    Entity<ModalAppearanceRegressionFixture>,
    &mut VisualTestContext,
) {
    cx.update(|cx| crate::ui::init(cx).expect("UI initialization should succeed"));
    let (fixture, cx) = cx.add_window_view(ModalAppearanceRegressionFixture::new);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    focus_input(&fixture, cx);
    (fixture, cx)
}

fn focus_input(fixture: &Entity<ModalAppearanceRegressionFixture>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        fixture.read(cx).input.read(cx).focus_handle().focus(window);
    });
    cx.run_until_parked();
}

fn replace_appearance(generation: u64, cx: &mut VisualTestContext) {
    let appearance = ChromeAppearance {
        text_scale: 1.0 + generation as f32 / 10.0,
        spacing_scale: 1.0 + generation as f32 / 20.0,
        ..ChromeAppearance::default()
    };
    let controls = crate::ui::control_theme_catalog::catalog(&appearance)
        .generation(spaceterm_ui::ControlThemeGeneration::new(generation));
    cx.update(|window, cx| {
        assert_eq!(
            spaceterm_ui::replace_control_theme_catalog(cx, controls),
            Ok(spaceterm_ui::ControlThemeReplacement::Applied)
        );
        cx.set_global(InstalledChrome(Arc::new(appearance)));
        window.refresh();
    });
    cx.run_until_parked();
}

fn assert_modal_rendered(presentation: ModalPresentationId, cx: &mut VisualTestContext) {
    let surface = match presentation.value() {
        1 => "modal-surface-1",
        2 => "modal-surface-2",
        3 => "modal-surface-3",
        unexpected => panic!("unexpected appearance regression presentation {unexpected}"),
    };
    assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
    assert!(cx.debug_bounds("spaceterm-modal-root").is_some());
    assert!(cx.debug_bounds(surface).is_some());
}

fn assert_input_restored(
    fixture: &Entity<ModalAppearanceRegressionFixture>,
    cx: &mut VisualTestContext,
) {
    let (value, focused) = cx.update(|window, cx| {
        let input = fixture.read(cx).input.read(cx);
        (
            input.value().to_owned(),
            input.focus_handle().is_focused(window),
        )
    });
    assert_eq!(value, INPUT_VALUE);
    assert!(
        focused,
        "modal closure should restore the obscured input focus"
    );
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let position = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was not rendered"))
        .center();
    cx.simulate_mouse_move(position, None, Modifiers::none());
    cx.simulate_click(position, Modifiers::none());
    cx.run_until_parked();
}

#[gpui::test]
fn open_modal_facades_should_survive_live_appearance_replacement(cx: &mut TestAppContext) {
    let (fixture, cx) = fixture_window(cx);

    let alert_outcome = Rc::new(RefCell::new(None));
    let alert_result = Rc::clone(&alert_outcome);
    let alert = cx.update(|window, cx| {
        fixture.update(cx, |_, cx| {
            Alert::new(
                ModalId::new("appearance-regression-alert"),
                "Appearance regression alert",
                "Appearance Alert",
                "The alert remains interactive after a live appearance replacement.",
                vec![
                    ModalAction::new(
                        "accept",
                        "Accept",
                        ModalActionRole::Affirmative,
                        "appearance-regression-alert-accept",
                    )
                    .default_action(true),
                ],
            )
            .present(window, cx, move |outcome, _| {
                *alert_result.borrow_mut() = Some(outcome);
            })
            .expect("Alert should present")
        })
    });
    cx.run_until_parked();
    assert_modal_rendered(alert.presentation_id(), cx);
    replace_appearance(2, cx);
    assert_modal_rendered(alert.presentation_id(), cx);
    click("modal-action-appearance-regression-alert-accept", cx);
    assert!(matches!(
        alert_outcome.borrow().as_ref(),
        Some(AlertOutcome::Activated {
            action_id: "accept",
            ..
        })
    ));
    assert_input_restored(&fixture, cx);

    let dialog_outcome = Rc::new(RefCell::new(None));
    let dialog_result = Rc::clone(&dialog_outcome);
    let dialog = cx.update(|window, cx| {
        fixture.update(cx, |_, cx| {
            Dialog::new(
                ModalId::new("appearance-regression-dialog"),
                "Appearance regression dialog",
                "Appearance Dialog",
                vec![
                    ModalAction::new(
                        "save",
                        "Save",
                        ModalActionRole::Affirmative,
                        "appearance-regression-dialog-save",
                    )
                    .default_action(true),
                    ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "appearance-regression-dialog-cancel",
                    ),
                ],
                DialogInitialFocus::Action("save"),
            )
            .description("The dialog retains its result authority across appearance replacement.")
            .present(
                window,
                cx,
                |_, _, _| DialogCloseDecision::Allow,
                move |outcome, _| *dialog_result.borrow_mut() = Some(outcome),
            )
            .expect("Dialog should present")
        })
    });
    cx.run_until_parked();
    assert_modal_rendered(dialog.presentation_id(), cx);
    replace_appearance(3, cx);
    assert_modal_rendered(dialog.presentation_id(), cx);
    click("modal-action-appearance-regression-dialog-save", cx);
    assert!(matches!(
        dialog_outcome.borrow().as_ref(),
        Some(DialogOutcome::Completed {
            action_id: "save",
            ..
        })
    ));
    assert_input_restored(&fixture, cx);

    let progress_outcome = Rc::new(RefCell::new(None));
    let progress_result = Rc::clone(&progress_outcome);
    let progress = cx.update(|window, cx| {
        fixture.update(cx, |_, cx| {
            ProgressDialog::<()>::new(
                ModalId::new("appearance-regression-progress"),
                "Appearance regression progress",
                "Appearance Progress",
                "Applying presentation",
                ProgressState::Indeterminate,
                ProgressCancellation::programmatic_only(Duration::from_secs(30)),
            )
            .detail("The progress presentation remains completeable after replacement.")
            .present(
                window,
                cx,
                |_, _, _| ProgressCancelDecision::Deny,
                move |outcome, _| *progress_result.borrow_mut() = Some(outcome),
            )
            .expect("ProgressDialog should present")
        })
    });
    cx.run_until_parked();
    assert_modal_rendered(progress.presentation_id(), cx);
    assert!(cx.debug_bounds("modal-progress-indeterminate").is_some());
    replace_appearance(4, cx);
    assert_modal_rendered(progress.presentation_id(), cx);
    assert!(cx.debug_bounds("modal-progress-indeterminate").is_some());
    cx.update(|window, cx| {
        progress
            .complete(window, cx)
            .expect("ProgressDialog should remain completeable")
    });
    cx.run_until_parked();
    assert_eq!(
        progress_outcome.borrow().as_ref(),
        Some(&ProgressDialogOutcome::Completed)
    );
    assert_input_restored(&fixture, cx);
}
