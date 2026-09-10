use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    EmptyView, Entity, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext,
};

use super::*;
use crate::ssh::command::{SshCommandContext, ValidatedRemoteShellCommand};
use crate::terminal::testing::{
    RecordedSessionCommand, TestTerminalSessionFactory, TestTerminalSessionRecords,
    test_local_directory,
};
use crate::terminal::{
    LocalTerminalLaunchPlan, RemoteTerminalChannelProvider, ScrollbarSnapshot, SessionExit,
    SessionFailure, TerminalLaunchPlan, TerminalSessionFactory,
};

#[test]
fn active_application_with_non_key_window_suppresses_inactive_only_notification() {
    let activity = SurfaceActivity {
        application_active: true,
        operating_system_window_key: false,
    };
    let mut attention = AttentionState::default();

    let effects = attention.observe(
        crate::terminal::attention::AttentionEvent::Bell,
        AttentionFacts {
            terminal_input_focus: false,
            surface_active: terminal_surface_active(TerminalProductFocus::default(), activity),
            application_active: activity.application_active,
        },
        Instant::now(),
    );

    assert_eq!(
        (effects.request_dock_attention, effects.notification),
        (true, None)
    );
}

struct KeyPropagationProbe {
    pane: Entity<TerminalPane>,
    propagated_key_downs: Rc<Cell<usize>>,
}

struct RecordingFilePreviewPanel {
    previews: Rc<Cell<usize>>,
    dismissals: Rc<Cell<usize>>,
}

impl FilePreviewPanel for RecordingFilePreviewPanel {
    fn preview_file(
        &mut self,
        _: &std::path::Path,
    ) -> Result<(), crate::terminal::native_services::file_preview::FilePreviewError> {
        self.previews.set(self.previews.get() + 1);
        Ok(())
    }

    fn dismiss(&mut self) {
        self.dismissals.set(self.dismissals.get() + 1);
    }
}

impl Render for KeyPropagationProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let propagated_key_downs = Rc::clone(&self.propagated_key_downs);
        div()
            .size_full()
            .on_key_down(move |_, _, _| {
                propagated_key_downs.set(propagated_key_downs.get() + 1);
            })
            .child(self.pane.clone())
    }
}

fn blinking_screen() -> Arc<ScreenSnapshot> {
    ScreenSnapshot::from_test_parts(
        Arc::from([Arc::from([crate::terminal::CellSnapshot {
            text: "x".to_owned(),
            foreground_source: crate::terminal::TerminalColor::Default,
            background_source: crate::terminal::TerminalColor::Default,
            inverse: false,
            bold: false,
            faint: false,
            italic: false,
            blinking: true,
            invisible: false,
            underline: crate::terminal::TerminalUnderlineSnapshot::None,
            underline_source: crate::terminal::TerminalColor::Default,
            strikethrough: false,
            overline: false,
            selected: false,
            spacer_tail: false,
            semantic_content: crate::terminal::CellSemanticSnapshot::Output,
            hyperlink: None,
        }])]),
        ScrollbarSnapshot::default(),
        "blink",
    )
}

fn blinking_cursor_screen(visible: bool, blinking: bool) -> Arc<ScreenSnapshot> {
    let mut screen = ScreenSnapshot::from_test_parts(
        Arc::from([Arc::from([crate::terminal::CellSnapshot {
            text: "x".to_owned(),
            foreground_source: crate::terminal::TerminalColor::Default,
            background_source: crate::terminal::TerminalColor::Default,
            inverse: false,
            bold: false,
            faint: false,
            italic: false,
            blinking: false,
            invisible: false,
            underline: crate::terminal::TerminalUnderlineSnapshot::None,
            underline_source: crate::terminal::TerminalColor::Default,
            strikethrough: false,
            overline: false,
            selected: false,
            spacer_tail: false,
            semantic_content: crate::terminal::CellSemanticSnapshot::Output,
            hyperlink: None,
        }])]),
        ScrollbarSnapshot::default(),
        "cursor blink",
    );
    Arc::make_mut(&mut screen).cursor = crate::terminal::CursorSnapshot {
        position: Some(crate::terminal::CursorPositionSnapshot {
            column: 0,
            row: 0,
            width_cells: 1,
        }),
        visible,
        blinking,
        ..crate::terminal::CursorSnapshot::default()
    };
    screen
}

fn context_action_screen(
    link: Option<crate::terminal::HyperlinkTarget>,
    selection_present: bool,
) -> Arc<ScreenSnapshot> {
    let mut screen = ScreenSnapshot::from_test_parts_at(
        Arc::from([Arc::from([crate::terminal::CellSnapshot {
            text: "x".to_owned(),
            foreground_source: crate::terminal::TerminalColor::Default,
            background_source: crate::terminal::TerminalColor::Default,
            inverse: false,
            bold: false,
            faint: false,
            italic: false,
            blinking: false,
            invisible: false,
            underline: crate::terminal::TerminalUnderlineSnapshot::None,
            underline_source: crate::terminal::TerminalColor::Default,
            strikethrough: false,
            overline: false,
            selected: selection_present,
            spacer_tail: false,
            semantic_content: crate::terminal::CellSemanticSnapshot::Output,
            hyperlink: link.map(Arc::new),
        }])]),
        ScrollbarSnapshot::default(),
        "context action",
        7,
    );
    Arc::make_mut(&mut screen).selection_present = selection_present;
    screen
}

fn graphics_screen(generation: u64, image_id: u32) -> Arc<ScreenSnapshot> {
    graphics_screen_with_images(generation, &[image_id])
}

fn graphics_screen_with_images(generation: u64, image_ids: &[u32]) -> Arc<ScreenSnapshot> {
    let mut screen = ScreenSnapshot::from_test_parts_at(
        Arc::from([]),
        ScrollbarSnapshot::default(),
        "graphics",
        generation,
    );
    Arc::make_mut(&mut screen).graphics = crate::terminal::GraphicsSnapshot {
        generation,
        placement_generation: generation,
        images: image_ids
            .iter()
            .map(|image_id| {
                Arc::new(crate::terminal::ImageSnapshot {
                    key: crate::terminal::ImageKey {
                        image_id: *image_id,
                        generation,
                    },
                    width: 1,
                    height: 1,
                    rgba: Arc::from([10, 20, 30, 255]),
                    reservation: None,
                })
            })
            .collect::<Vec<_>>()
            .into(),
        placements: image_ids
            .iter()
            .enumerate()
            .map(
                |(index, image_id)| crate::terminal::ImagePlacementSnapshot {
                    image: crate::terminal::ImageKey {
                        image_id: *image_id,
                        generation,
                    },
                    placement_id: index as u32,
                    z: 0,
                    viewport_col: index as i32,
                    viewport_row: 0,
                    cell_offset_x: 0,
                    cell_offset_y: 0,
                    source_x: 0,
                    source_y: 0,
                    source_width: 1,
                    source_height: 1,
                    destination_width: 1,
                    destination_height: 1,
                    unicode_placeholder: false,
                },
            )
            .collect::<Vec<_>>()
            .into(),
    };
    screen
}

fn text_screen(generation: u64, rows: &[&str]) -> Arc<ScreenSnapshot> {
    let rows = rows
        .iter()
        .map(|row| {
            row.chars()
                .map(|character| crate::terminal::CellSnapshot {
                    text: character.to_string(),
                    foreground_source: crate::terminal::TerminalColor::Default,
                    background_source: crate::terminal::TerminalColor::Default,
                    inverse: false,
                    bold: false,
                    faint: false,
                    italic: false,
                    blinking: false,
                    invisible: false,
                    underline: crate::terminal::TerminalUnderlineSnapshot::None,
                    underline_source: crate::terminal::TerminalColor::Default,
                    strikethrough: false,
                    overline: false,
                    selected: false,
                    spacer_tail: false,
                    semantic_content: crate::terminal::CellSemanticSnapshot::Output,
                    hyperlink: None,
                })
                .collect::<Vec<_>>()
                .into()
        })
        .collect::<Vec<_>>()
        .into();
    ScreenSnapshot::from_test_parts_at(
        rows,
        ScrollbarSnapshot::default(),
        "text preflight",
        generation,
    )
}

fn terminal_pane(cx: &mut TestAppContext) -> (Entity<TerminalPane>, &mut VisualTestContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
        TestTerminalSessionFactory::new(TestTerminalSessionRecords::default())
            .with_start_failure("terminal session unavailable in UI test"),
    );
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-terminal-pane-test",
        )),
    );
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, _cx| pane.focus(window));
    });
    cx.run_until_parked();
    (pane, cx)
}

fn latest_recorded_presentability(records: &TestTerminalSessionRecords) -> Option<bool> {
    records
        .commands()
        .into_iter()
        .rev()
        .find_map(|call| match call.command {
            RecordedSessionCommand::SetPresentable(presentable) => Some(presentable),
            _ => None,
        })
}

#[gpui::test]
fn visibility_subscription_coalesces_hidden_receivers_and_retires_without_polling(
    cx: &mut TestAppContext,
) {
    cx.update(crate::ui::init).unwrap();
    let records = TestTerminalSessionRecords::default();
    let factory =
        Rc::new(crate::platform::window_visibility::RecordingWindowVisibilityFactory::default());
    let mut dependencies = PaneLifecycleDependencies::testing();
    dependencies.visibility = factory.clone();
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        Rc::new(TestTerminalSessionFactory::new(records.clone())),
        test_local_directory(std::env::temp_dir()),
    );
    let (pane, cx) = cx.add_window_view(|window, cx| {
        TerminalPane::new_with_services(
            session_factory, None, crate::terminal::testing::test_terminal_key_input_adapter(),
            &crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default(),
            crate::terminal::native_services::testing::adapters(), dependencies, window, cx,
        )
    });
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, _| pane.focus(window));
    });
    cx.run_until_parked();
    let window_id = cx.update(|window, _| window.window_handle().window_id());
    assert_eq!(factory.captured_windows(), vec![window_id]);
    let sender = records.last_event_sender().unwrap();
    sender
        .try_send(SessionEvent::Screen(text_screen(1, &["fixture"])))
        .unwrap();
    cx.run_until_parked();
    pane.update(cx, |pane, cx| {
        pane.screen = blinking_cursor_screen(true, true);
        pane.sync_presentation_blink(true, true, cx);
        pane.start_visual_bell(cx);
    });
    factory.set_visibility(
        window_id,
        WindowVisibility {
            occluded: true,
            ..WindowVisibility::default()
        },
    );
    cx.run_until_parked();
    assert!(
        pane.read_with(cx, |pane, _| !pane.render_lifecycle.can_present()
            && pane._blink_task.is_none()
            && pane._attention_task.is_none())
    );
    assert_eq!(latest_recorded_presentability(&records), Some(false));
    let notifications = Rc::new(Cell::new(0));
    cx.update(|_, cx| {
        let notifications = notifications.clone();
        cx.observe(&pane, move |_, _| {
            notifications.set(notifications.get() + 1)
        })
        .detach();
    });
    for generation in 2..=12 {
        sender
            .try_send(SessionEvent::Screen(text_screen(generation, &["fixture"])))
            .unwrap();
    }
    records
        .last_accessibility_sender()
        .unwrap()
        .try_send(accessibility_model(12))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(notifications.get(), 0);
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.screen.generation),
        crate::terminal::PresentationGeneration::test(12)
    );
    factory.set_visibility(window_id, WindowVisibility::default());
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.render_lifecycle.can_present()));
    assert_eq!(latest_recorded_presentability(&records), Some(true));
    pane.update(cx, |pane, cx| {
        pane.set_product_focus(
            TerminalProductFocus {
                active_tab: false,
                ..TerminalProductFocus::default()
            },
            cx,
        );
    });
    assert_eq!(latest_recorded_presentability(&records), Some(false));
    pane.update(cx, |pane, cx| {
        pane.set_product_focus(TerminalProductFocus::default(), cx);
    });
    assert_eq!(latest_recorded_presentability(&records), Some(true));
    assert!(notifications.get() > 0);
    let (initial_accessibility, scrolled_accessibility) =
        crate::terminal::testing::test_accessibility_viewport_models(
            crate::terminal::PresentationGeneration::test(12),
        );
    pane.update(cx, |pane, _| pane.set_accessibility_hierarchy(true, 0));
    records
        .last_accessibility_sender()
        .unwrap()
        .try_send(initial_accessibility)
        .unwrap();
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| {
        pane.pending_accessibility_notifications.is_empty()
    }));
    factory.set_visibility(
        window_id,
        WindowVisibility {
            occluded: true,
            ..WindowVisibility::default()
        },
    );
    cx.run_until_parked();
    pane.update(cx, |pane, _| {
        pane.render_lifecycle.mark_presented(pane.screen.generation);
    });
    notifications.set(0);
    records
        .last_accessibility_sender()
        .unwrap()
        .try_send(scrolled_accessibility)
        .unwrap();
    cx.run_until_parked();
    assert_eq!(notifications.get(), 0);
    assert!(pane.read_with(cx, |pane, _| {
        pane.pending_accessibility_notifications.is_empty()
    }));
    factory.set_visibility(window_id, WindowVisibility::default());
    cx.run_until_parked();
    assert!(
        notifications.get() > 0,
        "restoration must publish accessibility-only changes"
    );
    assert!(!pane.read_with(cx, |pane, _| pane.accessibility_needs_presentation));
    let restored_notifications = notifications.get();
    factory.set_visibility(window_id, WindowVisibility::default());
    cx.run_until_parked();
    assert_eq!(notifications.get(), restored_notifications);
    pane.update(cx, |pane, _| {
        pane.close();
        pane.close();
    });
    assert_eq!(factory.drop_count(), 1);
    assert!(pane.read_with(cx, |pane, _| pane._visibility_task.is_none()));
    factory.set_visibility(
        window_id,
        WindowVisibility {
            minimized: true,
            ..WindowVisibility::default()
        },
    );
    cx.run_until_parked();
    assert!(!pane.read_with(cx, |pane, _| pane.render_lifecycle.can_present()));
}

fn directory_screen(
    generation: u64,
    path: &str,
    freshness: crate::terminal::metadata::MetadataFreshness,
) -> Arc<ScreenSnapshot> {
    let mut screen = ScreenSnapshot::from_test_parts_at(
        Arc::from([]),
        ScrollbarSnapshot::default(),
        "directory",
        generation,
    );
    {
        let screen = Arc::make_mut(&mut screen);
        let metadata = Arc::make_mut(&mut screen.metadata);
        metadata.directory.path = Arc::from(path);
        metadata.freshness = freshness;
    }
    screen
}

fn remote_directory_screen(generation: u64, path: &str) -> Arc<ScreenSnapshot> {
    let mut screen = directory_screen(
        generation,
        path,
        crate::terminal::metadata::MetadataFreshness::Live,
    );
    Arc::make_mut(&mut Arc::make_mut(&mut screen).metadata).context =
        crate::terminal::metadata::TerminalMetadataContext::Remote(
            crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                crate::domain::SshDestination::new("user@remote".to_owned()).unwrap(),
                crate::domain::RemoteDirectory::new(path.to_owned()).unwrap(),
            ),
        );
    screen
}

#[gpui::test]
fn retained_directory_metadata_should_reject_older_screens_and_clear_stale_directory(
    cx: &mut TestAppContext,
) {
    use crate::terminal::metadata::{CurrentDirectory, MetadataFreshness};
    let (pane, cx) = terminal_pane(cx);
    let latest = CurrentDirectory::Local(PathBuf::from("/projects/latest"));
    pane.update(cx, |pane, cx| {
        pane.accept_directory_metadata(crate::terminal::SessionDirectorySnapshot {
            revision: 20,
            current: Some(latest.clone()),
        });
        let mut old_screen = directory_screen(1, "/projects/old", MetadataFreshness::Live);
        Arc::make_mut(&mut Arc::make_mut(&mut old_screen).metadata).revision = 10;
        pane.handle_event(SessionEvent::Screen(old_screen), cx);
        assert_eq!(pane.current_directory(), Some(latest.clone()));
        pane.accept_directory_metadata(crate::terminal::SessionDirectorySnapshot {
            revision: 21,
            current: None,
        });
        assert_eq!(pane.current_directory(), None);
    });
}

#[gpui::test]
fn current_directory_preserves_machine_and_rejects_stale_or_relative_metadata(
    cx: &mut TestAppContext,
) {
    use crate::terminal::metadata::{CurrentDirectory, MetadataFreshness};
    let (pane, cx) = terminal_pane(cx);
    for (generation, path, freshness, expected) in [
        (
            1,
            "/Users/test/live",
            MetadataFreshness::Live,
            Some(CurrentDirectory::Local(PathBuf::from("/Users/test/live"))),
        ),
        (2, "/Users/test/stale", MetadataFreshness::Stale, None),
        (3, "relative", MetadataFreshness::Live, None),
    ] {
        pane.update(cx, |pane, cx| {
            pane.handle_event(
                SessionEvent::Screen(directory_screen(generation, path, freshness)),
                cx,
            );
        });
        assert_eq!(
            pane.read_with(cx, |pane, _| pane.current_directory()),
            expected
        );
    }
    pane.update(cx, |pane, cx| {
        pane.handle_event(
            SessionEvent::Screen(remote_directory_screen(4, "/srv/app")),
            cx,
        );
    });
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.current_directory()),
        Some(CurrentDirectory::Remote(
            crate::domain::RemoteDirectory::new("/srv/app".into()).unwrap()
        ))
    );
}

#[gpui::test]
fn visual_bell_presentation_state_clears_on_focus_or_input(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);

    pane.update(cx, |pane, cx| {
        pane.terminal_input_focus = true;
        pane.handle_event(
            SessionEvent::Attention(crate::terminal::attention::AttentionEvent::Bell),
            cx,
        );
    });
    assert!(pane.read_with(cx, |pane, _| pane.attention_visual));

    pane.update(cx, |pane, cx| pane.clear_attention(cx));
    assert!(!pane.read_with(cx, |pane, _| pane.attention_visual));
}

#[gpui::test]
fn accepted_input_method_commit_clears_pending_attention(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.terminal_input_focus = false;
        pane.handle_event(
            SessionEvent::Attention(crate::terminal::attention::AttentionEvent::Bell),
            cx,
        );
        pane.terminal_input_focus = true;
    });
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.attention.unread_count()),
        1
    );

    pane.update(cx, |pane, cx| {
        pane.send_key_translation(
            KeyTranslation::Encoded(KeyInput::input_method_commit("界")),
            cx,
        );
    });

    assert_eq!(
        pane.read_with(cx, |pane, _| pane.attention.unread_count()),
        0
    );
}

#[gpui::test]
fn accepted_paste_clears_pending_attention(cx: &mut TestAppContext) {
    let (pane, cx, _records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::Written),
        Ok(PasteResolution::Cancelled),
    );
    pane.update(cx, |pane, cx| {
        pane.terminal_input_focus = false;
        pane.handle_event(
            SessionEvent::Attention(crate::terminal::attention::AttentionEvent::Bell),
            cx,
        );
        pane.terminal_input_focus = true;
    });
    cx.write_to_clipboard(ClipboardItem::new_string("accepted paste".to_owned()));

    cx.dispatch_action(PasteClipboard);
    cx.run_until_parked();

    assert_eq!(
        pane.read_with(cx, |pane, _| pane.attention.unread_count()),
        0
    );
}

#[gpui::test]
fn stale_guarded_written_paste_does_not_clear_newer_attention(cx: &mut TestAppContext) {
    let (pane, cx, _records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::Written),
        Ok(PasteResolution::Cancelled),
    );
    cx.write_to_clipboard(ClipboardItem::new_string("stale paste".to_owned()));

    cx.dispatch_action(PasteClipboard);
    pane.update(cx, |pane, cx| {
        pane.advance_native_service_focus_epoch();
        pane.terminal_input_focus = false;
        pane.handle_event(
            SessionEvent::Attention(
                crate::terminal::attention::AttentionEvent::CommandFinished {
                    exit_status: Some(0),
                    duration: Duration::from_secs(1),
                },
            ),
            cx,
        );
        pane.terminal_input_focus = true;
    });
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.attention.unread_count()),
        1
    );

    cx.run_until_parked();

    assert_eq!(
        pane.read_with(cx, |pane, _| pane.attention.unread_count()),
        1
    );
}

fn connected_terminal_pane(
    cx: &mut TestAppContext,
) -> (
    Entity<TerminalPane>,
    &mut VisualTestContext,
    TestTerminalSessionRecords,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()));
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-terminal-pane-keyboard-test",
        )),
    );
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, _cx| pane.focus(window));
    });
    cx.run_until_parked();
    (pane, cx, records)
}

fn remote_workspace_session_factory(
    records: TestTerminalSessionRecords,
) -> WorkspaceTerminalSessionFactory {
    remote_workspace_session_factory_with_readiness(records, Arc::new(AtomicBool::new(true)))
}

struct ToggleRemoteChannelProvider {
    ready: Arc<AtomicBool>,
    command_context: Arc<SshCommandContext>,
}

impl RemoteTerminalChannelProvider for ToggleRemoteChannelProvider {
    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    fn revalidate(
        &self,
        _directory: crate::domain::RemoteDirectory,
        _identity: Option<crate::domain::RemoteDirectoryIdentity>,
    ) -> gpui::Task<Result<(), crate::terminal::RemoteChannelRevalidationError>> {
        if self.is_ready() {
            gpui::Task::ready(Ok(()))
        } else {
            gpui::Task::ready(Err(
                crate::terminal::RemoteChannelRevalidationError::ConnectionUnavailable,
            ))
        }
    }

    fn prepare(
        &self,
        _directory: &crate::domain::RemoteDirectory,
    ) -> Result<crate::ssh::command::PreparedSshPaneChannelCommand, RemoteChannelUnavailable> {
        if !self.is_ready() {
            return Err(RemoteChannelUnavailable);
        }
        Ok(self.command_context.prepare_pane_channel(
            ValidatedRemoteShellCommand::new("exec /bin/zsh -l".to_owned()).unwrap(),
        ))
    }
}

fn remote_workspace_session_factory_with_readiness(
    records: TestTerminalSessionRecords,
    ready: Arc<AtomicBool>,
) -> WorkspaceTerminalSessionFactory {
    let destination = crate::domain::SshDestination::new("tester@remote".to_owned()).unwrap();
    let command_context = Arc::new(
        SshCommandContext::new(
            crate::ssh::command::OpenSshExecutable::for_test(),
            PathBuf::from("/private/config/spaceterm/ssh_config"),
            destination.clone(),
            PathBuf::from("/private/runtime/spaceterm/master.sock"),
        )
        .unwrap(),
    );
    let channel_provider = Arc::new(ToggleRemoteChannelProvider {
        ready,
        command_context,
    });
    WorkspaceTerminalSessionFactory::new_remote(
        Rc::new(TestTerminalSessionFactory::new(records)),
        test_local_directory(PathBuf::from("/local/home")),
        crate::terminal::metadata::RemoteTerminalMetadataContext::new(
            destination,
            crate::domain::RemoteDirectory::new("~/project".to_owned()).unwrap(),
        ),
        crate::domain::RemoteDirectoryIdentity::new("/home/tester/project".to_owned()).unwrap(),
        "project on remote".to_owned(),
        channel_provider,
    )
}

fn connected_remote_terminal_pane(
    cx: &mut TestAppContext,
) -> (
    Entity<TerminalPane>,
    &mut VisualTestContext,
    TestTerminalSessionRecords,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory = remote_workspace_session_factory(records.clone());
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, _| pane.focus(window));
    });
    cx.run_until_parked();
    (pane, cx, records)
}

fn connected_remote_terminal_pane_with_readiness(
    cx: &mut TestAppContext,
    ready: Arc<AtomicBool>,
) -> (
    Entity<TerminalPane>,
    &mut VisualTestContext,
    TestTerminalSessionRecords,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory = remote_workspace_session_factory_with_readiness(records.clone(), ready);
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, _| pane.focus(window));
    });
    cx.run_until_parked();
    (pane, cx, records)
}

#[gpui::test]
fn remote_restart_ignores_prior_epoch_events_and_accepts_fresh_generation_one(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_remote_terminal_pane(cx);
    let exits = Rc::new(Cell::new(0));
    let observed_exits = Rc::clone(&exits);
    pane.update(cx, |_, cx| {
        cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
            if matches!(event, TerminalPaneEvent::Exited) {
                observed_exits.set(observed_exits.get() + 1);
            }
        })
        .detach();
    });

    let (old_epoch, old_accessibility) = pane.update(cx, |pane, cx| {
        let old_epoch = pane.terminal_session.session_epoch;
        pane.handle_session_event(
            old_epoch,
            SessionEvent::Screen(text_screen(90, &["old"])),
            cx,
        );
        pane.record_successfully_presented_screen(Arc::clone(&pane.screen));
        let old_accessibility = Arc::new(TerminalAccessibilityModel::from_screen(&text_screen(
            90,
            &["old"],
        )));
        pane.handle_session_accessibility(old_epoch, Arc::clone(&old_accessibility));
        (old_epoch, old_accessibility)
    });
    let factory = pane.read_with(cx, |pane, _| pane.terminal_session.session_factory.clone());
    pane.update(cx, |pane, cx| pane.disconnect_remote(7, cx).unwrap());
    assert_eq!(
        cx.executor().block(
            factory
                .revalidate_remote_child_launch()
                .expect("remote restart must require revalidation"),
        ),
        Ok(())
    );
    let prepared_launch = factory.prepare_child_launch().unwrap();
    let prepared = pane
        .read_with(cx, |pane, _| {
            pane.prepare_remote_restart(factory, 8, prepared_launch)
        })
        .unwrap();
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.commit_remote_restart(prepared, window, cx).unwrap()
        });
    });
    cx.run_until_parked();
    assert_eq!(records.starts().len(), 2);
    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.screen.generation,
            pane.title.clone(),
            Arc::ptr_eq(&pane.accessibility, &old_accessibility),
        )),
        (
            crate::terminal::PresentationGeneration::test(90),
            SharedString::from("text preflight"),
            true,
        )
    );

    pane.update(cx, |pane, cx| {
        pane.handle_session_event(
            old_epoch,
            SessionEvent::Screen(text_screen(99, &["stale"])),
            cx,
        );
        pane.handle_session_event(old_epoch, SessionEvent::Exited(SessionExit::Success), cx);
        pane.handle_session_accessibility(
            old_epoch,
            Arc::new(TerminalAccessibilityModel::from_screen(&text_screen(
                99,
                &["stale"],
            ))),
        );
        let current_epoch = pane.terminal_session.session_epoch;
        pane.handle_session_event(
            current_epoch,
            SessionEvent::Screen(text_screen(1, &["fresh"])),
            cx,
        );
        pane.record_successfully_presented_screen(Arc::clone(&pane.screen));
    });

    let state = pane.read_with(cx, |pane, _| {
        (
            pane.screen.generation,
            pane.last_valid_screen.generation,
            pane.title.clone(),
            Arc::ptr_eq(&pane.accessibility, &old_accessibility),
        )
    });
    assert_eq!(state.0, crate::terminal::PresentationGeneration::test(1));
    assert_eq!(state.1, crate::terminal::PresentationGeneration::test(1));
    assert_eq!(state.2.as_ref(), "text preflight");
    assert!(state.3);
    assert_eq!(exits.get(), 0);

    let successor_epoch = pane.read_with(cx, |pane, _| pane.terminal_session.session_epoch);
    let delayed_disconnect = pane.update(cx, |pane, cx| pane.disconnect_remote(7, cx));
    assert_eq!(
        delayed_disconnect,
        Err(RemotePaneLifecycleError::StaleGeneration {
            current: 8,
            received: 7,
        })
    );
    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.terminal_session.session_epoch,
            pane.terminal_session.remote_input_blocked
        )),
        (successor_epoch, false)
    );
}

#[gpui::test]
fn disconnected_remote_pane_blocks_input_but_preserves_copy_selection_and_find(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_remote_terminal_pane(cx);
    let pointer_position = pane.read_with(cx, |pane, _| {
        pane.grid_bounds
            .expect("terminal grid was painted")
            .center()
    });
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.handle_event(SessionEvent::Screen(context_action_screen(None, true)), cx);
            pane.open_find(&OpenTerminalFind, window, cx);
            pane.disconnect_remote(3, cx).unwrap();
            pane.copy_selection(&CopySelection, window, cx);
        });
    });
    cx.simulate_keystrokes("blocked");
    cx.simulate_mouse_move(pointer_position, None, Modifiers::none());
    cx.simulate_event(ScrollWheelEvent {
        position: pointer_position,
        delta: ScrollDelta::Lines(point(0.0, -1.0)),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();

    let state = cx.update(|window, cx| {
        pane.read_with(cx, |pane, _| {
            (
                pane.screen.selection_present,
                pane.find_input.is_some(),
                pane.terminal_input_focused(window, cx),
                pane.terminal_session.session.is_some(),
            )
        })
    });
    assert_eq!(state, (true, true, false, true));
    assert!(
        records
            .commands()
            .iter()
            .any(|call| { matches!(call.command, RecordedSessionCommand::RequestSelectionCopy) })
    );
    assert!(!records.commands().iter().any(|call| matches!(
        call.command,
        RecordedSessionCommand::Key(_)
            | RecordedSessionCommand::Pointer(_)
            | RecordedSessionCommand::PointerAndCopySelection(_)
            | RecordedSessionCommand::Wheel(_)
    )));
}

#[gpui::test]
fn failed_master_event_before_disconnect_retains_remote_pane(cx: &mut TestAppContext) {
    let ready = Arc::new(AtomicBool::new(true));
    let (pane, cx, _) = connected_remote_terminal_pane_with_readiness(cx, Arc::clone(&ready));
    ready.store(false, Ordering::SeqCst);

    pane.update(cx, |pane, cx| {
        let epoch = pane.terminal_session.session_epoch;
        pane.handle_session_event(
            epoch,
            SessionEvent::Failed(SessionFailure::Runtime("master exited".to_owned())),
            cx,
        );
        pane.disconnect_remote(7, cx).unwrap();
    });

    assert!(pane.read_with(cx, |pane, _| {
        pane.terminal_session.remote_input_blocked
            && pane.terminal_session.remote_connection_generation == Some(7)
            && pane.pane_state == PaneTerminalState::Running
    }));
}

#[gpui::test]
fn exited_master_event_before_disconnect_retains_remote_pane(cx: &mut TestAppContext) {
    let ready = Arc::new(AtomicBool::new(true));
    let (pane, cx, _) = connected_remote_terminal_pane_with_readiness(cx, Arc::clone(&ready));
    let exits = Rc::new(Cell::new(0));
    let observed_exits = Rc::clone(&exits);
    pane.update(cx, |_, cx| {
        cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
            if matches!(event, TerminalPaneEvent::Exited) {
                observed_exits.set(observed_exits.get() + 1);
            }
        })
        .detach();
    });
    ready.store(false, Ordering::SeqCst);

    pane.update(cx, |pane, cx| {
        let epoch = pane.terminal_session.session_epoch;
        pane.handle_session_event(epoch, SessionEvent::Exited(SessionExit::Success), cx);
        pane.disconnect_remote(7, cx).unwrap();
    });

    assert_eq!(exits.get(), 0);
    assert!(pane.read_with(cx, |pane, _| {
        pane.terminal_session.remote_input_blocked
            && pane.terminal_session.remote_connection_generation == Some(7)
            && pane.pane_state == PaneTerminalState::Running
    }));
}

#[gpui::test]
fn authoritative_disconnect_before_terminal_events_ignores_exit_and_failure(
    cx: &mut TestAppContext,
) {
    let ready = Arc::new(AtomicBool::new(true));
    let (pane, cx, _) = connected_remote_terminal_pane_with_readiness(cx, Arc::clone(&ready));
    let exits = Rc::new(Cell::new(0));
    let observed_exits = Rc::clone(&exits);
    pane.update(cx, |_, cx| {
        cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
            if matches!(event, TerminalPaneEvent::Exited) {
                observed_exits.set(observed_exits.get() + 1);
            }
        })
        .detach();
    });
    ready.store(false, Ordering::SeqCst);

    pane.update(cx, |pane, cx| {
        let old_epoch = pane.terminal_session.session_epoch;
        pane.disconnect_remote(7, cx).unwrap();
        pane.handle_session_event(
            old_epoch,
            SessionEvent::Failed(SessionFailure::Runtime("late failure".to_owned())),
            cx,
        );
        pane.handle_session_event(
            old_epoch,
            SessionEvent::Exited(SessionExit::ExitCode(255)),
            cx,
        );
    });

    assert_eq!(exits.get(), 0);
    assert!(pane.read_with(cx, |pane, _| {
        pane.terminal_session.remote_input_blocked && pane.pane_state == PaneTerminalState::Running
    }));
}

#[gpui::test]
fn disconnect_and_restart_clear_hidden_input_before_successor_focus(cx: &mut TestAppContext) {
    let (pane, cx, _) = connected_remote_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.handle_event(SessionEvent::HiddenInputChanged(true), cx);
    });
    assert!(pane.read_with(cx, |pane, _| pane.hidden_input && pane.terminal_input_focus));

    let factory = pane.read_with(cx, |pane, _| pane.terminal_session.session_factory.clone());
    pane.update(cx, |pane, cx| pane.disconnect_remote(7, cx).unwrap());
    assert!(pane.read_with(cx, |pane, _| {
        !pane.hidden_input && !pane.terminal_input_focus
    }));

    assert_eq!(
        cx.executor().block(
            factory
                .revalidate_remote_child_launch()
                .expect("remote restart must require revalidation"),
        ),
        Ok(())
    );
    let prepared_launch = factory.prepare_child_launch().unwrap();
    let prepared = pane
        .read_with(cx, |pane, _| {
            pane.prepare_remote_restart(factory, 8, prepared_launch)
        })
        .unwrap();
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.commit_remote_restart(prepared, window, cx).unwrap();
            assert!(!pane.hidden_input);
        });
    });
}

#[gpui::test]
fn local_pane_rejects_remote_disconnect_without_mutation(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let epoch = pane.read_with(cx, |pane, _| pane.terminal_session.session_epoch);
    let result = pane.update(cx, |pane, cx| pane.disconnect_remote(1, cx));
    assert_eq!(result, Err(RemotePaneLifecycleError::LocalPane));
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.terminal_session.session_epoch),
        epoch
    );
    assert_eq!(records.starts().len(), 1);
}

#[gpui::test]
fn remote_pane_disables_local_file_actions_but_preserves_text_services_and_web_links(
    cx: &mut TestAppContext,
) {
    let directory = std::env::temp_dir().join(format!(
        "spaceterm-remote-pane-capabilities-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("preview.txt");
    std::fs::write(&file, b"preview").unwrap();
    let local_link = crate::terminal::HyperlinkTarget::osc8(
        "file:preview.txt",
        &directory,
        None,
        TerminalLocalFileCapabilities::Enabled,
    )
    .unwrap();
    let web_link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
    let previews = Rc::new(Cell::new(0));
    let (pane, cx, records) = connected_remote_terminal_pane(cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.screen = context_action_screen(Some(local_link.clone()), true);
            pane.last_geometry = Some(TerminalGeometry::from_grid(
                CellGridSize::new(1, 1),
                LogicalCellSize::new(f32::from(pane.cell_width), pane.line_height),
                BackingScale::ONE,
            ));
            pane.file_preview = FilePreviewPresenter::new(Box::new(RecordingFilePreviewPanel {
                previews: Rc::clone(&previews),
                dismissals: Rc::new(Cell::new(0)),
            }));
            let menu = TerminalContextMenuState {
                generation: pane.screen.generation,
                position: SurfacePosition::default(),
                link: Some(local_link.clone()),
                selection_present: true,
                file_preview_eligible: true,
            };
            assert_eq!(
                pane.context_menu_actions(&menu),
                NativeContextActions {
                    copy: true,
                    open_link: false,
                    file_preview: false,
                }
            );
            pane.perform_context_menu_command(
                menu,
                TerminalContextMenuCommand::FilePreview,
                window,
                cx,
            );
            pane.insert_dropped_file_paths_for_test(std::slice::from_ref(&file), window, cx);
        });
    });
    cx.run_until_parked();
    cx.write_to_clipboard(ClipboardItem::new_string("clipboard text".to_owned()));
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.paste_clipboard(&PasteClipboard, window, cx);
        });
    });
    cx.run_until_parked();
    let origin = current_native_service_origin(&pane, cx);
    let inserted = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.insert_native_service_text(origin, "ordinary text".to_owned(), window, cx)
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.open_find(&OpenTerminalFind, window, cx);
        });
    });

    assert!(inserted);
    assert_eq!(previews.get(), 0);
    assert!(pane.read_with(cx, |pane, _| pane.find_input.is_some()));
    let generation = pane.read_with(cx, |pane, _| pane.screen.generation);
    assert_eq!(
        activated_link(
            TerminalLocalFileCapabilities::Disabled,
            generation,
            &web_link,
            generation,
            Some(&web_link),
            true,
        ),
        Some("https://example.test".to_owned())
    );
    assert!(records.commands().iter().any(|call| {
        call.command == RecordedSessionCommand::RequestPaste("ordinary text".to_owned())
    }));
    assert!(records.commands().iter().any(|call| {
        call.command == RecordedSessionCommand::RequestPaste("clipboard text".to_owned())
    }));
    assert!(records.commands().iter().all(|call| {
        !matches!(
            &call.command,
            RecordedSessionCommand::RequestPaste(text)
                if text.contains(file.to_string_lossy().as_ref())
        )
    }));
    std::fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn accessibility_uses_its_bounded_latest_lane_instead_of_screen_rows(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    records
        .last_event_sender()
        .unwrap()
        .send_blocking(SessionEvent::Screen(blinking_screen()))
        .unwrap();
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.accessibility.text().is_empty()));

    let sender = records.last_accessibility_sender().unwrap();
    for text in ["stale", "latest"] {
        sender
            .force_send(Arc::new(TerminalAccessibilityModel::new(
                vec![crate::terminal::AccessibilityLine::new(
                    vec![crate::terminal::AccessibilityCell::new(text, 1, false)],
                    false,
                )],
                0..1,
                Some((0, 0)),
            )))
            .unwrap();
    }
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.accessibility.text() == "latest"));
}

#[gpui::test]
fn accessibility_models_are_applied_only_with_their_matching_screen_generation(
    cx: &mut TestAppContext,
) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    let old_screen = graphics_screen(40, 1);
    let current_screen = graphics_screen(41, 2);
    let ahead_screen = graphics_screen(42, 3);
    let stale = Arc::new(TerminalAccessibilityModel::from_screen(&old_screen));
    let ahead = Arc::new(TerminalAccessibilityModel::from_screen(&ahead_screen));

    pane.update(cx, |pane, cx| {
        assert!(pane.handle_event(SessionEvent::Screen(current_screen), cx));
        let epoch = pane.terminal_session.session_epoch;
        pane.handle_session_accessibility(epoch, Arc::clone(&stale));
        assert!(!pane.accessibility.shares_snapshot(&stale));

        pane.handle_session_accessibility(epoch, Arc::clone(&ahead));
        assert!(!pane.accessibility.shares_snapshot(&ahead));
        assert!(pane.handle_event(SessionEvent::Screen(ahead_screen), cx));
        assert!(pane.accessibility.shares_snapshot(&ahead));
    });
}

fn accessibility_model(index: usize) -> Arc<TerminalAccessibilityModel> {
    Arc::new(TerminalAccessibilityModel::new(
        vec![crate::terminal::AccessibilityLine::new(
            vec![
                crate::terminal::AccessibilityCell::new(format!("update-{index}"), 1, false),
                crate::terminal::AccessibilityCell::new("x", 1, false),
            ],
            false,
        )],
        0..1,
        Some((0, if index.is_multiple_of(2) { 0 } else { 1 })),
    ))
}

#[gpui::test]
fn accessibility_adapter_receives_construction_publication_and_teardown(cx: &mut TestAppContext) {
    let (_, cx) = terminal_pane(cx);
    let factory =
        crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default();
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        Rc::new(TestTerminalSessionFactory::new(
            TestTerminalSessionRecords::default(),
        )),
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-accessibility-test",
        )),
    );
    let pane = cx.update(|window, cx| {
        cx.new(|cx| {
            TerminalPane::new_with_prepared_launch(
                session_factory.clone(),
                session_factory.prepare_child_launch().unwrap(),
                crate::terminal::testing::test_terminal_key_input_adapter(),
                &factory,
                crate::terminal::native_services::testing::adapters(),
                PaneLifecycleDependencies::testing(),
                window,
                cx,
            )
        })
    });
    assert_eq!(factory.records.borrow().len(), 1);
    let record = Rc::clone(&factory.records.borrow()[0]);
    assert!(record.borrow().model.text().is_empty());
    pane.read_with(cx, |pane, _| {
        assert_eq!(record.borrow().font_family, pane.font_family.as_ref());
        assert_eq!(record.borrow().font_size, px(pane.font_size));
    });
    let model = accessibility_model(42);
    let bounds = Bounds::new(point(px(13.0), px(27.0)), size(px(300.0), px(200.0)));
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| {
            pane.grid_bounds = Some(bounds);
            pane.cell_width = px(9.5);
            pane.line_height = 21.0;
            pane.handle_accessibility(Arc::clone(&model));
            pane.set_accessibility_hierarchy(true, 7);
            pane.sync_native_accessibility(window, true);
        })
    });
    {
        let record = record.borrow();
        assert!(record.model.shares_snapshot(&model));
        assert_eq!(record.bounds, Some(bounds));
        assert_eq!((record.cell_width, record.line_height), (px(9.5), px(21.0)));
        assert!(record.focused && record.visible);
        assert_eq!(record.hierarchy, [(true, 7)]);
    }
    pane.update(cx, |pane, _| pane.close());
    assert_eq!(record.borrow().hierarchy.last(), Some(&(false, usize::MAX)));
    assert!(!record.borrow().visible && !record.borrow().focused);
    cx.update(|_, _| drop(pane));
    cx.run_until_parked();
    assert!(record.borrow().dropped);
    assert!(record.borrow().selection_sender.is_none());
}

#[gpui::test]
fn accessibility_selection_authority_follows_remote_restart_hierarchy_and_close(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_remote_terminal_pane(cx);
    let record = prepare_accessibility_presentation(&pane, cx);
    let request = record.borrow().model.selection_request(0..0).unwrap();
    let predecessor = record
        .borrow()
        .selection_sender
        .clone()
        .expect("Pane must publish Selection authority");
    predecessor.request(request.clone());
    assert_eq!(
        records.accessibility_selection_requests(1),
        std::slice::from_ref(&request)
    );

    pane.update(cx, |pane, _| {
        pane.set_accessibility_hierarchy(false, usize::MAX)
    });
    assert!(record.borrow().selection_sender.is_none());
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false))
    });
    assert!(
        record.borrow().selection_sender.is_none(),
        "hidden publication must not restore authority"
    );
    pane.update(cx, |pane, _| {
        pane.set_accessibility_hierarchy(false, usize::MAX)
    });
    assert!(record.borrow().selection_sender.is_none());
    pane.update(cx, |pane, _| pane.set_accessibility_hierarchy(true, 0));
    cx.update(|window, cx| pane.update(cx, |pane, _| pane.sync_native_accessibility(window, true)));
    assert!(record.borrow().selection_sender.is_some());

    let factory = pane.read_with(cx, |pane, _| pane.terminal_session.session_factory.clone());
    pane.update(cx, |pane, cx| pane.disconnect_remote(7, cx).unwrap());
    assert_eq!(
        cx.executor()
            .block(factory.revalidate_remote_child_launch().unwrap()),
        Ok(())
    );
    let prepared_launch = factory.prepare_child_launch().unwrap();
    let prepared = pane
        .read_with(cx, |pane, _| {
            pane.prepare_remote_restart(factory, 8, prepared_launch)
        })
        .unwrap();
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.commit_remote_restart(prepared, window, cx).unwrap()
        })
    });
    cx.run_until_parked();
    cx.update(|window, cx| pane.update(cx, |pane, _| pane.sync_native_accessibility(window, true)));
    predecessor.request(request.clone());
    assert!(records.accessibility_selection_requests(2).is_empty());
    let successor = record
        .borrow()
        .selection_sender
        .clone()
        .expect("restart must publish successor authority");
    successor.request(request.clone());
    assert_eq!(
        records.accessibility_selection_requests(2),
        std::slice::from_ref(&request)
    );
    assert_eq!(records.dropped_session_ids(), [1]);
    pane.update(cx, |pane, _| pane.close());
    assert!(record.borrow().selection_sender.is_none());
    successor.request(request);
    assert!(records.accessibility_selection_requests(2).is_empty());
    assert_eq!(records.dropped_session_ids(), [1, 2]);
}

fn prepare_accessibility_presentation(
    pane: &Entity<TerminalPane>,
    cx: &mut VisualTestContext,
) -> Rc<std::cell::RefCell<crate::platform::terminal_accessibility::testing::AccessibilityRecord>> {
    let factory =
        crate::platform::terminal_accessibility::testing::RecordingAccessibilityFactory::default();
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| {
            pane.accessibility_element = factory.create(
                window,
                pane.accessibility.as_ref().clone(),
                pane.font_family.as_ref(),
                px(pane.font_size),
            );
        });
    });
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| {
            pane.set_accessibility_hierarchy(true, 0);
            pane.sync_native_accessibility(window, false);
        });
    });
    Rc::clone(&factory.records.borrow()[0])
}

fn visible_surface() -> SurfaceVisibility {
    SurfaceVisibility {
        application_active: true,
        key_window: true,
        minimized: false,
        occluded: false,
        live_resize: false,
        workspace_visible: true,
        pane_visible: true,
    }
}

#[gpui::test]
fn minimized_pane_coalesces_sustained_accessibility_updates_until_restore(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    let accessibility_record = prepare_accessibility_presentation(&pane, cx);
    pane.update(cx, |pane, _| {
        pane.render_lifecycle.update_visibility(SurfaceVisibility {
            minimized: true,
            ..visible_surface()
        });
        for index in 0..4_096 {
            pane.handle_accessibility(accessibility_model(index));
        }
    });

    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.len(),
                pane.pending_accessibility_notifications
                    .contains(AccessibilityNotification::Value),
                pane.pending_accessibility_notifications
                    .contains(AccessibilityNotification::Selection),
                pane.accessibility.text().to_owned(),
            )
        }),
        (2, true, true, "update-4095x".to_owned())
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, _| {
            pane.render_lifecycle.update_visibility(visible_surface());
            pane.sync_native_accessibility(window, false);
        });
    });
    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.is_empty(),
                accessibility_record
                    .borrow()
                    .delivered
                    .iter()
                    .collect::<Vec<_>>(),
                accessibility_record.borrow().model.text().to_owned(),
            )
        }),
        (
            true,
            vec![
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
            ],
            "update-4095x".to_owned(),
        )
    );
}

#[gpui::test]
fn occluded_pane_coalesces_sustained_accessibility_updates_until_restore(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    let accessibility_record = prepare_accessibility_presentation(&pane, cx);
    pane.update(cx, |pane, _| {
        pane.render_lifecycle.update_visibility(SurfaceVisibility {
            occluded: true,
            ..visible_surface()
        });
        for index in 0..4_096 {
            pane.handle_accessibility(accessibility_model(index));
        }
    });

    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.len(),
                pane.accessibility.text().to_owned(),
            )
        }),
        (2, "update-4095x".to_owned())
    );

    cx.update(|window, cx| {
        pane.update(cx, |pane, _| {
            pane.render_lifecycle.update_visibility(visible_surface());
            pane.sync_native_accessibility(window, false);
        });
    });
    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.is_empty(),
                accessibility_record
                    .borrow()
                    .delivered
                    .iter()
                    .collect::<Vec<_>>(),
                accessibility_record.borrow().model.text().to_owned(),
            )
        }),
        (
            true,
            vec![
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
            ],
            "update-4095x".to_owned(),
        )
    );
}

#[gpui::test]
fn zoom_hidden_pane_retains_only_bounded_accessibility_state_until_restore(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = terminal_pane(cx);
    let accessibility_record = prepare_accessibility_presentation(&pane, cx);
    let layout_bounds = pane.read_with(cx, |pane, _| {
        pane.grid_bounds.expect("initial presentation has geometry")
    });
    pane.update(cx, |pane, cx| {
        pane.set_product_focus(
            TerminalProductFocus {
                pane_visible: false,
                focused_pane: false,
                ..TerminalProductFocus::default()
            },
            cx,
        );
        pane.set_accessibility_hierarchy(false, usize::MAX);
        for index in 0..4_096 {
            pane.handle_accessibility(accessibility_model(index));
        }
    });
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false));
    });

    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.len(),
                accessibility_record.borrow().delivered.is_empty(),
                pane.accessibility.text().to_owned(),
            )
        }),
        (2, true, "update-4095x".to_owned())
    );

    pane.update(cx, |pane, cx| {
        pane.set_product_focus(TerminalProductFocus::default(), cx);
        pane.set_accessibility_hierarchy(true, 0);
    });
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false));
    });
    assert!(pane.read_with(cx, |pane, _| {
        pane.grid_bounds.is_none()
            && pane.pending_accessibility_notifications.len() == 2
            && !accessibility_record.borrow().visible
            && accessibility_record.borrow().delivered.is_empty()
            && accessibility_record.borrow().model.text() == "update-4095x"
    }));
    // Production restores geometry in on_children_prepainted before publishing
    // native accessibility. Exercise that order instead of using hidden bounds.
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.update_grid_bounds(layout_bounds, cx);
            pane.sync_native_accessibility(window, false);
        });
    });
    assert!(accessibility_record.borrow().visible);
    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.is_empty(),
                accessibility_record
                    .borrow()
                    .delivered
                    .iter()
                    .collect::<Vec<_>>(),
                accessibility_record.borrow().model.text().to_owned(),
            )
        }),
        (
            true,
            vec![
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
            ],
            "update-4095x".to_owned(),
        )
    );
}

#[gpui::test]
fn inactive_workspace_retains_only_bounded_accessibility_state_until_restore(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = terminal_pane(cx);
    let accessibility_record = prepare_accessibility_presentation(&pane, cx);
    let layout_bounds = pane.read_with(cx, |pane, _| {
        pane.grid_bounds.expect("initial presentation has geometry")
    });
    pane.update(cx, |pane, cx| {
        pane.set_product_focus(
            TerminalProductFocus {
                active_workspace: false,
                active_tab: false,
                pane_visible: false,
                focused_pane: false,
                blocker: None,
            },
            cx,
        );
        pane.set_accessibility_hierarchy(false, usize::MAX);
        for index in 0..4_096 {
            pane.handle_accessibility(accessibility_model(index));
        }
    });
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false));
    });

    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.len(),
                accessibility_record.borrow().delivered.is_empty(),
                pane.accessibility.text().to_owned(),
            )
        }),
        (2, true, "update-4095x".to_owned())
    );

    pane.update(cx, |pane, cx| {
        pane.set_product_focus(TerminalProductFocus::default(), cx);
        pane.set_accessibility_hierarchy(true, 0);
    });
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| pane.sync_native_accessibility(window, false));
    });
    assert!(pane.read_with(cx, |pane, _| {
        pane.grid_bounds.is_none()
            && pane.pending_accessibility_notifications.len() == 2
            && !accessibility_record.borrow().visible
            && accessibility_record.borrow().delivered.is_empty()
            && accessibility_record.borrow().model.text() == "update-4095x"
    }));
    // Production restores geometry in on_children_prepainted before publishing
    // native accessibility. Exercise that order instead of using hidden bounds.
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.update_grid_bounds(layout_bounds, cx);
            pane.sync_native_accessibility(window, false);
        });
    });
    assert!(accessibility_record.borrow().visible);
    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.is_empty(),
                accessibility_record
                    .borrow()
                    .delivered
                    .iter()
                    .collect::<Vec<_>>(),
                accessibility_record.borrow().model.text().to_owned(),
            )
        }),
        (
            true,
            vec![
                AccessibilityNotification::Value,
                AccessibilityNotification::Selection,
            ],
            "update-4095x".to_owned(),
        )
    );
}

#[gpui::test]
fn hidden_focus_gain_is_delivered_once_when_the_pane_becomes_presented(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    let accessibility_record = prepare_accessibility_presentation(&pane, cx);
    pane.update(cx, |pane, _| {
        pane.set_accessibility_hierarchy(false, usize::MAX);
        pane.apply_terminal_input_focus(false);
        pane.apply_terminal_input_focus(true);
    });
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| pane.sync_native_accessibility(window, true));
    });
    assert!(pane.read_with(cx, |pane, _| {
        pane.pending_accessibility_notifications
            .contains(AccessibilityNotification::Focus)
            && accessibility_record.borrow().delivered.is_empty()
    }));

    pane.update(cx, |pane, _| pane.set_accessibility_hierarchy(true, 0));
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| pane.sync_native_accessibility(window, true));
    });

    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.is_empty(),
                accessibility_record
                    .borrow()
                    .delivered
                    .iter()
                    .collect::<Vec<_>>(),
            )
        }),
        (true, vec![AccessibilityNotification::Focus])
    );
}

#[gpui::test]
fn focus_out_and_in_between_presentations_delivers_one_retained_focus_notification(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = terminal_pane(cx);
    let accessibility_record = prepare_accessibility_presentation(&pane, cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, _| {
            pane.apply_terminal_input_focus(true);
            pane.sync_native_accessibility(window, true);
        });
    });

    cx.update(|window, cx| {
        pane.update(cx, |pane, pane_cx| {
            pane.set_product_focus(
                TerminalProductFocus {
                    blocker: Some(TerminalFocusBlocker::Modal),
                    ..TerminalProductFocus::default()
                },
                pane_cx,
            );
            pane.set_product_focus(TerminalProductFocus::default(), pane_cx);
            assert!(pane.synchronize_terminal_input_focus(window, pane_cx));
            pane.sync_native_accessibility(window, true);
        });
    });

    assert_eq!(
        pane.read_with(cx, |pane, _| {
            (
                pane.pending_accessibility_notifications.is_empty(),
                accessibility_record
                    .borrow()
                    .delivered
                    .iter()
                    .collect::<Vec<_>>(),
            )
        }),
        (true, vec![AccessibilityNotification::Focus])
    );
}

fn connected_terminal_pane_with_key_propagation(
    cx: &mut TestAppContext,
) -> (
    Entity<TerminalPane>,
    &mut VisualTestContext,
    TestTerminalSessionRecords,
    Rc<Cell<usize>>,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()));
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-terminal-pane-keyboard-propagation-test",
        )),
    );
    let propagated_key_downs = Rc::new(Cell::new(0));
    let propagated_for_probe = Rc::clone(&propagated_key_downs);
    let (probe, cx) = cx.add_window_view(|window, cx| {
        let pane = cx.new(|cx| TerminalPane::new(session_factory, window, cx));
        KeyPropagationProbe {
            pane,
            propagated_key_downs: propagated_for_probe,
        }
    });
    let pane = probe.read_with(cx, |probe, _| probe.pane.clone());
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, _| pane.focus(window));
    });
    cx.run_until_parked();
    (pane, cx, records, propagated_key_downs)
}

fn terminal_pane_with_selection_copy(
    cx: &mut TestAppContext,
    copy: SelectionCopy,
) -> (
    Entity<TerminalPane>,
    &mut VisualTestContext,
    TestTerminalSessionRecords,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
        TestTerminalSessionFactory::new(records.clone())
            .with_selection_copy_response(Ok(Some(copy))),
    );
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-terminal-pane-copy-test",
        )),
    );
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, _cx| pane.focus(window));
    });
    cx.run_until_parked();
    (pane, cx, records)
}

fn terminal_pane_with_paste_response(
    cx: &mut TestAppContext,
    response: Result<PasteRequestOutcome, String>,
    resolution: Result<PasteResolution, String>,
) -> (
    Entity<TerminalPane>,
    &mut VisualTestContext,
    TestTerminalSessionRecords,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
        TestTerminalSessionFactory::new(records.clone())
            .with_paste_response(response)
            .with_paste_resolution(resolution),
    );
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-terminal-pane-paste-test",
        )),
    );
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    cx.update(|window, cx| {
        window.activate_window();
        pane.update(cx, |pane, _cx| pane.focus(window));
    });
    cx.run_until_parked();
    (pane, cx, records)
}

fn current_native_service_origin(
    pane: &Entity<TerminalPane>,
    cx: &mut VisualTestContext,
) -> NativeServiceOrigin {
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.native_service_status(
                WorkspaceId::new(1),
                TabId::new(1),
                PaneId::new(1),
                pane.native_service_hierarchy_generation,
                window,
                cx,
            )
            .origin
            .expect("the connected test terminal must expose its Service origin")
        })
    })
}

#[gpui::test]
fn command_equals_should_increase_terminal_font_size(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    let before = pane.read_with(cx, |pane, _cx| pane.font_size());

    cx.simulate_keystrokes("cmd-=");
    let after = pane.read_with(cx, |pane, _cx| pane.font_size());

    assert_eq!((before, after), (18.0, 19.0));
}

#[gpui::test]
fn font_size_changes_notify_accessibility_when_terminal_text_is_static(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let accessibility = Arc::clone(&pane.accessibility);
            pane.pending_accessibility_notifications = AccessibilityNotifications::default();
            pane.set_font_size(15.0, window, cx);

            assert!(Arc::ptr_eq(&pane.accessibility, &accessibility));
            assert_eq!(
                pane.pending_accessibility_notifications
                    .iter()
                    .collect::<Vec<_>>(),
                vec![AccessibilityNotification::Value]
            );
        });
    });
}

fn terminal_find_input(
    pane: &Entity<TerminalPane>,
    cx: &mut VisualTestContext,
) -> Entity<TextInput> {
    pane.read_with(cx, |pane, _| {
        pane.find_input
            .clone()
            .expect("Terminal Find should remain open")
    })
}

fn replace_terminal_find_input(input: &Entity<TextInput>, text: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_text_in_range(None, text, window, cx);
        });
    });
}

#[gpui::test]
fn terminal_find_open_edit_navigate_and_close_are_pane_scoped(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);

    cx.dispatch_action(OpenTerminalFind);
    let input = terminal_find_input(&pane, cx);
    replace_terminal_find_input(&input, "日本", cx);
    cx.dispatch_action(FindNext);
    cx.dispatch_action(FindPrevious);
    cx.dispatch_action(CloseTerminalFind);

    let commands = records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedSessionCommand::SetFindQuery(generation, query) => {
                Some((format!("set:{query}"), generation))
            }
            RecordedSessionCommand::NavigateFind(generation, FindDirection::Next) => {
                Some(("next".to_owned(), generation))
            }
            RecordedSessionCommand::NavigateFind(generation, FindDirection::Previous) => {
                Some(("previous".to_owned(), generation))
            }
            RecordedSessionCommand::EndFind(generation) => Some(("end".to_owned(), generation)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        commands
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["set:", "set:日本", "next", "previous", "end"]
    );
    assert!(commands[0].1 < commands[1].1);
    assert_eq!(commands[1].1, commands[2].1);
    assert_eq!(commands[2].1, commands[3].1);
    assert!(commands[3].1 < commands[4].1);
    assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
}

#[gpui::test]
fn terminal_find_should_handle_native_select_all_and_cut(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    let input = terminal_find_input(&pane, cx);
    replace_terminal_find_input(&input, "needle", cx);
    let command_count = records.commands().len();

    cx.dispatch_action(spaceterm_ui::EditSelectAll);
    assert_eq!(
        input.read_with(cx, |input, _| input.selection().range()),
        0..6
    );
    cx.dispatch_action(spaceterm_ui::EditCopy);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("needle".to_owned())
    );
    assert!(
        records
            .commands()
            .into_iter()
            .skip(command_count)
            .all(|call| !matches!(call.command, RecordedSessionCommand::RequestSelectionCopy))
    );

    cx.dispatch_action(spaceterm_ui::EditCut);

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("needle".to_owned())
    );
    assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");
}

#[gpui::test]
fn terminal_find_should_handle_native_undo_and_redo(cx: &mut TestAppContext) {
    let (pane, cx, _) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    let input = terminal_find_input(&pane, cx);
    replace_terminal_find_input(&input, "needle", cx);

    cx.dispatch_action(spaceterm_ui::EditUndo);
    assert_eq!(input.read_with(cx, |input, _| input.value().to_owned()), "");

    cx.dispatch_action(spaceterm_ui::EditRedo);
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "needle"
    );
}

#[gpui::test]
fn repeated_terminal_find_selects_and_refocuses_the_existing_query(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    let input = terminal_find_input(&pane, cx);
    replace_terminal_find_input(&input, "needle", cx);
    cx.update(|window, cx| pane.update(cx, |pane, _| pane.focus(window)));

    cx.dispatch_action(OpenTerminalFind);

    let reopened = terminal_find_input(&pane, cx);
    assert_eq!(reopened.entity_id(), input.entity_id());
    assert_eq!(
        reopened.read_with(cx, |input, _| input.selection().range()),
        0..6
    );
    assert!(reopened.read_with(cx, |input, _| input.is_focused()));
    assert_eq!(
        records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedSessionCommand::SetFindQuery(_, _)))
            .count(),
        2
    );
}

#[gpui::test]
fn dropped_old_terminal_find_input_cannot_change_a_later_find(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    let old_input = terminal_find_input(&pane, cx);
    cx.dispatch_action(CloseTerminalFind);
    cx.dispatch_action(OpenTerminalFind);
    let new_input = terminal_find_input(&pane, cx);
    let command_count = records.commands().len();

    replace_terminal_find_input(&old_input, "stale", cx);

    assert_ne!(old_input.entity_id(), new_input.entity_id());
    assert_eq!(
        new_input.read_with(cx, |input, _| input.value().to_owned()),
        ""
    );
    assert_eq!(records.commands().len(), command_count);
}

#[gpui::test]
fn losing_focused_pane_status_closes_terminal_find(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.set_product_focus(
            TerminalProductFocus {
                active_workspace: true,
                active_tab: true,
                focused_pane: true,
                ..TerminalProductFocus::default()
            },
            cx,
        );
    });
    cx.dispatch_action(OpenTerminalFind);

    pane.update(cx, |pane, cx| {
        pane.set_product_focus(
            TerminalProductFocus {
                active_workspace: true,
                active_tab: true,
                focused_pane: false,
                ..TerminalProductFocus::default()
            },
            cx,
        );
    });

    assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
    assert!(
        records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedSessionCommand::EndFind(_)))
    );
}

#[gpui::test]
fn terminal_find_renders_shared_input_and_moves_responder_focus(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let command_count = records.commands().len();
    let focus_epoch = pane.read_with(cx, |pane, _| pane.native_service_focus_epoch.get());

    cx.dispatch_action(OpenTerminalFind);
    cx.run_until_parked();
    let input = terminal_find_input(&pane, cx);

    assert!(cx.debug_bounds("terminal-find-bar").is_some());
    let input_bounds = cx
        .debug_bounds("terminal-find-input")
        .expect("the shared Find input should be rendered");
    assert!(
        input_bounds.size.width > px(0.0) && input_bounds.size.height > px(0.0),
        "the shared Find input collapsed inside its context-menu decorator: {input_bounds:?}"
    );
    assert!(input.read_with(cx, |input, _| input.is_focused()));
    assert!(pane.read_with(cx, |pane, _| pane.native_service_focus_epoch.get()) > focus_epoch);
    assert!(cx.update(|window, app| {
        pane.read_with(app, |pane, _| !pane.focus_handle.is_focused(window))
    }));

    cx.dispatch_action(CloseTerminalFind);
    cx.simulate_keystrokes("a");
    cx.run_until_parked();
    let commands = records
        .commands()
        .into_iter()
        .skip(command_count)
        .filter_map(|call| match call.command {
            command @ (RecordedSessionCommand::Focus(_) | RecordedSessionCommand::Key(_)) => {
                Some(command)
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(matches!(commands[0], RecordedSessionCommand::Focus(false)));
    assert!(matches!(commands[1], RecordedSessionCommand::Focus(true)));
    assert!(matches!(commands[2], RecordedSessionCommand::Key(_)));
}

#[gpui::test]
fn terminal_click_restores_terminal_responder_without_closing_find(cx: &mut TestAppContext) {
    let (pane, cx, _) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    let input = terminal_find_input(&pane, cx);
    let terminal = pane.read_with(cx, |pane, _| {
        pane.grid_bounds
            .expect("Terminal grid must be measured")
            .center()
    });

    cx.simulate_click(terminal, Modifiers::none());
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.find_input.is_some()));
    assert!(!input.read_with(cx, |input, _| input.is_focused()));
    assert!(cx.update(|window, app| {
        pane.read_with(app, |pane, _| pane.focus_handle.is_focused(window))
    }));
}

#[gpui::test]
fn terminal_find_submit_navigates_and_escape_cancels(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);

    cx.simulate_keystrokes("enter escape");
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
    assert!(cx.update(|window, app| {
        pane.read_with(app, |pane, _| pane.focus_handle.is_focused(window))
    }));
    assert!(records.commands().iter().any(|call| matches!(
        call.command,
        RecordedSessionCommand::NavigateFind(_, FindDirection::Next)
    )));
    assert!(
        records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedSessionCommand::EndFind(_)))
    );
}

#[gpui::test]
fn one_escape_closes_terminal_find_during_active_composition(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    let input = terminal_find_input(&pane, cx);
    cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "に", None, window, cx);
        });
    });

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
    assert!(
        records
            .commands()
            .iter()
            .any(|call| matches!(call.command, RecordedSessionCommand::EndFind(_)))
    );
}

#[gpui::test]
fn terminal_find_return_activates_each_focused_button(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);

    for tab_count in 1..=3 {
        cx.dispatch_action(OpenTerminalFind);
        pane.update(cx, |pane, cx| {
            Arc::make_mut(&mut pane.screen).find =
                Some(Arc::new(crate::terminal::TerminalFindSnapshot {
                    generation: pane.find_generation,
                    total_matches: 1,
                    current_match: Some(1),
                    visible_spans: Arc::from([]),
                }));
            cx.notify();
        });
        cx.run_until_parked();
        let command_count = records.commands().len();

        for _ in 0..tab_count {
            cx.simulate_keystrokes("tab");
        }
        let enter = Keystroke::parse("enter").unwrap_or_default();
        cx.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            is_held: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke: enter });
        if tab_count < 3 {
            cx.simulate_keystrokes("escape");
        }
        cx.run_until_parked();

        assert!(
            pane.read_with(cx, |pane, _| pane.find_input.is_none()),
            "Escape did not close Find after tabbing to button {tab_count}"
        );
        let commands = records.commands();
        if tab_count < 3 {
            let expected_direction = if tab_count == 1 {
                FindDirection::Previous
            } else {
                FindDirection::Next
            };
            assert!(
                commands.iter().skip(command_count).any(|call| matches!(
                    call.command,
                    RecordedSessionCommand::NavigateFind(_, direction)
                        if direction == expected_direction
                )),
                "Return did not activate focused Find button {tab_count}"
            );
        }
        assert!(
            commands
                .iter()
                .skip(command_count)
                .any(|call| matches!(call.command, RecordedSessionCommand::EndFind(_))),
            "focused Close or Escape did not end Find after button {tab_count}"
        );
    }
}

#[gpui::test]
fn terminal_find_tab_delegates_to_its_composite_controls(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    let input = terminal_find_input(&pane, cx);

    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    assert!(!input.read_with(cx, |input, _| input.is_focused()));
    assert!(!cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));

    cx.simulate_keystrokes("shift-tab x");
    cx.run_until_parked();

    assert!(input.read_with(cx, |input, _| input.is_focused()));
    assert_eq!(
        input.read_with(cx, |input, _| input.value().to_owned()),
        "x"
    );
    assert!(records.commands().iter().any(|call| matches!(
        &call.command,
        RecordedSessionCommand::SetFindQuery(_, query) if query == "x"
    )));
}

#[gpui::test]
fn terminal_find_ime_candidate_geometry_comes_from_shared_input(cx: &mut TestAppContext) {
    let (pane, cx, _) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    cx.run_until_parked();
    let input = terminal_find_input(&pane, cx);
    let input_bounds = cx
        .debug_bounds("terminal-find-input")
        .expect("the shared Find input was not rendered");

    let candidate = cx.update(|window, cx| {
        input.update(cx, |input, cx| {
            input.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
            input
                .bounds_for_range(1..1, input_bounds, window, cx)
                .expect("the shared input should expose candidate geometry")
        })
    });

    assert!(input.read_with(cx, |input, _| input.composition().is_some()));
    assert!(pane.read_with(cx, |pane, _| pane.find_input.is_some()));
    assert!(candidate.left() > input_bounds.left());
    assert_eq!(candidate.top(), input_bounds.top());
    assert_eq!(candidate.bottom(), input_bounds.bottom());
}

#[gpui::test]
fn terminal_find_buttons_should_disable_navigation_without_results_and_close_find(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.dispatch_action(OpenTerminalFind);
    cx.run_until_parked();
    let command_count = records.commands().len();

    for selector in ["terminal-find-previous", "terminal-find-next"] {
        let button = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not rendered"));
        cx.simulate_click(button.center(), Modifiers::none());
    }
    let close = cx
        .debug_bounds("terminal-find-close")
        .expect("the Find close button was not rendered");
    cx.simulate_click(close.center(), Modifiers::none());
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.find_input.is_none()));
    assert!(
        records
            .commands()
            .into_iter()
            .skip(command_count)
            .all(|call| !matches!(call.command, RecordedSessionCommand::NavigateFind(_, _)))
    );
}

#[gpui::test]
fn pointer_press_should_follow_synchronous_terminal_focus_admission(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let command_count = records.commands().len();
    let position = pane.read_with(cx, |pane, _| {
        pane.grid_bounds
            .expect("Terminal grid must be measured")
            .center()
    });

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.apply_terminal_input_focus(false);
            pane.on_mouse_down(
                &MouseDownEvent {
                    position,
                    modifiers: Modifiers::none(),
                    button: MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                },
                window,
                cx,
            );
        });
    });
    let commands = records
        .commands()
        .into_iter()
        .skip(command_count)
        .map(|call| call.command)
        .collect::<Vec<_>>();

    assert!(matches!(commands[0], RecordedSessionCommand::Focus(false)));
    assert!(matches!(commands[1], RecordedSessionCommand::Focus(true)));
    assert!(matches!(
        commands[2],
        RecordedSessionCommand::Pointer(PointerInput {
            phase: PointerPhase::Press,
            ..
        })
    ));
}

#[gpui::test]
fn text_blink_uses_an_injected_clock_only_while_visible_content_demands_it(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = terminal_pane(cx);
    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(
                TerminalProductFocus {
                    active_workspace: true,
                    active_tab: true,
                    ..TerminalProductFocus::default()
                },
                cx,
            );
            pane.handle_event(SessionEvent::Screen(blinking_screen()), cx);
            cx.notify();
        });
    });
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

    cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(
                TerminalProductFocus {
                    active_workspace: true,
                    active_tab: false,
                    ..TerminalProductFocus::default()
                },
                cx,
            );
            cx.notify();
        });
    });
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));

    cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL * 2);
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
}

#[gpui::test]
fn focused_cursor_blink_uses_the_injected_pane_clock(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(
                TerminalProductFocus {
                    active_workspace: true,
                    active_tab: true,
                    focused_pane: true,
                    ..TerminalProductFocus::default()
                },
                cx,
            );
            pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
            cx.notify();
        });
    });
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

    let before = pane.read_with(cx, |pane, _| pane.grid_presentation.paint_counts());
    assert!(before.0 > 0, "the initial grid must be painted");
    let storage = pane.read_with(cx, |pane, _| pane.grid_presentation.cursor_storage());
    assert_eq!(storage.map(|(_, rows)| rows), Some(1));

    cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.grid_presentation.paint_counts()),
        before
    );

    for _ in 0..20 {
        cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
        cx.run_until_parked();
    }
    let after = pane.read_with(cx, |pane, _| pane.grid_presentation.paint_counts());
    assert_eq!(
        after.0, before.0,
        "blink frames must skip full-grid preflight and submit"
    );
    assert_eq!(
        after.1,
        before.1 + 10,
        "only visible cursor phases paint the cursor layer"
    );
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.grid_presentation.cursor_storage()),
        storage
    );
}

#[gpui::test]
fn cursor_layer_refresh_does_not_repeat_a_completed_presentation(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| {
        pane.grid_presentation.cursor_storage().is_some()
    }));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.pane_state.clone()),
        PaneTerminalState::Running
    );
}

#[gpui::test]
fn product_hiding_releases_cursor_resources_before_redraw_and_restores_latest_screen(
    cx: &mut TestAppContext,
) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
        cx.notify();
    });
    cx.run_until_parked();
    let visible = pane.read_with(cx, |pane, _| pane.product_focus);
    for (index, hidden) in [
        TerminalProductFocus {
            active_tab: false,
            ..visible
        },
        TerminalProductFocus {
            active_workspace: false,
            ..visible
        },
        TerminalProductFocus {
            pane_visible: false,
            ..visible
        },
    ]
    .into_iter()
    .enumerate()
    {
        let previous_resources =
            pane.read_with(cx, |pane, _| pane.grid_presentation.resource_liveness());
        assert_eq!(previous_resources(), (true, true));

        pane.update(cx, |pane, cx| {
            pane.set_product_focus(hidden, cx);
            assert_eq!(pane.grid_presentation.resource_liveness()(), (false, false));
            assert!(pane.grid_bounds.is_none());
            assert!(pane.latest_presentation_operation.is_none());
        });
        // Hidden product branches need not render again. GPUI may hold its old
        // view until a frame boundary, but our shared cursor batch must be gone.
        assert!(!previous_resources().1);

        let mut latest = blinking_cursor_screen(true, true);
        Arc::make_mut(&mut latest).generation =
            crate::terminal::PresentationGeneration::test(42 + index as u64);
        pane.update(cx, |pane, cx| {
            pane.handle_event(SessionEvent::Screen(Arc::clone(&latest)), cx);
            pane.set_product_focus(visible, cx);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| {
            Arc::ptr_eq(&pane.last_valid_screen, &latest)
                && pane.grid_presentation.cursor_storage().is_some()
        }));
    }
}

#[gpui::test]
fn occlusion_releases_the_cursor_batch_before_another_frame(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
        cx.notify();
    });
    cx.run_until_parked();
    let previous_resources =
        pane.read_with(cx, |pane, _| pane.grid_presentation.resource_liveness());
    assert_eq!(previous_resources(), (true, true));
    pane.update(cx, |pane, cx| {
        pane.update_runtime_visibility(
            WindowVisibility {
                minimized: false,
                occluded: true,
                live_resize: false,
            },
            cx,
        );
    });
    assert!(!previous_resources().1);
    assert!(pane.read_with(cx, |pane, _| {
        pane.grid_presentation.cursor_storage().is_none()
    }));
}

#[gpui::test]
fn cursor_layer_rebuilds_for_output_selection_and_font_changes(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    let mut screen = text_screen(10, &["first row", "cursor row", "last row"]);
    Arc::make_mut(&mut screen).cursor = blinking_cursor_screen(true, true).cursor;
    pane.update(cx, |pane, cx| {
        pane.handle_event(SessionEvent::Screen(screen.clone()), cx);
        cx.notify();
    });
    cx.run_until_parked();
    let mut paints = pane.read_with(cx, |pane, _| pane.grid_presentation.paint_counts().0);
    assert!(paints > 0);

    // A new Selection and output arrive in the same frame as a blink. Snapshot
    // identity must win over the otherwise reusable cursor phase.
    let changed = Arc::make_mut(&mut screen);
    changed.generation = crate::terminal::PresentationGeneration::test(11);
    let rows = Arc::make_mut(&mut changed.rows);
    Arc::make_mut(&mut rows[0])[0].selected = true;
    Arc::make_mut(&mut rows[2])[0].text = "changed".to_owned();
    pane.update(cx, |pane, cx| {
        pane.blink_phase_visible = !pane.blink_phase_visible;
        pane.handle_event(SessionEvent::Screen(screen), cx);
        cx.notify();
    });
    cx.run_until_parked();
    let after_output = pane.read_with(cx, |pane, _| pane.grid_presentation.paint_counts().0);
    assert!(
        after_output > paints,
        "{:?}",
        pane.read_with(cx, |pane, _| (
            pane.screen.generation,
            pane.last_valid_screen.generation,
            pane.pane_state.clone(),
            pane.grid_presentation.cursor_storage(),
            pane.terminal_input_focus
        ))
    );
    paints = after_output;

    cx.simulate_keystrokes("cmd-=");
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.grid_presentation.paint_counts().0) > paints);
    let after_resize = pane.read_with(cx, |pane, _| pane.grid_presentation.paint_counts().0);
    cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
    cx.run_until_parked();
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.grid_presentation.paint_counts().0),
        after_resize
    );
}

#[gpui::test]
fn cursor_layer_retires_for_graphics_text_blink_and_preedit(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    let cursor = blinking_cursor_screen(true, true).cursor;
    for (index, mut screen) in [graphics_screen(20, 1), blinking_screen()]
        .into_iter()
        .enumerate()
    {
        pane.update(cx, |pane, cx| {
            let mut idle = blinking_cursor_screen(true, true);
            Arc::make_mut(&mut idle).generation =
                crate::terminal::PresentationGeneration::test(20 + index as u64 * 2);
            pane.handle_event(SessionEvent::Screen(idle), cx);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| {
            pane.grid_presentation.cursor_storage().is_some()
        }));
        let changed = Arc::make_mut(&mut screen);
        changed.cursor = cursor;
        changed.generation = crate::terminal::PresentationGeneration::test(21 + index as u64 * 2);
        pane.update(cx, |pane, cx| {
            pane.handle_event(SessionEvent::Screen(screen), cx);
            cx.notify();
        });
        cx.run_until_parked();
        assert!(pane.read_with(cx, |pane, _| {
            pane.grid_presentation.cursor_storage().is_none()
        }));
    }
    pane.update(cx, |pane, cx| {
        let mut idle = blinking_cursor_screen(true, true);
        Arc::make_mut(&mut idle).generation = crate::terminal::PresentationGeneration::test(30);
        pane.handle_event(SessionEvent::Screen(idle), cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| {
        pane.grid_presentation.cursor_storage().is_some()
    }));
    pane.update(cx, |pane, cx| {
        pane.mark_for_preedit_cache_test("かな", 2..2);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| {
        pane.grid_presentation.cursor_storage().is_none()
    }));
}

#[gpui::test]
fn cursor_blink_resets_on_accepted_input_and_focus_gain(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(
                TerminalProductFocus {
                    active_workspace: true,
                    active_tab: true,
                    focused_pane: true,
                    ..TerminalProductFocus::default()
                },
                cx,
            );
            pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
            cx.notify();
        });
    });
    cx.run_until_parked();

    cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.send_key_translation(
                KeyTranslation::Encoded(KeyInput::input_method_commit("x")),
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

    cx.executor()
        .advance_clock(PRESENTATION_BLINK_INTERVAL - Duration::from_millis(1));
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));

    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(
                TerminalProductFocus {
                    active_workspace: true,
                    active_tab: true,
                    focused_pane: false,
                    ..TerminalProductFocus::default()
                },
                cx,
            );
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(
                TerminalProductFocus {
                    active_workspace: true,
                    active_tab: true,
                    focused_pane: true,
                    ..TerminalProductFocus::default()
                },
                cx,
            );
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

    cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(!pane.read_with(cx, |pane, _| pane.blink_phase_visible));
}

#[gpui::test]
fn cursor_blink_has_no_task_when_steady_hidden_or_unfocused_and_close_cancels(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = terminal_pane(cx);
    let focused = TerminalProductFocus {
        active_workspace: true,
        active_tab: true,
        focused_pane: true,
        ..TerminalProductFocus::default()
    };

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(focused, cx);
            pane.handle_event(
                SessionEvent::Screen(blinking_cursor_screen(true, false)),
                cx,
            );
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            let mut screen = blinking_cursor_screen(false, true);
            Arc::make_mut(&mut screen).generation =
                crate::terminal::PresentationGeneration::test(2);
            pane.handle_event(SessionEvent::Screen(screen), cx);
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            let mut screen = blinking_cursor_screen(true, true);
            Arc::make_mut(&mut screen).generation =
                crate::terminal::PresentationGeneration::test(3);
            pane.handle_event(SessionEvent::Screen(screen), cx);
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(
                TerminalProductFocus {
                    focused_pane: false,
                    ..focused
                },
                cx,
            );
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));
    assert!(pane.read_with(cx, |pane, _| pane.blink_phase_visible));

    cx.update(|_window, cx| {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(focused, cx);
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_some()));

    let generation = pane.read_with(cx, |pane, _| pane.blink_generation);
    let phase = pane.read_with(cx, |pane, _| pane.blink_phase_visible);
    pane.update(cx, |pane, _| pane.close());
    assert!(pane.read_with(cx, |pane, _| pane._blink_task.is_none()));
    assert_ne!(
        pane.read_with(cx, |pane, _| pane.blink_generation),
        generation
    );
    cx.executor().advance_clock(PRESENTATION_BLINK_INTERVAL);
    cx.run_until_parked();
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.blink_phase_visible),
        phase
    );
}

#[test]
fn pointer_presentation_matches_the_effective_mouse_route() {
    let policy = ShiftSelectionPolicy::OverrideApplicationMouse;

    assert!(pointer_uses_text_cursor(false, false, policy));
    assert!(!pointer_uses_text_cursor(true, false, policy));
    assert!(pointer_uses_text_cursor(true, true, policy));
    assert!(!pointer_uses_text_cursor(
        true,
        true,
        ShiftSelectionPolicy::ReportToApplication,
    ));
}

#[test]
fn context_menu_preserves_application_mouse_tracking_and_shift_override() {
    let policy = ShiftSelectionPolicy::OverrideApplicationMouse;

    assert!(opens_terminal_context_menu(
        PointerButton::Right,
        false,
        false,
        policy,
    ));
    assert!(!opens_terminal_context_menu(
        PointerButton::Right,
        true,
        false,
        policy,
    ));
    assert!(opens_terminal_context_menu(
        PointerButton::Right,
        true,
        true,
        policy,
    ));
    assert!(!opens_terminal_context_menu(
        PointerButton::Right,
        true,
        true,
        ShiftSelectionPolicy::ReportToApplication,
    ));
    assert!(!opens_terminal_context_menu(
        PointerButton::Left,
        false,
        false,
        policy,
    ));
}

#[gpui::test]
fn command_minus_should_decrease_terminal_font_size(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    let before = pane.read_with(cx, |pane, _cx| pane.font_size());

    cx.simulate_keystrokes("cmd--");
    let after = pane.read_with(cx, |pane, _cx| pane.font_size());

    assert_eq!((before, after), (18.0, 17.0));
}

#[gpui::test]
fn command_zero_should_reset_terminal_font_size(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| pane.set_font_size(20.0, window, cx));
    });
    let before = pane.read_with(cx, |pane, _cx| pane.font_size());

    cx.simulate_keystrokes("cmd-0");
    let after = pane.read_with(cx, |pane, _cx| pane.font_size());

    assert_eq!((before, after), (20.0, DEFAULT_FONT_SIZE));
}

#[gpui::test]
fn command_actions_resolve_before_the_raw_terminal_key_handler(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let before = pane.read_with(cx, |pane, _cx| pane.font_size());

    cx.simulate_keystrokes("cmd-=");

    assert_eq!(
        pane.read_with(cx, |pane, _cx| pane.font_size()),
        before + 1.0
    );
    assert!(
        records
            .commands()
            .iter()
            .all(|call| !matches!(call.command, RecordedSessionCommand::Key(_)))
    );
}

#[gpui::test]
fn copy_action_requests_semantic_selection_and_writes_plain_text_pasteboard(
    cx: &mut TestAppContext,
) {
    let (_pane, cx, records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: "alpha\nbeta".to_owned(),
            html: Some("<pre>alpha\nbeta</pre>".to_owned()),
        },
    );

    cx.simulate_keystrokes("cmd-c");

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("alpha\nbeta".to_owned())
    );
    assert!(
        records
            .commands()
            .iter()
            .any(|call| { matches!(call.command, RecordedSessionCommand::RequestSelectionCopy) })
    );
}

#[gpui::test]
fn completed_local_selection_copies_to_the_pasteboard_after_release(cx: &mut TestAppContext) {
    let (pane, cx, records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: "selected text".to_owned(),
            html: Some("<pre>selected text</pre>".to_owned()),
        },
    );
    let position = pane.read_with(cx, |pane, _| {
        pane.grid_bounds
            .expect("terminal grid must be measured")
            .center()
    });
    let command_count = records.commands().len();

    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());

    let commands = records
        .commands()
        .into_iter()
        .skip(command_count)
        .filter(|call| {
            matches!(
                call.command,
                RecordedSessionCommand::Pointer(_)
                    | RecordedSessionCommand::PointerAndCopySelection(_)
            )
        })
        .map(|call| call.command)
        .collect::<Vec<_>>();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("selected text".to_owned())
    );
    assert!(matches!(
        commands.as_slice(),
        [
            RecordedSessionCommand::Pointer(PointerInput {
                phase: PointerPhase::Press,
                ..
            }),
            RecordedSessionCommand::PointerAndCopySelection(PointerInput {
                phase: PointerPhase::Release,
                ..
            }),
        ]
    ));
}

#[gpui::test]
fn application_mouse_release_does_not_copy_the_existing_selection(cx: &mut TestAppContext) {
    let (pane, cx, records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: "existing selection".to_owned(),
            html: None,
        },
    );
    pane.update(cx, |pane, cx| {
        Arc::make_mut(&mut pane.screen).mouse_tracking = true;
        cx.notify();
    });
    cx.run_until_parked();
    let position = pane.read_with(cx, |pane, _| {
        pane.grid_bounds
            .expect("terminal grid must be measured")
            .center()
    });
    let command_count = records.commands().len();
    cx.write_to_clipboard(ClipboardItem::new_string("keep me".to_owned()));

    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("keep me".to_owned())
    );
    assert!(
        records
            .commands()
            .into_iter()
            .skip(command_count)
            .all(|call| !matches!(
                call.command,
                RecordedSessionCommand::PointerAndCopySelection(_)
            ))
    );
}

#[gpui::test]
fn shift_override_selection_copies_while_application_mouse_tracking_is_active(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: "shift selection".to_owned(),
            html: None,
        },
    );
    pane.update(cx, |pane, cx| {
        Arc::make_mut(&mut pane.screen).mouse_tracking = true;
        cx.notify();
    });
    cx.run_until_parked();
    let position = pane.read_with(cx, |pane, _| {
        pane.grid_bounds
            .expect("terminal grid must be measured")
            .center()
    });
    let modifiers = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let command_count = records.commands().len();

    cx.simulate_mouse_down(position, MouseButton::Left, modifiers);
    cx.simulate_mouse_up(position, MouseButton::Left, modifiers);

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("shift selection".to_owned())
    );
    assert!(
        records
            .commands()
            .into_iter()
            .skip(command_count)
            .any(|call| matches!(
                call.command,
                RecordedSessionCommand::PointerAndCopySelection(_)
            ))
    );
}

#[gpui::test]
fn empty_completed_selection_preserves_the_pasteboard(cx: &mut TestAppContext) {
    let (pane, cx, _records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: String::new(),
            html: None,
        },
    );
    let position = pane.read_with(cx, |pane, _| {
        pane.grid_bounds
            .expect("terminal grid must be measured")
            .center()
    });
    cx.write_to_clipboard(ClipboardItem::new_string("keep me".to_owned()));

    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::none());

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("keep me".to_owned())
    );
}

#[gpui::test]
fn copy_updates_the_pasteboard_before_the_action_returns(cx: &mut TestAppContext) {
    let (pane, cx, _records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: "new selection".to_owned(),
            html: None,
        },
    );
    cx.write_to_clipboard(ClipboardItem::new_string("old clipboard".to_owned()));

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.copy_selection(&CopySelection, window, cx);
        });
    });

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("new selection".to_owned())
    );
}

#[gpui::test]
fn native_service_selection_uses_the_ordered_terminal_selection_query(cx: &mut TestAppContext) {
    let (pane, cx, records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: "authoritative selection".to_owned(),
            html: None,
        },
    );

    let origin = current_native_service_origin(&pane, cx);
    let selection = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.native_service_selection(origin, window, cx)
        })
    });
    let requested_selection = records
        .commands()
        .iter()
        .any(|call| matches!(call.command, RecordedSessionCommand::RequestSelectionCopy));

    assert_eq!(
        (selection.map(|copy| copy.plain_text), requested_selection),
        (Some("authoritative selection".to_owned()), true),
    );
}

#[gpui::test]
fn native_service_return_routes_through_paste_payload_instead_of_ime(cx: &mut TestAppContext) {
    let confirmation = PasteConfirmation {
        id: crate::terminal::PasteConfirmationId::new(17),
        byte_len: 12,
        line_count: 2,
        risk: crate::terminal::PasteRisk {
            multiline: true,
            control_bytes: false,
            closing_fence: false,
        },
    };
    let (pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
        Ok(PasteResolution::Cancelled),
    );
    let origin = current_native_service_origin(&pane, cx);

    let accepted = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.insert_native_service_text(origin, "first\nsecond".to_owned(), window, cx)
        })
    });
    cx.run_until_parked();
    let commands = records.commands();
    let paste_payload = commands.iter().find_map(|call| match &call.command {
        RecordedSessionCommand::RequestPaste(text) => Some(text.clone()),
        _ => None,
    });
    let ime_input = commands
        .iter()
        .any(|call| matches!(call.command, RecordedSessionCommand::Key(_)));
    let pending_confirmation = pane.read_with(cx, |pane, _| pane.pending_paste);

    assert_eq!(
        (accepted, paste_payload, ime_input, pending_confirmation,),
        (
            true,
            Some("first\nsecond".to_owned()),
            false,
            Some(confirmation),
        ),
    );
}

#[gpui::test]
fn focus_loss_before_paste_reply_cancels_stale_confirmation(cx: &mut TestAppContext) {
    let confirmation = PasteConfirmation {
        id: crate::terminal::PasteConfirmationId::new(18),
        byte_len: 12,
        line_count: 2,
        risk: crate::terminal::PasteRisk {
            multiline: true,
            control_bytes: false,
            closing_fence: false,
        },
    };
    let (pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
        Ok(PasteResolution::Cancelled),
    );
    let origin = current_native_service_origin(&pane, cx);

    let accepted = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let accepted =
                pane.insert_native_service_text(origin, "first\nsecond".to_owned(), window, cx);
            pane.open_find(&OpenTerminalFind, window, cx);
            accepted
        })
    });
    cx.run_until_parked();

    assert!(accepted);
    assert_eq!(pane.read_with(cx, |pane, _| pane.pending_paste), None);
    assert!(records.commands().iter().any(|call| {
        call.command == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Cancel)
    }));
}

#[gpui::test]
fn hierarchy_change_before_paste_reply_cancels_stale_confirmation(cx: &mut TestAppContext) {
    let confirmation = PasteConfirmation {
        id: crate::terminal::PasteConfirmationId::new(19),
        byte_len: 12,
        line_count: 2,
        risk: crate::terminal::PasteRisk {
            multiline: true,
            control_bytes: false,
            closing_fence: false,
        },
    };
    let (pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
        Ok(PasteResolution::Cancelled),
    );
    let origin = current_native_service_origin(&pane, cx);

    let accepted = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let accepted =
                pane.insert_native_service_text(origin, "first\nsecond".to_owned(), window, cx);
            pane.synchronize_native_service_hierarchy_generation(
                origin.hierarchy_generation().wrapping_add(1),
            );
            accepted
        })
    });
    cx.run_until_parked();

    assert!(accepted);
    assert_eq!(pane.read_with(cx, |pane, _| pane.pending_paste), None);
    assert!(records.commands().iter().any(|call| {
        call.command == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Cancel)
    }));
}

#[gpui::test]
fn responder_focus_away_and_back_invalidates_the_previous_service_origin(cx: &mut TestAppContext) {
    let (pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::Written),
        Ok(PasteResolution::Written),
    );
    let origin = current_native_service_origin(&pane, cx);

    let accepted = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.open_find(&OpenTerminalFind, window, cx);
            pane.focus(window);
            pane.insert_native_service_text(origin, "stale return".to_owned(), window, cx)
        })
    });

    assert!(!accepted);
    assert!(
        !records
            .commands()
            .iter()
            .any(|call| { matches!(call.command, RecordedSessionCommand::RequestPaste(_)) })
    );
}

#[gpui::test]
fn native_service_return_is_rejected_without_terminal_input_focus(cx: &mut TestAppContext) {
    let (pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::Written),
        Ok(PasteResolution::Written),
    );
    let origin = current_native_service_origin(&pane, cx);
    pane.update(cx, |pane, cx| {
        pane.set_product_focus(
            TerminalProductFocus {
                active_workspace: false,
                ..TerminalProductFocus::default()
            },
            cx,
        );
    });

    let accepted = cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.insert_native_service_text(origin, "must not be inserted".to_owned(), window, cx)
        })
    });
    let requested_paste = records
        .commands()
        .iter()
        .any(|call| matches!(call.command, RecordedSessionCommand::RequestPaste(_)));

    assert_eq!((accepted, requested_paste), (false, false));
}

#[gpui::test]
fn unsafe_paste_confirmation_retains_terminal_focus_and_keeps_only_metadata_in_ui(
    cx: &mut TestAppContext,
) {
    let confirmation = PasteConfirmation {
        id: crate::terminal::PasteConfirmationId::new(7),
        byte_len: 12,
        line_count: 2,
        risk: crate::terminal::PasteRisk {
            multiline: true,
            control_bytes: false,
            closing_fence: false,
        },
    };
    let (pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
        Ok(PasteResolution::Written),
    );
    cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".to_owned()));

    cx.dispatch_action(PasteClipboard);
    cx.run_until_parked();

    assert_eq!(
        pane.read_with(cx, |pane, _| pane.pending_paste),
        Some(confirmation)
    );
    assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
    assert!(
        records
            .commands()
            .iter()
            .any(|call| { matches!(call.command, RecordedSessionCommand::RequestPaste(_)) })
    );

    let confirm = cx
        .debug_bounds("confirm-unsafe-paste")
        .expect("unsafe Paste should expose its confirmation button");
    cx.simulate_click(confirm.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(records.commands().iter().any(|call| {
        call.command
            == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Confirm)
    }));
    assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
}

#[gpui::test]
fn unsafe_paste_prompt_enter_should_confirm_without_moving_responder_focus(
    cx: &mut TestAppContext,
) {
    let confirmation = PasteConfirmation {
        id: crate::terminal::PasteConfirmationId::new(6),
        byte_len: 12,
        line_count: 2,
        risk: crate::terminal::PasteRisk {
            multiline: true,
            control_bytes: false,
            closing_fence: false,
        },
    };
    let (pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
        Ok(PasteResolution::Written),
    );
    cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".to_owned()));
    cx.dispatch_action(PasteClipboard);
    cx.run_until_parked();

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(records.commands().iter().any(|call| {
        call.command
            == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Confirm)
    }));
    assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
}

#[gpui::test]
fn unsafe_paste_prompt_escape_should_cancel_without_moving_responder_focus(
    cx: &mut TestAppContext,
) {
    let confirmation = PasteConfirmation {
        id: crate::terminal::PasteConfirmationId::new(8),
        byte_len: 12,
        line_count: 2,
        risk: crate::terminal::PasteRisk {
            multiline: true,
            control_bytes: false,
            closing_fence: false,
        },
    };
    let (_pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
        Ok(PasteResolution::Cancelled),
    );
    cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".to_owned()));
    cx.dispatch_action(PasteClipboard);
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(records.commands().iter().any(|call| {
        call.command == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Cancel)
    }));
}

#[gpui::test]
fn losing_product_focus_cancels_pending_paste_without_confirming_it(cx: &mut TestAppContext) {
    let confirmation = PasteConfirmation {
        id: crate::terminal::PasteConfirmationId::new(9),
        byte_len: 12,
        line_count: 2,
        risk: crate::terminal::PasteRisk {
            multiline: true,
            control_bytes: false,
            closing_fence: false,
        },
    };
    let (pane, cx, records) = terminal_pane_with_paste_response(
        cx,
        Ok(PasteRequestOutcome::ConfirmationRequired(confirmation)),
        Ok(PasteResolution::Cancelled),
    );
    cx.write_to_clipboard(ClipboardItem::new_string("first\nsecond".to_owned()));
    cx.dispatch_action(PasteClipboard);
    cx.run_until_parked();

    pane.update(cx, |pane, cx| {
        pane.set_product_focus(
            TerminalProductFocus {
                active_workspace: false,
                ..TerminalProductFocus::default()
            },
            cx,
        );
    });
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.pending_paste.is_none()));
    assert!(records.commands().iter().any(|call| {
        call.command == RecordedSessionCommand::ResolvePaste(confirmation.id, PasteDecision::Cancel)
    }));
}

#[gpui::test]
fn raw_key_down_and_key_up_reach_the_session_as_distinct_actions(cx: &mut TestAppContext) {
    let (_pane, cx, records) = connected_terminal_pane(cx);
    let keystroke = Keystroke {
        key: "a".to_owned(),
        key_char: Some("a".to_owned()),
        modifiers: Modifiers::default(),
    };

    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
    });
    cx.simulate_event(KeyUpEvent { keystroke });

    let actions = records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Key(input) => Some(input.action),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(actions, [KeyAction::Press, KeyAction::Release]);
}

#[gpui::test]
fn closed_combo_box_control_navigation_bindings_reach_terminal_input(cx: &mut TestAppContext) {
    let (_pane, cx, records) = connected_terminal_pane(cx);
    cx.update(|_, cx| {
        spaceterm_ui::install_combo_box_keybindings(
            cx,
            spaceterm_ui::ComboBoxKeybindingProfile::MacOs,
        );
    });
    let key_count_before = records
        .commands()
        .iter()
        .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
        .count();

    cx.simulate_keystrokes("ctrl-n ctrl-p");
    cx.run_until_parked();

    let key_count_after = records
        .commands()
        .iter()
        .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
        .count();
    assert_eq!(key_count_after, key_count_before + 2);
}

#[gpui::test]
fn unhandled_key_translation_preserves_pane_presentation_and_propagates(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.handle_event(SessionEvent::Screen(blinking_cursor_screen(true, true)), cx);
        pane.blink_phase_visible = false;
    });
    cx.run_until_parked();
    let (
        presentation_generation,
        pending_frame,
        blink_generation,
        blink_phase_visible,
        diagnostic_count,
    ) = pane.read_with(cx, |pane, _| {
        (
            pane.screen.generation,
            pane.render_lifecycle.take_frame(),
            pane.blink_generation,
            pane.blink_phase_visible,
            pane.diagnostics.record_count(),
        )
    });
    let key_count_before = records
        .commands()
        .iter()
        .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
        .count();

    let handled = pane.update(cx, |pane, cx| {
        pane.send_key_translation(
            KeyTranslation::Unhandled(UnhandledKeyEvent {
                kind: TerminalKeyInputEventKind::KeyDown,
                action: KeyAction::Press,
                native_key_code: Some(u16::MAX),
            }),
            cx,
        )
    });

    let key_count_after = records
        .commands()
        .iter()
        .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
        .count();
    let after = pane.read_with(cx, |pane, _| {
        (
            pane.screen.generation,
            pane.render_lifecycle.take_frame(),
            pane.blink_generation,
            pane.blink_phase_visible,
            pane.diagnostics.record_count(),
            pane.authoritative_status(),
            pane.pane_state.clone(),
        )
    });
    assert_eq!(
        (
            handled,
            key_count_before,
            key_count_after,
            after,
            cx.debug_bounds("terminal-status"),
        ),
        (
            false,
            0,
            0,
            (
                presentation_generation,
                pending_frame,
                blink_generation,
                blink_phase_visible,
                diagnostic_count + 1,
                None,
                PaneTerminalState::Running,
            ),
            None,
        )
    );
}

#[gpui::test]
fn unhandled_key_down_preserves_attention_and_propagates(cx: &mut TestAppContext) {
    let (pane, cx, _records, propagated_key_downs) =
        connected_terminal_pane_with_key_propagation(cx);
    let attention_events = Rc::new(Cell::new(0));
    let attention_events_for_subscription = Rc::clone(&attention_events);
    pane.update(cx, |_, cx| {
        cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
            if matches!(event, TerminalPaneEvent::AttentionChanged { .. }) {
                attention_events_for_subscription.set(attention_events_for_subscription.get() + 1);
            }
        })
        .detach();
    });
    pane.update(cx, |pane, cx| {
        pane.terminal_input_focus = false;
        pane.handle_event(
            SessionEvent::Attention(crate::terminal::attention::AttentionEvent::Bell),
            cx,
        );
        pane.terminal_input_focus = true;
    });
    let before = pane.read_with(cx, |pane, _| {
        (
            pane.attention.unread_count(),
            pane.attention.visual_bell(),
            pane.attention_visual,
            pane.attention_generation,
            pane._attention_task.is_some(),
            pane.diagnostics.record_count(),
        )
    });
    let attention_events_before = attention_events.get();

    cx.simulate_event(event("hyper", None, Modifiers::default()));

    let after = pane.read_with(cx, |pane, _| {
        (
            pane.attention.unread_count(),
            pane.attention.visual_bell(),
            pane.attention_visual,
            pane.attention_generation,
            pane._attention_task.is_some(),
            pane.diagnostics.record_count(),
        )
    });
    assert_eq!(
        (
            before,
            after,
            attention_events_before,
            attention_events.get(),
            propagated_key_downs.get(),
        ),
        (
            (1, true, true, before.3, true, before.5),
            (1, true, true, before.3, true, before.5 + 1),
            attention_events_before,
            attention_events_before,
            1,
        )
    );
}

#[gpui::test]
fn printable_text_without_physical_identity_reaches_the_terminal_session(cx: &mut TestAppContext) {
    let (_pane, cx, records) = connected_terminal_pane(cx);

    cx.simulate_event(event("hyper", Some("界"), Modifiers::default()));

    let inputs = records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Key(input) => Some(input),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(inputs, [KeyInput::text_input("界")]);
}

#[gpui::test]
fn authoritative_focus_transitions_share_one_deduplicated_session_path(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let focus_commands = || {
        records
            .commands()
            .into_iter()
            .filter_map(|call| match call.command {
                RecordedSessionCommand::Focus(focused) => Some(focused),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(focus_commands(), vec![false, true]);

    cx.update(|_window, app| {
        pane.update(app, |pane, app| {
            pane.set_product_focus(
                TerminalProductFocus {
                    focused_pane: false,
                    ..TerminalProductFocus::default()
                },
                app,
            );
            app.notify();
        });
    });
    cx.run_until_parked();
    assert_eq!(focus_commands(), vec![false, true, false]);

    cx.update(|_window, app| {
        pane.update(app, |pane, app| {
            pane.set_product_focus(
                TerminalProductFocus {
                    focused_pane: false,
                    ..TerminalProductFocus::default()
                },
                app,
            );
            app.notify();
        });
    });
    cx.run_until_parked();
    assert_eq!(focus_commands(), vec![false, true, false]);

    cx.update(|_window, app| {
        pane.update(app, |pane, app| {
            pane.set_product_focus(TerminalProductFocus::default(), app);
            app.notify();
        });
    });
    cx.run_until_parked();
    assert_eq!(focus_commands(), vec![false, true, false, true]);

    cx.deactivate_window();
    assert_eq!(focus_commands(), vec![false, true, false, true, false]);
}

#[gpui::test]
fn non_key_operating_system_window_should_remain_distinct_from_active_application(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let command_count = records.commands().len();
    let second_window = cx.add_window(|_, _| EmptyView);

    second_window
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    let non_key = cx.update(|window, cx| {
        assert!(cx.active_window().is_some());
        assert!(!window.is_window_active());
        pane.read(cx).terminal_input_focused(window, cx)
    });
    assert!(!non_key);

    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let focus_edges = records
        .commands()
        .into_iter()
        .skip(command_count)
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Focus(focused) => Some((call.session_id, focused)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(cx.update(|window, cx| pane.read(cx).terminal_input_focused(window, cx)));
    assert_eq!(focus_edges, [(1, false), (1, true)]);
}

#[gpui::test]
fn native_save_panel_should_block_before_prompt_and_restore_after_cancel(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let command_count = records.commands().len();

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.export_diagnostics(&ExportTerminalDiagnostics, window, cx);
        });
    });

    let blocked = cx.update(|window, cx| {
        let pane = pane.read(cx);
        (
            pane.focus_coordinator.native_dialog_open(),
            pane.terminal_input_focused(window, cx),
        )
    });
    assert_eq!(
        (blocked, cx.did_prompt_for_new_path()),
        ((true, false), true)
    );

    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();

    let restored = cx.update(|window, cx| {
        let pane = pane.read(cx);
        (
            pane.focus_coordinator.native_dialog_open(),
            pane.terminal_input_focused(window, cx),
        )
    });
    let focus_edges = records
        .commands()
        .into_iter()
        .skip(command_count)
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Focus(focused) => Some(focused),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!((restored, focus_edges), ((false, true), vec![false, true]));
}

#[gpui::test]
fn file_drop_from_inactive_app_focuses_before_requesting_paste(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.deactivate_window();

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.insert_dropped_file_paths_for_test(
                &[PathBuf::from("/tmp/a dropped file")],
                window,
                cx,
            );
        });
    });
    cx.run_until_parked();

    let mut relevant = records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Focus(true) => Some(RecordedSessionCommand::Focus(true)),
            RecordedSessionCommand::RequestPaste(text) => {
                Some(RecordedSessionCommand::RequestPaste(text))
            }
            _ => None,
        })
        .rev()
        .take(2)
        .collect::<Vec<_>>();
    relevant.reverse();
    assert_eq!(
        relevant,
        vec![
            RecordedSessionCommand::Focus(true),
            RecordedSessionCommand::RequestPaste("'/tmp/a dropped file'".to_owned()),
        ]
    );
}

fn event(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> KeyDownEvent {
    KeyDownEvent {
        keystroke: Keystroke {
            key: key.to_owned(),
            key_char: key_char.map(ToOwned::to_owned),
            modifiers,
        },
        is_held: false,
    }
}

#[test]
fn reported_terminal_title_should_replace_the_shell_fallback() {
    assert_eq!(
        normalized_pane_title("  Claude Code  ", "zsh"),
        "Claude Code"
    );
}

#[test]
fn preferred_terminal_font_is_selected_when_present() {
    let available = vec!["Menlo".to_owned(), "JetBrains Mono".to_owned()];

    assert_eq!(select_terminal_font(&available), "JetBrains Mono");
}

#[test]
fn system_monospace_font_is_selected_when_preferred_fonts_are_absent() {
    let available = vec!["Helvetica".to_owned(), "Menlo".to_owned()];

    assert_eq!(select_terminal_font(&available), "Menlo");
}

#[test]
fn ime_candidate_bounds_follow_wrapped_wide_preedit_caret() {
    let element_bounds = Bounds::new(point(px(10.0), px(20.0)), size(px(50.0), px(60.0)));
    let layout = layout_preedit("界", 0, 4, 5, 1);

    assert_eq!(
        ime_candidate_bounds(element_bounds, 5, px(10.0), px(20.0), layout.caret),
        Bounds::new(point(px(30.0), px(40.0)), size(px(10.0), px(20.0)))
    );
}

#[gpui::test]
fn unchanged_marked_text_reuses_logical_preedit_clusters(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    pane.update(cx, |pane, _| {
        pane.screen = blinking_cursor_screen(true, false);
        pane.mark_for_preedit_cache_test("かな", 2..2);
    });

    let first = pane.update(cx, |pane, _| pane.preedit_layout().unwrap());
    let second = pane.update(cx, |pane, _| pane.preedit_layout().unwrap());

    assert!(Arc::ptr_eq(&first.clusters, &second.clusters));
}

#[gpui::test]
fn marked_text_edit_replaces_logical_preedit_clusters(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    pane.update(cx, |pane, _| {
        pane.screen = blinking_cursor_screen(true, false);
        pane.mark_for_preedit_cache_test("か", 1..1);
    });
    let first = pane.update(cx, |pane, _| pane.preedit_layout().unwrap());
    pane.update(cx, |pane, _| pane.mark_for_preedit_cache_test("かな", 2..2));

    let second = pane.update(cx, |pane, _| pane.preedit_layout().unwrap());

    assert!(!Arc::ptr_eq(&first.clusters, &second.clusters));
}

#[gpui::test]
fn native_shaper_resolves_emoji_through_terminal_fallbacks(cx: &mut TestAppContext) {
    let (_pane, cx) = terminal_pane(cx);

    cx.update(|window, _cx| {
        let text = "👩\u{200d}💻";
        let run = TextRun {
            len: text.len(),
            font: crate::ui::terminal_element::terminal_cell_font(&"Menlo".into(), false, false),
            color: gpui_color(ACTIVE_THEME.terminal_foreground).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let shaped =
            window
                .text_system()
                .shape_line(text.into(), px(DEFAULT_FONT_SIZE), &[run], None);

        assert_eq!(shaped.len(), text.len());
        assert!(
            shaped
                .runs
                .iter()
                .flat_map(|run| &run.glyphs)
                .any(|glyph| glyph.is_emoji)
        );
    });
}

#[gpui::test]
fn marked_text_stays_local_and_commits_each_input_method_once(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let screen_before = pane.read_with(cx, |pane, _| pane.screen.clone());
    let committed = [
        ("´", "é"),
        ("にほん", "日本"),
        ("ㅎㅏㄴ", "한"),
        ("zhong", "中"),
        ("👩\u{200d}", "👩\u{200d}💻"),
    ];

    for (marked, commit) in committed {
        let key_count_before_mark = records
            .commands()
            .iter()
            .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
            .count();
        cx.update(|window, app| {
            pane.update(app, |pane, pane_cx| {
                pane.replace_and_mark_text_in_range(
                    None,
                    marked,
                    Some(marked.encode_utf16().count()..marked.encode_utf16().count()),
                    window,
                    pane_cx,
                );
            });
        });
        assert_eq!(
            records
                .commands()
                .iter()
                .filter(|call| matches!(call.command, RecordedSessionCommand::Key(_)))
                .count(),
            key_count_before_mark
        );
        assert!(pane.read_with(cx, |pane, _| Arc::ptr_eq(&pane.screen, &screen_before)));
        cx.update(|window, app| {
            pane.update(app, |pane, pane_cx| {
                pane.replace_text_in_range(None, commit, window, pane_cx);
            });
        });
    }

    let commits = records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Key(input) if input.is_input_method_commit() => input.text,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(commits, ["é", "日本", "한", "中", "👩\u{200d}💻"]);
}

#[gpui::test]
fn cancellation_and_focus_loss_discard_marked_text_without_bytes(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let mark = |pane: &Entity<TerminalPane>, cx: &mut VisualTestContext| {
        cx.update(|window, app| {
            pane.update(app, |pane, pane_cx| {
                pane.replace_and_mark_text_in_range(None, "かな", Some(2..2), window, pane_cx);
            });
        });
    };

    mark(&pane, cx);
    cx.update(|window, app| {
        pane.update(app, |pane, pane_cx| pane.unmark_text(window, pane_cx));
    });
    assert!(pane.read_with(cx, |pane, _| pane.ime.marked_text().is_none()));

    mark(&pane, cx);
    cx.update(|_window, app| {
        pane.update(app, |pane, app| {
            pane.set_product_focus(
                TerminalProductFocus {
                    focused_pane: false,
                    ..TerminalProductFocus::default()
                },
                app,
            );
            app.notify();
        });
    });
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.ime.marked_text().is_none()));
    assert!(
        records
            .commands()
            .iter()
            .all(|call| !matches!(call.command, RecordedSessionCommand::Key(_)))
    );
}

#[gpui::test]
fn raw_key_callbacks_are_suppressed_while_marked_text_is_active(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    cx.update(|window, app| {
        pane.update(app, |pane, pane_cx| {
            pane.replace_and_mark_text_in_range(None, "に", Some(1..1), window, pane_cx);
        });
    });

    cx.simulate_event(event("a", Some("a"), Modifiers::default()));

    assert!(
        records
            .commands()
            .iter()
            .all(|call| !matches!(call.command, RecordedSessionCommand::Key(_)))
    );

    cx.update(|window, app| {
        pane.update(app, |pane, pane_cx| {
            pane.replace_text_in_range(None, "日", window, pane_cx);
        });
    });
    cx.simulate_event(KeyUpEvent {
        keystroke: Keystroke {
            key: "a".to_owned(),
            key_char: Some("a".to_owned()),
            modifiers: Modifiers::default(),
        },
    });

    let keys = records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Key(input) => Some(input),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 1);
    assert!(keys[0].is_input_method_commit());
}

#[test]
fn empty_terminal_title_should_restore_the_shell_fallback() {
    assert_eq!(normalized_pane_title("\n\t", "zsh"), "zsh");
}

#[test]
fn pane_title_should_remove_control_characters() {
    assert_eq!(
        normalized_pane_title("cargo\u{7} test", "zsh"),
        "cargo test"
    );
}

#[test]
fn maps_rendered_positions_to_reported_terminal_geometry() {
    let bounds = Bounds::new(
        gpui::point(px(10.0), px(20.0)),
        gpui::size(px(75.0), px(40.0)),
    );
    let geometry = TerminalGeometry::from_grid(
        CellGridSize::new(10, 2),
        LogicalCellSize::new(7.5, 20.0),
        BackingScale::ONE,
    );

    let position =
        terminal_surface_position(bounds, gpui::point(px(47.5), px(30.0)), geometry, false)
            .unwrap();

    assert_eq!(position, SurfacePosition { x: 37.5, y: 10.0 });
    assert!(
        terminal_surface_position(bounds, gpui::point(px(9.0), px(20.0)), geometry, false,)
            .is_none()
    );
    assert_eq!(
        terminal_surface_position(bounds, gpui::point(px(2.5), px(65.0)), geometry, true),
        Some(SurfacePosition { x: -7.5, y: 45.0 })
    );
}

#[test]
fn accumulates_fractional_trackpad_scroll_into_terminal_steps() {
    let mut accumulator = WheelAccumulator::default();
    assert_eq!(
        accumulator.push(0.4, 0.4, WheelPhase::GestureStarted),
        (0, 0)
    );
    assert_eq!(
        accumulator.push(0.7, 0.7, WheelPhase::GestureChanged),
        (1, 1)
    );
    assert_eq!(
        accumulator.push(-1.3, -1.3, WheelPhase::MomentumStarted),
        (-1, -1)
    );
    assert_eq!(
        accumulator.push(0.0, 0.0, WheelPhase::MomentumCancelled),
        (0, 0)
    );
    assert_eq!((accumulator.horizontal, accumulator.vertical), (0.0, 0.0));
}

#[test]
fn hyperlinks_open_only_on_platform_modified_same_generation_release() {
    let link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
    let first = crate::terminal::PresentationGeneration::test(1);
    let second = crate::terminal::PresentationGeneration::test(2);

    assert_eq!(
        activated_link(
            TerminalLocalFileCapabilities::Enabled,
            first,
            &link,
            first,
            Some(&link),
            true,
        ),
        Some("https://example.test".to_owned())
    );
    assert_eq!(
        activated_link(
            TerminalLocalFileCapabilities::Enabled,
            first,
            &link,
            second,
            Some(&link),
            true,
        ),
        None
    );
    assert_eq!(
        activated_link(
            TerminalLocalFileCapabilities::Enabled,
            first,
            &link,
            first,
            None,
            true,
        ),
        None
    );
    assert_eq!(
        activated_link(
            TerminalLocalFileCapabilities::Enabled,
            first,
            &link,
            first,
            Some(&link),
            false,
        ),
        None
    );
}

#[test]
fn active_link_hover_requires_the_platform_modifier() {
    let link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
    let generation = crate::terminal::PresentationGeneration::test(1);
    let hovered = HoveredTerminalLink {
        generation,
        cell: CellGridPosition::new(0, 0),
        target: link,
    };

    assert_eq!(
        (
            active_hovered_link(Some(&hovered), generation, false),
            active_hovered_link(Some(&hovered), generation, true),
        ),
        (None, Some(&hovered))
    );
}

#[gpui::test]
fn stationary_link_hover_updates_when_the_platform_modifier_changes(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.screen = context_action_screen(
            crate::terminal::HyperlinkTarget::url("https://example.test"),
            false,
        );
        cx.notify();
    });
    cx.run_until_parked();
    let pointer = pane.read_with(cx, |pane, _| {
        let bounds = pane.grid_bounds.expect("terminal grid was painted");
        point(
            bounds.left() + pane.cell_width / 2.0,
            bounds.top() + px(pane.line_height / 2.0),
        )
    });

    cx.simulate_mouse_move(pointer, None, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-link-preview").is_none());

    cx.simulate_modifiers_change(Modifiers {
        platform: true,
        ..Modifiers::none()
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-link-preview").is_some());

    cx.simulate_modifiers_change(Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-link-preview").is_none());
}

#[test]
fn hovered_link_is_inert_after_presentation_generation_advances() {
    let link = crate::terminal::HyperlinkTarget::url("https://example.test").unwrap();
    let first = crate::terminal::PresentationGeneration::test(1);
    let second = crate::terminal::PresentationGeneration::test(2);
    let hovered = HoveredTerminalLink {
        generation: first,
        cell: CellGridPosition::new(0, 0),
        target: link,
    };

    assert!(hovered_link_for_generation(Some(&hovered), first).is_some());
    assert_eq!(hovered_link_for_generation(Some(&hovered), second), None);
    assert_eq!(
        NativeContextActions::from_presence(
            TerminalLocalFileCapabilities::Enabled,
            false,
            hovered_link_for_generation(Some(&hovered), second).map(|hovered| &hovered.target),
        ),
        NativeContextActions::default()
    );
}

#[test]
fn context_link_requires_the_clicked_generation_and_identity() {
    let clicked = crate::terminal::HyperlinkTarget::url("https://example.test/first").unwrap();
    let replacement = crate::terminal::HyperlinkTarget::url("https://example.test/second").unwrap();
    let first = crate::terminal::PresentationGeneration::test(1);
    let second = crate::terminal::PresentationGeneration::test(2);

    assert_eq!(
        revalidated_context_link(first, Some(&clicked), first, Some(&clicked)),
        Some(&clicked)
    );
    assert_eq!(
        revalidated_context_link(first, Some(&clicked), second, Some(&clicked)),
        None
    );
    assert_eq!(
        revalidated_context_link(first, Some(&clicked), first, Some(&replacement)),
        None
    );
    assert_eq!(revalidated_context_link(first, None, first, None), None);
}

#[gpui::test]
fn context_copy_requires_the_frozen_presentation_generation(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);

    let (current, stale) = pane.update(cx, |pane, _| {
        pane.screen = context_action_screen(None, true);
        let mut menu = TerminalContextMenuState {
            generation: pane.screen.generation,
            position: SurfacePosition::default(),
            link: None,
            selection_present: true,
            file_preview_eligible: false,
        };
        let current = pane.context_menu_actions(&menu);
        menu.generation = crate::terminal::PresentationGeneration::test(6);
        (current, pane.context_menu_actions(&menu))
    });

    assert!(current.copy);
    assert!(!stale.copy);
}

#[gpui::test]
fn context_menu_focus_blocker_reports_focus_out_before_focus_in(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.context_menu = Some(TerminalContextMenuState {
                generation: pane.screen.generation,
                position: SurfacePosition::default(),
                link: None,
                selection_present: false,
                file_preview_eligible: false,
            });
            pane.sync_terminal_input_focus(window, cx);
            pane.context_menu_closed(cx);
            pane.sync_terminal_input_focus(window, cx);
        });
    });

    let reports = records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Focus(focused) => Some(focused),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reports, [false, true, false, true]);
}

#[gpui::test]
fn local_right_click_opens_the_packaged_menu_and_copy_revalidates_selection(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.screen = context_action_screen(
            crate::terminal::HyperlinkTarget::url("https://example.test"),
            true,
        );
        cx.notify();
    });
    cx.run_until_parked();
    let click = pane.read_with(cx, |pane, _| {
        let bounds = pane.grid_bounds.expect("terminal grid was painted");
        point(
            bounds.origin.x + pane.cell_width / 2.0,
            bounds.origin.y + px(pane.line_height / 2.0),
        )
    });

    cx.simulate_mouse_down(click, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(click, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();

    cx.simulate_mouse_move(click, None, Modifiers::none());
    cx.simulate_event(ScrollWheelEvent {
        position: click,
        delta: ScrollDelta::Lines(point(0.0, -1.0)),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("terminal-context-menu").is_some());
    assert!(
        cx.debug_bounds("terminal-context-menu-row-copy-enabled")
            .is_some()
    );
    assert!(
        cx.debug_bounds("terminal-context-menu-row-open-link-enabled")
            .is_some()
    );
    assert!(
        cx.debug_bounds("terminal-context-menu-row-file-preview-disabled")
            .is_some()
    );
    assert!(records.commands().iter().all(|call| !matches!(
        call.command,
        RecordedSessionCommand::Pointer(_)
            | RecordedSessionCommand::PointerAndCopySelection(_)
            | RecordedSessionCommand::Wheel(_)
    )));

    let copy = cx
        .debug_bounds("terminal-context-menu-row-copy-enabled")
        .expect("Copy row was not rendered")
        .center();
    cx.simulate_mouse_move(copy, None, Modifiers::none());
    cx.simulate_click(copy, Modifiers::none());
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_none()));
    let relevant = records
        .commands()
        .into_iter()
        .filter_map(|call| match call.command {
            RecordedSessionCommand::Focus(focused) => Some(RecordedSessionCommand::Focus(focused)),
            RecordedSessionCommand::RequestSelectionCopyAt(generation) => {
                Some(RecordedSessionCommand::RequestSelectionCopyAt(generation))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        &relevant[relevant.len() - 3..],
        [
            RecordedSessionCommand::Focus(false),
            RecordedSessionCommand::Focus(true),
            RecordedSessionCommand::RequestSelectionCopyAt(
                pane.read_with(cx, |pane, _| pane.screen.generation)
            ),
        ]
    );
}

#[gpui::test]
fn context_menu_keys_never_reach_the_terminal_session(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.screen = context_action_screen(None, true);
        cx.notify();
    });
    cx.run_until_parked();
    let click = pane.read_with(cx, |pane, _| {
        let bounds = pane.grid_bounds.expect("terminal grid was painted");
        bounds.center()
    });

    cx.simulate_mouse_down(click, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(click, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_some()));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_none()));
    assert!(
        records
            .commands()
            .iter()
            .all(|call| !matches!(call.command, RecordedSessionCommand::Key(_)))
    );
}

#[gpui::test]
fn file_preview_command_revalidates_then_calls_the_retained_presenter(cx: &mut TestAppContext) {
    let directory = std::env::temp_dir().join(format!(
        "spaceterm-context-file-preview-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("preview.txt");
    std::fs::write(&file, b"preview").unwrap();
    let link = crate::terminal::HyperlinkTarget::osc8(
        "file:preview.txt",
        &directory,
        None,
        TerminalLocalFileCapabilities::Enabled,
    )
    .unwrap();
    let previews = Rc::new(Cell::new(0));
    let dismissals = Rc::new(Cell::new(0));
    let (pane, cx, _records) = connected_terminal_pane(cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.screen = context_action_screen(Some(link.clone()), false);
            pane.last_geometry = Some(TerminalGeometry::from_grid(
                CellGridSize::new(1, 1),
                LogicalCellSize::new(f32::from(pane.cell_width), pane.line_height),
                BackingScale::ONE,
            ));
            pane.file_preview = FilePreviewPresenter::new(Box::new(RecordingFilePreviewPanel {
                previews: Rc::clone(&previews),
                dismissals: Rc::clone(&dismissals),
            }));
            let menu = TerminalContextMenuState {
                generation: pane.screen.generation,
                position: SurfacePosition::default(),
                link: Some(link),
                selection_present: false,
                file_preview_eligible: true,
            };
            pane.context_menu = Some(menu.clone());
            pane.sync_terminal_input_focus(window, cx);
            pane.perform_context_menu_command(
                menu,
                TerminalContextMenuCommand::FilePreview,
                window,
                cx,
            );
        });
    });

    assert_eq!(previews.get(), 1);
    assert_eq!(dismissals.get(), 0);
    pane.update(cx, |pane, _| pane.close());
    assert_eq!(dismissals.get(), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn stale_context_generation_never_reaches_the_presenter(cx: &mut TestAppContext) {
    let directory = std::env::temp_dir().join(format!(
        "spaceterm-stale-context-file-preview-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("preview.txt");
    std::fs::write(&file, b"preview").unwrap();
    let link = crate::terminal::HyperlinkTarget::osc8(
        "file:preview.txt",
        &directory,
        None,
        TerminalLocalFileCapabilities::Enabled,
    )
    .unwrap();
    let previews = Rc::new(Cell::new(0));
    let dismissals = Rc::new(Cell::new(0));
    let (pane, cx, _records) = connected_terminal_pane(cx);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            let clicked_screen = context_action_screen(Some(link.clone()), false);
            pane.last_geometry = Some(TerminalGeometry::from_grid(
                CellGridSize::new(1, 1),
                LogicalCellSize::new(f32::from(pane.cell_width), pane.line_height),
                BackingScale::ONE,
            ));
            pane.file_preview = FilePreviewPresenter::new(Box::new(RecordingFilePreviewPanel {
                previews: Rc::clone(&previews),
                dismissals: Rc::clone(&dismissals),
            }));
            let menu = TerminalContextMenuState {
                generation: clicked_screen.generation,
                position: SurfacePosition::default(),
                link: Some(link),
                selection_present: false,
                file_preview_eligible: true,
            };
            pane.context_menu = Some(menu.clone());
            pane.screen = ScreenSnapshot::from_test_parts_at(
                clicked_screen.rows.clone(),
                ScrollbarSnapshot::default(),
                "advanced",
                8,
            );
            pane.sync_terminal_input_focus(window, cx);
            pane.perform_context_menu_command(
                menu,
                TerminalContextMenuCommand::FilePreview,
                window,
                cx,
            );
        });
    });

    assert_eq!(previews.get(), 0);
    assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_none()));
    pane.update(cx, |pane, _| pane.close());
    assert_eq!(dismissals.get(), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[gpui::test]
fn session_exit_dismisses_file_preview_and_the_context_menu(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    let dismissals = Rc::new(Cell::new(0));

    pane.update(cx, |pane, cx| {
        pane.file_preview = FilePreviewPresenter::new(Box::new(RecordingFilePreviewPanel {
            previews: Rc::new(Cell::new(0)),
            dismissals: Rc::clone(&dismissals),
        }));
        pane.context_menu = Some(TerminalContextMenuState {
            generation: pane.screen.generation,
            position: SurfacePosition::default(),
            link: None,
            selection_present: false,
            file_preview_eligible: false,
        });
        pane.handle_event(
            SessionEvent::Exited(crate::terminal::SessionExit::Success),
            cx,
        );
    });

    assert_eq!(dismissals.get(), 1);
    assert!(pane.read_with(cx, |pane, _| pane.context_menu.is_none()));
}

#[gpui::test]
fn context_copy_stays_eligible_for_offscreen_selection_presence(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);

    let actions = pane.update(cx, |pane, _| {
        let mut screen =
            ScreenSnapshot::from_test_parts(Arc::from([]), ScrollbarSnapshot::default(), "");
        Arc::make_mut(&mut screen).selection_present = true;
        pane.screen = screen;
        pane.native_context_actions()
    });

    assert!(actions.copy);
}

#[test]
fn maps_gpui_buttons_and_modifiers_to_terminal_input() {
    assert_eq!(pointer_button(MouseButton::Left), Some(PointerButton::Left));
    assert_eq!(
        pointer_button(MouseButton::Navigate(gpui::NavigationDirection::Back)),
        None
    );
    assert_eq!(
        input_modifiers(Modifiers {
            control: true,
            alt: true,
            shift: true,
            platform: true,
            function: true,
        }),
        InputModifiers {
            shift: true,
            alt: true,
            control: true,
            platform: true,
            ..InputModifiers::default()
        }
    );
}

#[gpui::test]
fn terminal_scrollbar_should_request_exact_row_offsets(cx: &mut TestAppContext) {
    let (pane, cx) = terminal_pane(cx);
    let records = TestTerminalSessionRecords::default();
    let session = TestTerminalSessionFactory::new(records.clone())
        .start(
            TerminalGeometry::from_grid(
                CellGridSize::new(80, 24),
                LogicalCellSize::new(8.0, 16.0),
                BackingScale::ONE,
            ),
            TerminalLaunchPlan::Local(LocalTerminalLaunchPlan::new(test_local_directory(
                PathBuf::from("/tmp/spaceterm-terminal-pane-test"),
            ))),
        )
        .expect("the test terminal session should start");
    let screen =
        ScreenSnapshot::from_test_parts_at(Arc::from([]), ScrollbarSnapshot::default(), "", 7);
    let generation = screen.generation;
    let scrollbar = pane.update(cx, |pane, _| {
        pane.terminal_session.session = Some(session.handle);
        pane.screen = screen;
        pane.scrollbar.clone()
    });

    scrollbar.update(cx, |_, cx| {
        cx.emit(OverlayScrollbarEvent::OffsetRequested(u64::MAX - 1));
    });
    cx.run_until_parked();

    assert_eq!(
        records.commands().last().map(|input| &input.command),
        Some(&RecordedSessionCommand::ScrollTo(u64::MAX - 1, generation,))
    );
}

#[gpui::test]
fn terminal_pane_should_ignore_an_older_screen_presentation(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let events = records.last_event_sender().unwrap();
    events
        .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts_at(
            Arc::from([]),
            ScrollbarSnapshot::default(),
            "newest",
            2,
        )))
        .unwrap();
    cx.run_until_parked();
    events
        .try_send(SessionEvent::Screen(ScreenSnapshot::from_test_parts_at(
            Arc::from([]),
            ScrollbarSnapshot::default(),
            "stale",
            1,
        )))
        .unwrap();
    cx.run_until_parked();

    let (generation, title) = pane.update(cx, |pane, _| {
        (pane.screen.generation, pane.title.to_string())
    });
    assert_eq!(
        generation,
        ScreenSnapshot::from_test_parts_at(Arc::from([]), ScrollbarSnapshot::default(), "", 2,)
            .generation
    );
    assert_eq!(title, "newest");
}

#[gpui::test]
fn backing_scale_change_should_preserve_the_grid_and_resize_backing_pixels(
    cx: &mut TestAppContext,
) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()));
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-terminal-pane-scale-test",
        )),
    );
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    cx.run_until_parked();
    let initial = records.starts()[0].geometry;

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.update_backing_scale(1.0, window, cx);
        });
    });
    cx.run_until_parked();
    let resized = records
        .commands()
        .into_iter()
        .find_map(|call| match call.command {
            RecordedSessionCommand::Resize(geometry) => Some(geometry),
            _ => None,
        })
        .expect("a backing-scale change should resize the Terminal Session");

    assert_eq!(
        (
            initial.grid(),
            resized.grid(),
            initial.backing_grid_size().width,
            resized.backing_grid_size().width,
        ),
        (
            initial.grid(),
            initial.grid(),
            resized.backing_grid_size().width * 2,
            resized.backing_grid_size().width,
        )
    );
}

#[gpui::test]
fn terminal_pane_close_should_drop_its_session_once_when_repeated(cx: &mut TestAppContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()));
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-terminal-pane-test",
        )),
    );
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    cx.run_until_parked();

    pane.update(cx, |pane, _| {
        pane.close();
        pane.close();
    });

    assert_eq!(records.dropped_session_ids(), vec![1]);
}

#[gpui::test]
fn presentation_failure_retry_preserves_and_restores_the_current_presentation(
    cx: &mut TestAppContext,
) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    let generation = pane.read_with(cx, |pane, _| pane.screen.generation);

    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.update_backing_scale(f32::NAN, window, cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.screen.generation,
            pane.pane_state.last_valid_frame(),
            pane.pane_state
                .failure()
                .map(crate::terminal::TerminalFailure::class),
            pane.terminal_session.session.is_some(),
        )),
        (
            generation,
            Some(generation),
            Some(crate::terminal::FailureClass::Presentation),
            true,
        )
    );

    let retry = cx
        .debug_bounds("retry-terminal-recovery")
        .expect("recoverable presentation failure should expose Retry");
    cx.simulate_click(retry.center(), Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.pane_state.clone(),
            pane.status.clone(),
            pane.screen.generation,
            pane.terminal_session.session.is_some(),
        )),
        (PaneTerminalState::Running, None, generation, true)
    );
}

#[gpui::test]
fn second_row_preflight_failure_submits_only_the_last_valid_generation(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let events = records.last_event_sender().unwrap();
    events
        .try_send(SessionEvent::Screen(text_screen(1, &["old", "frame"])))
        .unwrap();
    cx.run_until_parked();
    let submissions_before = pane.read_with(cx, |pane, _| pane.scene_submission_attempts.len());

    pane.update(cx, |pane, _| {
        pane.paint_fault = Some(PaintPreflightFault::Row(1));
    });
    events
        .try_send(SessionEvent::Screen(text_screen(2, &["new", "frame"])))
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.screen.generation,
            pane.last_valid_screen.generation,
            pane.pane_state.last_valid_frame(),
            pane.pane_state.failure().map(TerminalFailure::class),
        )),
        (
            crate::terminal::PresentationGeneration::test(2),
            crate::terminal::PresentationGeneration::test(1),
            Some(crate::terminal::PresentationGeneration::test(1)),
            Some(crate::terminal::FailureClass::Presentation),
        )
    );
    let submissions = pane.read_with(cx, |pane, _| {
        pane.scene_submission_attempts[submissions_before..].to_vec()
    });
    assert!(!submissions.contains(&crate::terminal::PresentationGeneration::test(2)));
    assert_eq!(
        submissions.last(),
        Some(&crate::terminal::PresentationGeneration::test(1))
    );
}

#[gpui::test]
fn candidate_and_fallback_use_isolated_render_caches(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);

    let (candidate, fallback) = pane.read_with(cx, |pane, _| {
        (
            pane.render_cache.entity_id(),
            pane.fallback_render_cache.entity_id(),
        )
    });

    assert_ne!(candidate, fallback);
}

#[gpui::test]
fn second_glyph_preflight_failure_submits_only_the_last_valid_generation(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let events = records.last_event_sender().unwrap();
    events
        .try_send(SessionEvent::Screen(text_screen(1, &["old"])))
        .unwrap();
    cx.run_until_parked();
    let submissions_before = pane.read_with(cx, |pane, _| pane.scene_submission_attempts.len());

    pane.update(cx, |pane, _| {
        pane.paint_fault = Some(PaintPreflightFault::Glyph(1));
    });
    events
        .try_send(SessionEvent::Screen(text_screen(2, &["new"])))
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.screen.generation,
            pane.last_valid_screen.generation,
            pane.pane_state.failure().map(TerminalFailure::class),
        )),
        (
            crate::terminal::PresentationGeneration::test(2),
            crate::terminal::PresentationGeneration::test(1),
            Some(crate::terminal::FailureClass::Presentation),
        )
    );
    let submissions = pane.read_with(cx, |pane, _| {
        pane.scene_submission_attempts[submissions_before..].to_vec()
    });
    assert!(!submissions.contains(&crate::terminal::PresentationGeneration::test(2)));
}

#[gpui::test]
fn second_image_preflight_failure_rolls_back_the_unpresented_generation(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let events = records.last_event_sender().unwrap();
    events
        .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
        .unwrap();
    cx.run_until_parked();
    let submissions_before = pane.read_with(cx, |pane, _| pane.scene_submission_attempts.len());

    pane.update(cx, |pane, _| {
        pane.paint_fault = Some(PaintPreflightFault::Image(1));
    });
    events
        .try_send(SessionEvent::Screen(graphics_screen_with_images(
            2,
            &[2, 3],
        )))
        .unwrap();
    cx.run_until_parked();

    let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
    assert_eq!(
        (
            pane.read_with(cx, |pane, _| pane.last_valid_screen.generation),
            pane.read_with(cx, |pane, _| {
                pane.pane_state.failure().map(TerminalFailure::class)
            }),
            cache.read_with(cx, |cache, _| cache.cached_image_keys()),
            cache.read_with(cx, |cache, _| cache.staged_image_keys()),
        ),
        (
            crate::terminal::PresentationGeneration::test(1),
            Some(crate::terminal::FailureClass::Resource),
            vec![crate::terminal::ImageKey {
                image_id: 1,
                generation: 1,
            }],
            Vec::new(),
        )
    );
    let submissions = pane.read_with(cx, |pane, _| {
        pane.scene_submission_attempts[submissions_before..].to_vec()
    });
    assert!(!submissions.contains(&crate::terminal::PresentationGeneration::test(2)));
}

#[gpui::test]
fn graphics_post_mutation_failures_remain_quota_bounded_for_changing_keys(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    records
        .last_event_sender()
        .unwrap()
        .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
        .unwrap();
    cx.run_until_parked();
    let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
    let retained = cache.read_with(cx, |cache, _| cache.retained_bytes());

    for generation in 2..18 {
        let screen = graphics_screen(generation, generation as u32);
        let result = cx.update(|window, cx| {
            cache.update(cx, |cache, cx| {
                cache.fail_after_staging();
                cache.sync(screen.active_screen, &screen.graphics, window, cx)
            })
        });
        assert!(result.is_err());
        assert_eq!(
            cache.read_with(cx, |cache, _| (
                cache.cached_image_keys(),
                cache.staged_image_keys(),
                cache.retained_bytes(),
            )),
            (
                vec![crate::terminal::ImageKey {
                    image_id: 1,
                    generation: 1,
                }],
                Vec::new(),
                retained,
            )
        );
    }
}

#[gpui::test]
fn stale_graphics_attempt_cannot_roll_back_a_newer_same_generation_stage(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    records
        .last_event_sender()
        .unwrap()
        .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
        .unwrap();
    cx.run_until_parked();
    let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
    let screen_a = graphics_screen(2, 2);
    let screen_b = graphics_screen(2, 3);
    let attempt_a = cx.update(|window, cx| {
        cache
            .update(cx, |cache, cx| {
                cache.sync(screen_a.active_screen, &screen_a.graphics, window, cx)
            })
            .unwrap()
            .token
    });
    let attempt_b = cx.update(|window, cx| {
        cache
            .update(cx, |cache, cx| {
                cache.sync(screen_b.active_screen, &screen_b.graphics, window, cx)
            })
            .unwrap()
            .token
    });

    cache.update(cx, |cache, cx| {
        assert!(!cache.rollback(attempt_a, None, cx));
        assert_eq!(
            cache.staged_image_keys(),
            vec![crate::terminal::ImageKey {
                image_id: 3,
                generation: 2,
            }]
        );
        assert!(cache.mark_presented(attempt_b, cx));
    });
    assert_eq!(
        cache.read_with(cx, |cache, _| cache.cached_image_keys()),
        vec![crate::terminal::ImageKey {
            image_id: 3,
            generation: 2,
        }]
    );
}

#[gpui::test]
fn stale_recoverable_and_export_completions_cannot_mask_a_fatal_failure(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    let (recovery, export) = pane.update(cx, |pane, _| {
        pane.present_failure(
            TerminalFailure::platform("recoverable-operation"),
            true,
            Some(RecoveryAction::CopySelection),
        );
        let recovery = pane.pending_recovery.unwrap();
        let export = pane.begin_operation(pane.screen.generation, None);
        pane.latest_export_operation = Some(export.id);
        (recovery, export)
    });
    pane.update(cx, |pane, cx| {
        pane.handle_event(
            SessionEvent::Failed(SessionFailure::PtyRead {
                read_error: "unavailable".to_owned(),
                exit_status: "exit code 7".to_owned(),
            }),
            cx,
        );
        assert!(!pane.clear_recovery(recovery));
        pane.finish_export(export, Ok(()), PathBuf::from("stale"), cx);
        pane.finish_export(
            export,
            Err(std::io::Error::other("stale failure")),
            PathBuf::from("stale"),
            cx,
        );
        pane.handle_event(
            SessionEvent::Exited(crate::terminal::SessionExit::Success),
            cx,
        );
    });

    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.pane_state.failure().map(TerminalFailure::class),
            pane.pending_recovery,
            pane.authoritative_status(),
        )),
        (
            Some(crate::terminal::FailureClass::Pty),
            None,
            Some(
                "PTY failed during read-shell-output. Close this Pane and restart the terminal command."
                    .to_owned(),
            ),
        )
    );
}

#[gpui::test]
fn newer_same_action_and_export_tokens_reject_stale_completions(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.present_failure_at(
            TerminalFailure::resource("first-renderer-attempt"),
            true,
            Some(RecoveryAction::RendererResources),
            crate::terminal::PresentationGeneration::test(1),
        );
        let first_recovery = pane.pending_recovery.unwrap();
        pane.present_failure_at(
            TerminalFailure::resource("second-renderer-attempt"),
            true,
            Some(RecoveryAction::RendererResources),
            crate::terminal::PresentationGeneration::test(2),
        );
        let second_recovery = pane.pending_recovery.unwrap();
        assert!(!pane.clear_recovery(first_recovery));
        assert_eq!(pane.pending_recovery, Some(second_recovery));

        let first_export = pane.begin_operation(pane.screen.generation, None);
        let second_export = pane.begin_operation(pane.screen.generation, None);
        pane.latest_export_operation = Some(second_export.id);
        pane.finish_export(
            first_export,
            Err(std::io::Error::other("stale export")),
            PathBuf::from("stale"),
            cx,
        );
        assert_eq!(pane.pending_recovery, Some(second_recovery));
        assert_eq!(
            pane.pane_state.failure().map(TerminalFailure::operation),
            Some("second-renderer-attempt")
        );
    });
}

#[gpui::test]
fn normal_exit_remains_distinct_from_stale_failures(cx: &mut TestAppContext) {
    let (pane, cx, _records) = connected_terminal_pane(cx);
    pane.update(cx, |pane, cx| {
        pane.handle_event(
            SessionEvent::Exited(crate::terminal::SessionExit::Success),
            cx,
        );
        pane.present_failure(
            TerminalFailure::resource("stale-resource-operation"),
            true,
            Some(RecoveryAction::RendererResources),
        );
        pane.handle_event(
            SessionEvent::Failed(SessionFailure::Runtime("stale fatal".to_owned())),
            cx,
        );
    });
    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.pane_state.clone(),
            pane.pending_recovery,
            pane.authoritative_status(),
        )),
        (
            PaneTerminalState::exited(crate::terminal::SessionExit::Success),
            None,
            Some("Shell exited successfully".to_owned()),
        )
    );
}

#[gpui::test]
fn renderer_resource_retry_retains_the_previous_gpu_cache_until_success(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let events = records
        .last_event_sender()
        .expect("the connected Pane should own a Terminal Session");
    events
        .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
        .unwrap();
    cx.run_until_parked();

    pane.update(cx, |pane, cx| {
        pane.graphics_cache
            .update(cx, |cache, _| cache.fail_next_sync());
    });
    events
        .try_send(SessionEvent::Screen(graphics_screen(2, 2)))
        .unwrap();
    cx.run_until_parked();

    let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
    assert_eq!(
        (
            pane.read_with(cx, |pane, _| pane.screen.generation),
            pane.read_with(cx, |pane, _| pane.pane_state.last_valid_frame()),
            pane.read_with(cx, |pane, _| {
                pane.pane_state
                    .failure()
                    .map(crate::terminal::TerminalFailure::class)
            }),
            cache.read_with(cx, |cache, _| cache.cached_image_keys()),
        ),
        (
            crate::terminal::PresentationGeneration::test(2),
            Some(crate::terminal::PresentationGeneration::test(1)),
            Some(crate::terminal::FailureClass::Resource),
            vec![crate::terminal::ImageKey {
                image_id: 1,
                generation: 1,
            }],
        )
    );

    let retry = cx
        .debug_bounds("retry-terminal-recovery")
        .expect("recoverable renderer failure should expose Retry");
    cx.simulate_click(retry.center(), Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        (
            pane.read_with(cx, |pane, _| pane.pane_state.clone()),
            pane.read_with(cx, |pane, _| pane.status.clone()),
            cache.read_with(cx, |cache, _| cache.cached_image_keys()),
        ),
        (
            PaneTerminalState::Running,
            None,
            vec![crate::terminal::ImageKey {
                image_id: 2,
                generation: 2,
            }],
        )
    );
}

#[gpui::test]
fn native_platform_retry_keeps_the_session_usable_and_clears_transient_failure(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: "recovered selection".to_owned(),
            html: None,
        },
    );
    pane.update(cx, |pane, _| {
        pane.selection_pasteboard.fail_next_write();
    });

    cx.simulate_keystrokes("cmd-c");
    cx.run_until_parked();

    assert_eq!(
        pane.read_with(cx, |pane, _| (
            pane.pane_state
                .failure()
                .map(crate::terminal::TerminalFailure::class),
            pane.terminal_session.session.is_some(),
        )),
        (Some(crate::terminal::FailureClass::Platform), true)
    );
    let retry = cx
        .debug_bounds("retry-terminal-recovery")
        .expect("recoverable native failure should expose Retry");
    cx.simulate_click(retry.center(), Modifiers::none());
    cx.run_until_parked();

    let copy_requests = records
        .commands()
        .iter()
        .filter(|call| matches!(call.command, RecordedSessionCommand::RequestSelectionCopy))
        .count();
    assert_eq!(
        (
            pane.read_with(cx, |pane, _| pane.pane_state.clone()),
            pane.read_with(cx, |pane, _| pane.status.clone()),
            pane.read_with(cx, |pane, _| pane.terminal_session.session.is_some()),
            cx.read_from_clipboard().and_then(|item| item.text()),
            copy_requests,
        ),
        (
            PaneTerminalState::Running,
            None,
            true,
            Some("recovered selection".to_owned()),
            2,
        )
    );
}

#[gpui::test]
fn native_platform_retry_requires_a_successful_selection_write_to_clear_failure(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = terminal_pane_with_selection_copy(
        cx,
        SelectionCopy {
            plain_text: "recovered selection".to_owned(),
            html: None,
        },
    );
    pane.update(cx, |pane, _| {
        pane.selection_pasteboard.fail_next_write();
    });
    cx.simulate_keystrokes("cmd-c");
    cx.run_until_parked();

    records.queue_selection_copy(None);
    let retry = cx
        .debug_bounds("retry-terminal-recovery")
        .expect("recoverable native failure should expose Retry");
    cx.simulate_click(retry.center(), Modifiers::none());
    cx.run_until_parked();

    assert!(pane.read_with(cx, |pane, _| {
        pane.pane_state
            .failure()
            .is_some_and(|failure| failure.class() == crate::terminal::FailureClass::Platform)
            && pane.pending_recovery.is_some()
    }));
    let retry = cx
        .debug_bounds("retry-terminal-recovery")
        .expect("a missing Selection should keep Retry available");
    cx.simulate_click(retry.center(), Modifiers::none());
    cx.run_until_parked();

    assert_eq!(
        (
            pane.read_with(cx, |pane, _| pane.pane_state.clone()),
            pane.read_with(cx, |pane, _| pane.pending_recovery),
            cx.read_from_clipboard().and_then(|item| item.text()),
        ),
        (
            PaneTerminalState::Running,
            None,
            Some("recovered selection".to_owned()),
        )
    );
}

#[gpui::test]
fn terminal_failure_should_keep_the_pane_visible_with_a_failure_status(cx: &mut TestAppContext) {
    cx.update(crate::ui::init)
        .expect("UI initialization should succeed");
    let records = TestTerminalSessionRecords::default();
    let session_factory: Rc<dyn TerminalSessionFactory> =
        Rc::new(TestTerminalSessionFactory::new(records.clone()));
    let session_factory = WorkspaceTerminalSessionFactory::new_local(
        session_factory,
        crate::terminal::testing::test_local_directory(PathBuf::from(
            "/tmp/spaceterm-terminal-pane-test",
        )),
    );
    let (pane, cx) =
        cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
    let exits = Rc::new(Cell::new(0));
    let exits_for_subscription = Rc::clone(&exits);
    pane.update(cx, |_, cx| {
        cx.subscribe(&pane, move |_, _, event: &TerminalPaneEvent, _| {
            if matches!(event, TerminalPaneEvent::Exited) {
                exits_for_subscription.update(|exits| exits + 1);
            }
        })
        .detach();
    });
    cx.run_until_parked();
    let sender = records
        .event_sender(1)
        .expect("rendering the Pane must start its Terminal Session");

    sender
        .try_send(SessionEvent::Failed(SessionFailure::PtyRead {
            read_error: "read unavailable".to_owned(),
            exit_status: "exit code 7".to_owned(),
        }))
        .unwrap();
    cx.run_until_parked();

    let state = pane.read_with(cx, |pane, _| {
        (
            pane.authoritative_status(),
            pane.terminal_session.session.is_some(),
            pane.pane_state
                .failure()
                .map(crate::terminal::TerminalFailure::class),
            pane.diagnostics.record_count(),
        )
    });
    assert_eq!(
        state,
        (
            Some(
                "PTY failed during read-shell-output. Close this Pane and restart the terminal command."
                    .to_owned()
            ),
            true,
            Some(crate::terminal::FailureClass::Pty),
            1,
        )
    );
    assert_eq!(
        (exits.get(), records.dropped_session_ids()),
        (0, Vec::new())
    );
    assert!(cx.debug_bounds("terminal-status").is_some());
    assert!(cx.debug_bounds("retry-terminal-recovery").is_none());
    let export = cx
        .debug_bounds("export-terminal-diagnostics")
        .expect("typed failure should expose explicit diagnostic export");
    cx.simulate_click(export.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.did_prompt_for_new_path());
}
#[cfg(all(test, target_os = "macos", feature = "macos-native-tests"))]
mod macos_adapter_tests {
    include!("../../platform/macos_adapter_tests/terminal_pane.rs");
}

#[gpui::test]
fn repeated_same_generation_delivery_preserves_snapshot_identity_and_does_not_submit(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let events = records.last_event_sender().unwrap();
    let original = text_screen(1, &["stable"]);
    events
        .try_send(SessionEvent::Screen(Arc::clone(&original)))
        .unwrap();
    cx.run_until_parked();
    let submissions = pane.read_with(cx, |pane, _| pane.scene_submission_attempts.len());
    for _ in 0..16 {
        let redundant = Arc::new((*original).clone());
        let released = Arc::downgrade(&redundant);
        events.try_send(SessionEvent::Screen(redundant)).unwrap();
        cx.run_until_parked();
        assert!(released.upgrade().is_none());
    }
    assert!(pane.read_with(cx, |pane, _| Arc::ptr_eq(&pane.screen, &original)));
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.scene_submission_attempts.len()),
        submissions
    );
}

#[gpui::test]
fn occlusion_evicts_image_resources_and_restore_reuploads_without_new_output(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let events = records.last_event_sender().unwrap();
    events
        .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
        .unwrap();
    cx.run_until_parked();
    let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
    let retained = cache.read_with(cx, |cache, _| cache.retained_bytes());
    assert!(retained > 0);
    pane.update(cx, |pane, cx| {
        pane.update_runtime_visibility(
            WindowVisibility {
                minimized: false,
                occluded: true,
                live_resize: false,
            },
            cx,
        )
    });
    assert_eq!(cache.read_with(cx, |cache, _| cache.retained_bytes()), 0);
    assert!(pane.read_with(cx, |pane, _| pane.grid_bounds.is_none()));
    pane.update(cx, |pane, cx| {
        pane.update_runtime_visibility(
            WindowVisibility {
                minimized: false,
                occluded: false,
                live_resize: false,
            },
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(
        cache.read_with(cx, |cache, _| cache.retained_bytes()),
        retained
    );
    assert!(pane.read_with(cx, |pane, _| {
        pane.render_lifecycle
            .is_presented(crate::terminal::PresentationGeneration::test(1))
    }));
}

#[gpui::test]
fn visible_focus_changes_preserve_graphics_resources_and_geometry(cx: &mut TestAppContext) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    records
        .last_event_sender()
        .unwrap()
        .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
        .unwrap();
    cx.run_until_parked();
    let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
    let retained = cache.read_with(cx, |cache, _| cache.retained_bytes());
    let keys = cache.read_with(cx, |cache, _| cache.cached_image_keys());
    let bounds = pane.read_with(cx, |pane, _| pane.grid_bounds);
    let focused = pane.read_with(cx, |pane, _| pane.product_focus);
    assert!(retained > 0 && bounds.is_some());

    for focus in [
        TerminalProductFocus {
            focused_pane: false,
            ..focused
        },
        focused,
        TerminalProductFocus {
            blocker: Some(TerminalFocusBlocker::Modal),
            ..focused
        },
        focused,
    ] {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(focus, cx);
            assert!(pane.render_lifecycle.can_present());
            assert_eq!(pane.grid_bounds, bounds);
            assert_eq!(pane.graphics_cache.read(cx).retained_bytes(), retained);
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            cache.read_with(cx, |cache, _| cache.retained_bytes()),
            retained
        );
        assert_eq!(
            cache.read_with(cx, |cache, _| cache.cached_image_keys()),
            keys
        );
    }
}

#[gpui::test]
fn product_hiding_releases_graphics_before_redraw_and_restores_without_output(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    records
        .last_event_sender()
        .unwrap()
        .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
        .unwrap();
    cx.run_until_parked();
    let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
    let retained = cache.read_with(cx, |cache, _| cache.retained_bytes());
    assert!(retained > 0);
    let visible = pane.read_with(cx, |pane, _| pane.product_focus);

    for hidden in [
        TerminalProductFocus {
            active_tab: false,
            ..visible
        },
        TerminalProductFocus {
            active_workspace: false,
            ..visible
        },
        TerminalProductFocus {
            pane_visible: false,
            ..visible
        },
    ] {
        pane.update(cx, |pane, cx| {
            pane.set_product_focus(hidden, cx);
        });
        assert_eq!(cache.read_with(cx, |cache, _| cache.retained_bytes()), 0);
        assert!(cache.read_with(cx, |cache, _| cache.cached_image_keys().is_empty()));
        assert!(cache.read_with(cx, |cache, _| cache.staged_image_keys().is_empty()));

        pane.update(cx, |pane, cx| {
            pane.set_product_focus(visible, cx);
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            cache.read_with(cx, |cache, _| cache.retained_bytes()),
            retained
        );
        assert!(pane.read_with(cx, |pane, _| {
            pane.render_lifecycle
                .is_presented(crate::terminal::PresentationGeneration::test(1))
        }));
    }
}

#[gpui::test]
fn graphics_without_presented_images_reserves_zero_bytes_and_releases_previous_images(
    cx: &mut TestAppContext,
) {
    let (pane, cx, records) = connected_terminal_pane(cx);
    let events = records.last_event_sender().unwrap();
    events
        .try_send(SessionEvent::Screen(graphics_screen(1, 1)))
        .unwrap();
    cx.run_until_parked();
    let cache = pane.read_with(cx, |pane, _| pane.graphics_cache.clone());
    assert!(cache.read_with(cx, |cache, _| cache.retained_bytes()) > 0);
    let mut no_placements = (*graphics_screen(2, 2)).clone();
    no_placements.graphics.placements = Arc::from([]);
    no_placements.graphics.placement_generation += 1;
    events
        .try_send(SessionEvent::Screen(Arc::new(no_placements)))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cache.read_with(cx, |cache, _| cache.retained_bytes()), 0);
    assert!(cache.read_with(cx, |cache, _| cache.cached_image_keys().is_empty()));
}

#[test]
fn local_origin_should_present_the_account_and_machine_without_its_mdns_suffix() {
    let context = crate::terminal::metadata::TerminalMetadataContext::local(
        crate::local_path::LocalPathSemantics::Posix,
        "/Users/tester",
        crate::terminal::metadata::LocalMachine::new(
            Some("tester"),
            Some("Testers-Mac.local"),
            Some("/Users/tester"),
        ),
    );

    let origin = PaneOrigin::from_context(&context);

    assert_eq!(origin.user.as_ref(), "tester");
    assert_eq!(origin.host.as_ref(), "Testers-Mac");
    assert!(!origin.remote);
}

#[test]
fn remote_origin_should_split_its_destination_and_stay_classified_remote() {
    for (destination, expected) in [
        ("user@remote.example", ("user", "remote.example")),
        ("build-box", ("", "build-box")),
        ("user@10.0.0.4", ("user", "10.0.0.4")),
    ] {
        let context = crate::terminal::metadata::TerminalMetadataContext::Remote(
            crate::terminal::metadata::RemoteTerminalMetadataContext::new(
                crate::domain::SshDestination::new(destination.to_owned()).unwrap(),
                crate::domain::RemoteDirectory::new("~/project".to_owned()).unwrap(),
            ),
        );

        let origin = PaneOrigin::from_context(&context);

        assert_eq!(
            (origin.user.as_ref(), origin.host.as_ref()),
            expected,
            "{destination}"
        );
        assert!(origin.remote, "{destination}");
    }
}

#[test]
fn displayed_directories_should_abbreviate_only_a_local_home_prefix() {
    for (directory, home, expected) in [
        ("/Users/tester", Some("/Users/tester"), "~"),
        ("/Users/tester/", Some("/Users/tester"), "~"),
        (
            "/Users/tester/Projects/app",
            Some("/Users/tester"),
            "~/Projects/app",
        ),
        (
            "/Users/tester-two/Projects",
            Some("/Users/tester"),
            "/Users/tester-two/Projects",
        ),
        ("/srv/app", Some("/Users/tester"), "/srv/app"),
        ("/Users/tester/app", None, "/Users/tester/app"),
        ("~/project", None, "~/project"),
    ] {
        assert_eq!(
            compact_home_directory(directory, home),
            expected,
            "{directory}"
        );
    }
}
