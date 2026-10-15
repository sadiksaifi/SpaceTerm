//! One explicitly launched Development Pane. Organization and client-native effects are absent.
use super::*;
use spaceterm_server::{
    ServerError,
    pane::{self, *},
};
use std::{sync::Arc, time::Duration};

pub struct DevelopmentPaneHost {
    boot: [u8; 32],
    pane: Option<OwnedPane>,
    factory: NativeTerminalSessionFactory,
    directory: ValidatedLocalDirectory,
}
struct OwnedPane {
    device: [u8; 32],
    lease: Lease,
    connected: bool,
    runtime: TerminalRuntime,
    subscription: TerminalSubscription,
    screen: Option<Arc<ScreenSnapshot>>,
    sent: Option<PresentationGeneration>,
    ended: bool,
    failed: bool,
    appearance_generation: u64,
}
fn nonce() -> Result<[u8; 32], ServerError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| ServerError::StartupFailed)?;
    Ok(bytes)
}
impl DevelopmentPaneHost {
    pub fn new(
        factory: NativeTerminalSessionFactory,
        directory: ValidatedLocalDirectory,
    ) -> Result<Self, ServerError> {
        Ok(Self {
            boot: nonce()?,
            pane: None,
            factory,
            directory,
        })
    }
    /// Native composition for the explicitly launched local Development host.
    pub fn local() -> Result<Self, ServerError> {
        let filesystem = local_filesystem();
        let directory = filesystem
            .validate_directory(
                &crate::platform::app_directories::AppDirectoryEnvironment::capture()
                    .home
                    .map(std::path::PathBuf::from)
                    .ok_or(ServerError::StartupFailed)?,
            )
            .map_err(|_| ServerError::StartupFailed)?;
        let shell = crate::platform::launch_host::user_shell();
        let factory = NativeTerminalSessionFactory::new(
            native_pty_factory(),
            ShellLaunchPlanner::new(
                shell,
                std::path::PathBuf::new(),
                ShellIntegrationMode::Disabled,
                ShellEnvironment::default(),
                ShellIntegrationPolicy {
                    supported: true,
                    path_list_separator: ':',
                    fallback_xdg_data_dirs: "/usr/local/share:/usr/share".into(),
                },
                &[],
            ),
            filesystem,
            LocalMachine::default(),
        );
        Self::new(factory, directory)
    }
    fn start(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        size: PaneSize,
        style: Option<PresentationStyle>,
    ) -> Result<Lease, ServerError> {
        let lease = Lease {
            server: self.boot,
            session: nonce()?,
            control: nonce()?,
            connection,
        };
        let runtime = self
            .factory
            .start_runtime(
                geometry(size),
                TerminalLaunchPlan::Local(
                    LocalTerminalLaunchPlan::new(self.directory.clone()).without_client_effects(),
                ),
                appearance(style, 1).map_err(|_| ServerError::StartupFailed)?,
            )
            .map_err(|_| ServerError::StartupFailed)?;
        let subscription = runtime.subscribe();
        // No focus grant: OSC 52 and native requests have no eligible client in this slice.
        self.pane = Some(OwnedPane {
            device,
            lease,
            connected: true,
            runtime,
            subscription,
            screen: None,
            sent: None,
            ended: false,
            failed: false,
            appearance_generation: 1,
        });
        Ok(lease)
    }
    fn handle(
        &mut self,
        device: [u8; 32],
        connection: [u8; 32],
        request: Request,
    ) -> Result<Reply, Refusal> {
        if matches!(request, Request::Capabilities) {
            return Ok(Reply::Capabilities {
                development_pane: true,
            });
        }
        if let Request::Attach {
            create,
            size,
            resume,
            style,
        } = request
        {
            size.validate().map_err(|_| Refusal::Unsupported)?;
            if let Some(pane) = &mut self.pane {
                if pane.device != device {
                    return Err(Refusal::Controlled);
                }
                if resume.is_some_and(|old| {
                    (old.server, old.session, old.control)
                        != (pane.lease.server, pane.lease.session, pane.lease.control)
                }) {
                    return Err(Refusal::Stale);
                }
                // Reconnection never starts a replacement shell, even after ordinary exit.
                pane.lease.connection = connection;
                pane.connected = true;
                pane.sent = None;
                pane.runtime.resize(geometry(size));
                if style.is_some() {
                    pane.appearance_generation += 1;
                    pane.runtime.update_appearance(
                        appearance(style, pane.appearance_generation)
                            .map_err(|_| Refusal::Failed)?,
                    );
                }
                return Ok(Reply::Attached { lease: pane.lease });
            }
            if !create {
                return Err(Refusal::Missing);
            }
            return self
                .start(device, connection, size, style)
                .map(|lease| Reply::Attached { lease })
                .map_err(|_| Refusal::Failed);
        }
        if matches!(request, Request::Shutdown) {
            self.shutdown().map_err(|_| Refusal::CleanupUnresolved)?;
            return Ok(Reply::Closed);
        }
        let pane = self.pane.as_mut().ok_or(Refusal::Missing)?;
        let lease = match &request {
            Request::Poll { lease }
            | Request::Key { lease, .. }
            | Request::Resize { lease, .. }
            | Request::Close { lease }
            | Request::Style { lease, .. } => lease,
            _ => return Err(Refusal::Unsupported),
        };
        if device != pane.device
            || connection != pane.lease.connection
            || *lease != pane.lease
            || !pane.connected
        {
            return Err(Refusal::Stale);
        }
        pane.drain();
        match request {
            Request::Poll { .. } => {
                if let Some(screen) = &pane.screen
                    && pane.sent != Some(screen.generation)
                {
                    pane.sent = Some(screen.generation);
                    return Ok(Reply::Screen(Box::new(encode_screen(screen))));
                }
                if pane.failed {
                    return Err(Refusal::Failed);
                }
                Ok(if pane.ended {
                    Reply::Ended
                } else {
                    Reply::Unchanged
                })
            }
            Request::Key { key, .. } => {
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                pane.runtime
                    .key(decode_key(key).ok_or(Refusal::Unsupported)?);
                Ok(Reply::Unchanged)
            }
            Request::Resize { size, .. } => {
                size.validate().map_err(|_| Refusal::Unsupported)?;
                if pane.ended || pane.failed {
                    return Err(Refusal::Missing);
                }
                pane.runtime.resize(geometry(size));
                Ok(Reply::Unchanged)
            }
            Request::Style { style, .. } => {
                pane.appearance_generation += 1;
                pane.runtime.update_appearance(
                    appearance(Some(style), pane.appearance_generation)
                        .map_err(|_| Refusal::Failed)?,
                );
                Ok(Reply::Unchanged)
            }
            Request::Close { .. } => {
                pane.runtime
                    .shutdown_and_wait(Duration::from_secs(5))
                    .map_err(|_| Refusal::CleanupUnresolved)?;
                self.pane = None;
                Ok(Reply::Closed)
            }
            _ => Err(Refusal::Unsupported),
        }
    }
}
impl PaneService for DevelopmentPaneHost {
    fn request(&mut self, device: [u8; 32], connection: [u8; 32], request: Request) -> Reply {
        self.handle(device, connection, request)
            .unwrap_or_else(Reply::Refused)
    }
    fn disconnected(&mut self, connection: [u8; 32]) {
        if let Some(pane) = &mut self.pane
            && pane.lease.connection == connection
        {
            pane.connected = false;
        }
    }
    fn shutdown(&mut self) -> Result<(), ServerError> {
        if let Some(pane) = &mut self.pane {
            pane.connected = false;
            pane.runtime
                .shutdown_and_wait(Duration::from_secs(5))
                .map_err(|_| ServerError::CleanupUnresolved)?;
        }
        self.pane = None;
        Ok(())
    }
}
impl OwnedPane {
    fn drain(&mut self) {
        while let Ok(event) = self.subscription.events.try_recv() {
            match event {
                TerminalSessionEvent::Screen(screen) => self.screen = Some(screen),
                TerminalSessionEvent::Exited(_) => self.ended = true,
                TerminalSessionEvent::Failed(_) => self.failed = true,
                _ => {}
            }
        }
    }
}
pub(crate) fn geometry(size: PaneSize) -> TerminalGeometry {
    TerminalGeometry::from_viewport(
        LogicalSize::new(
            f32::from(size.columns) * f32::from(size.cell_width),
            f32::from(size.rows) * f32::from(size.cell_height),
        ),
        LogicalCellSize::new(f32::from(size.cell_width), f32::from(size.cell_height)),
        BackingScale::ONE,
        CellGridSize::new(2, 2),
    )
}
fn appearance(
    style: Option<PresentationStyle>,
    generation: u64,
) -> Result<TerminalAppearanceUpdate, TerminalFailure> {
    let mut update = TerminalAppearanceUpdate::builtin()?;
    update.generation = AppearanceGeneration::new(generation);
    if let Some(style) = style {
        let resolved = Arc::make_mut(&mut update.appearance);
        resolved.appearance = if style.light {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        resolved.colors.foreground = from_rgb(style.foreground);
        resolved.colors.background = from_rgb(style.background);
        resolved.colors.cursor = from_rgb(style.cursor);
        resolved.colors.cursor_text = style.cursor_text.map(from_rgb);
        resolved.colors.normal = std::array::from_fn(|i| from_rgb(style.palette[i]));
        resolved.colors.bright = std::array::from_fn(|i| from_rgb(style.palette[i + 8]));
        resolved.bold_as_bright = style.bold_as_bright;
    }
    Ok(update)
}
fn from_rgb(c: [u8; 3]) -> super::Color {
    super::Color::from_rgb_components(c[0], c[1], c[2])
}
fn rgb(color: super::Color) -> [u8; 3] {
    [color.r, color.g, color.b]
}
fn encode_color(color: TerminalColor) -> pane::Color {
    match color {
        TerminalColor::Default => pane::Color::Default,
        TerminalColor::Palette(i) => pane::Color::Palette(i),
        TerminalColor::Rgb(c) => pane::Color::Rgb(rgb(c)),
    }
}
pub(crate) fn encode_screen(screen: &ScreenSnapshot) -> Screen {
    Screen {
        columns: screen.size.cols,
        rows: screen
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| Cell {
                        text: c.text.chars().take(64).collect(),
                        foreground: encode_color(c.foreground_source),
                        background: encode_color(c.background_source),
                        underline_color: encode_color(c.underline_source),
                        flags: u16::from(c.inverse)
                            | (u16::from(c.bold) << 1)
                            | (u16::from(c.faint) << 2)
                            | (u16::from(c.italic) << 3)
                            | (u16::from(c.blinking) << 4)
                            | (u16::from(c.invisible) << 5)
                            | (u16::from(c.strikethrough) << 6)
                            | (u16::from(c.overline) << 7)
                            | (u16::from(c.spacer_tail) << 8),
                        underline: match c.underline {
                            TerminalUnderlineSnapshot::None => 0,
                            TerminalUnderlineSnapshot::Single => 1,
                            TerminalUnderlineSnapshot::Double => 2,
                            TerminalUnderlineSnapshot::Curly => 3,
                            TerminalUnderlineSnapshot::Dotted => 4,
                            TerminalUnderlineSnapshot::Dashed => 5,
                        },
                    })
                    .collect()
            })
            .collect(),
        wrapped: screen.row_soft_wrapped.to_vec(),
        foreground: rgb(screen.colors.foreground),
        background: rgb(screen.colors.background),
        palette: screen.colors.palette.iter().copied().map(rgb).collect(),
        reversed: screen.colors.reversed,
        foreground_overridden: screen.colors.foreground_source
            == TerminalDefaultColorSource::ProgramOverride,
        background_overridden: screen.colors.background_source
            == TerminalDefaultColorSource::ProgramOverride,
        cursor_overridden: screen.colors.cursor_source
            == TerminalDefaultColorSource::ProgramOverride,
        palette_overrides: screen.colors.palette_overrides.to_vec(),
        cursor: screen
            .cursor
            .position
            .map(|p| (p.column, p.row, p.width_cells)),
        cursor_visible: screen.cursor.visible,
        cursor_blinking: screen.cursor.blinking,
        cursor_shape: match screen.cursor.shape {
            CursorShapeSnapshot::Block => 0,
            CursorShapeSnapshot::Bar => 1,
            CursorShapeSnapshot::Underline => 2,
            CursorShapeSnapshot::BlockHollow => 3,
        },
        cursor_color: rgb(screen.cursor.color),
        cursor_text: rgb(screen.cursor.text_color),
        alternate: screen.active_screen == ActiveScreenSnapshot::Alternate,
    }
}
fn decode_modifiers(bits: u16) -> InputModifiers {
    InputModifiers {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        control: bits & 4 != 0,
        platform: bits & 8 != 0,
        caps_lock: bits & 16 != 0,
        num_lock: bits & 32 != 0,
        shift_right: bits & 64 != 0,
        alt_right: bits & 128 != 0,
        control_right: bits & 256 != 0,
        platform_right: bits & 512 != 0,
    }
}
pub(crate) fn decode_key(key: Key) -> Option<KeyInput> {
    if key.logical.len() > 128
        || key.text.as_ref().is_some_and(|t| t.len() > 4096)
        || key.modifiers > 1023
        || key.consumed > 1023
    {
        return None;
    }
    let input = KeyInput {
        action: match key.action {
            0 => KeyAction::Press,
            1 => KeyAction::Repeat,
            2 => KeyAction::Release,
            _ => return None,
        },
        physical_key: PhysicalKey::try_from(i32::try_from(key.physical).ok()?).ok()?,
        native_key_code: None,
        logical_key: key.logical,
        text: key.text,
        unshifted_codepoint: key.unshifted,
        modifiers: decode_modifiers(key.modifiers),
        consumed_modifiers: decode_modifiers(key.consumed),
        option_as_alt: if key.option_as_alt {
            OptionAsAltPolicy::Both
        } else {
            OptionAsAltPolicy::None
        },
    };
    input.validate().ok()?;
    Some(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn host() -> DevelopmentPaneHost {
        let filesystem = local_filesystem();
        let directory = filesystem
            .validate_directory(&std::env::temp_dir())
            .unwrap();
        let factory = NativeTerminalSessionFactory::new(
            native_pty_factory(),
            ShellLaunchPlanner::new(
                "/bin/sh".into(),
                std::path::PathBuf::new(),
                ShellIntegrationMode::Disabled,
                ShellEnvironment::default(),
                ShellIntegrationPolicy {
                    supported: true,
                    path_list_separator: ':',
                    fallback_xdg_data_dirs: "/usr/local/share:/usr/share".into(),
                },
                &[],
            ),
            filesystem,
            LocalMachine::default(),
        );
        DevelopmentPaneHost::new(factory, directory).unwrap()
    }
    fn size() -> PaneSize {
        PaneSize {
            columns: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 18,
        }
    }
    fn attach(host: &mut DevelopmentPaneHost, connection: u8, create: bool) -> Lease {
        let Reply::Attached { lease } = host.request(
            [1; 32],
            [connection; 32],
            Request::Attach {
                create,
                size: size(),
                resume: None,
                style: None,
            },
        ) else {
            panic!("expected attachment")
        };
        lease
    }
    fn command(host: &mut DevelopmentPaneHost, connection: u8, lease: Lease, text: &str) -> Reply {
        let result = host.request(
            [1; 32],
            [connection; 32],
            Request::Key {
                lease,
                key: Key {
                    physical: PhysicalKey::Unidentified as u32,
                    action: 0,
                    logical: "text-input".into(),
                    text: Some(text.trim_end_matches('\n').into()),
                    unshifted: None,
                    modifiers: 0,
                    consumed: 0,
                    option_as_alt: false,
                },
            },
        );
        if !matches!(result, Reply::Unchanged) {
            return result;
        }
        host.request(
            [1; 32],
            [connection; 32],
            Request::Key {
                lease,
                key: Key {
                    physical: PhysicalKey::Enter as u32,
                    action: 0,
                    logical: "enter".into(),
                    text: None,
                    unshifted: None,
                    modifiers: 0,
                    consumed: 0,
                    option_as_alt: false,
                },
            },
        )
    }
    fn await_text(host: &mut DevelopmentPaneHost, connection: u8, lease: Lease, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Reply::Screen(screen) =
                host.request([1; 32], [connection; 32], Request::Poll { lease })
                && screen.rows.iter().any(|row| {
                    row.iter()
                        .map(|c| c.text.as_str())
                        .collect::<String>()
                        .contains(text)
                })
            {
                return;
            }
            assert!(Instant::now() < deadline, "expected terminal output {text}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn reconnect_keeps_running_work_and_fences_the_old_connection() {
        let mut host = host();
        let first = attach(&mut host, 2, true);
        command(
            &mut host,
            2,
            first,
            "stty -echo; printf '\\033[2J\\033[HSTARTED\\n'; sleep 1; printf 'CONTINUED\\n'\n",
        );
        await_text(&mut host, 2, first, "STARTED");
        host.disconnected([2; 32]);
        std::thread::sleep(Duration::from_millis(1100));
        let second = attach(&mut host, 3, false);
        assert_eq!(first.session, second.session);
        assert!(matches!(
            command(&mut host, 2, first, "printf 'STALE'\n"),
            Reply::Refused(Refusal::Stale)
        ));
        assert!(matches!(
            host.request(
                [9; 32],
                [9; 32],
                Request::Attach {
                    create: false,
                    size: size(),
                    resume: None,
                    style: None
                }
            ),
            Reply::Refused(Refusal::Controlled)
        ));
        await_text(&mut host, 3, second, "CONTINUED");
        host.shutdown().unwrap();
    }

    #[test]
    fn reconnect_and_stale_generations_cannot_start_or_mutate_a_replacement() {
        let mut host = host();
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::Attach {
                    create: false,
                    size: size(),
                    resume: None,
                    style: None
                }
            ),
            Reply::Refused(Refusal::Missing)
        ));
        let old = attach(&mut host, 2, true);
        assert!(matches!(
            host.request([1; 32], [2; 32], Request::Close { lease: old }),
            Reply::Closed
        ));
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::Attach {
                    create: false,
                    size: size(),
                    resume: None,
                    style: None
                }
            ),
            Reply::Refused(Refusal::Missing)
        ));
        let live = attach(&mut host, 2, true);
        assert!(matches!(
            host.request(
                [1; 32],
                [2; 32],
                Request::Attach {
                    create: false,
                    size: PaneSize {
                        columns: 90,
                        ..size()
                    },
                    style: None,
                    resume: Some(old)
                }
            ),
            Reply::Refused(Refusal::Stale)
        ));
        for stale in [
            Lease {
                server: old.server.map(|b| b ^ 1),
                ..live
            },
            Lease {
                session: old.session,
                ..live
            },
            Lease {
                control: old.control,
                ..live
            },
        ] {
            assert!(matches!(
                host.request([1; 32], [2; 32], Request::Close { lease: stale }),
                Reply::Refused(Refusal::Stale)
            ));
            assert!(matches!(
                host.request(
                    [1; 32],
                    [2; 32],
                    Request::Resize {
                        lease: stale,
                        size: PaneSize {
                            columns: 90,
                            ..size()
                        }
                    }
                ),
                Reply::Refused(Refusal::Stale)
            ));
        }
        command(
            &mut host,
            2,
            live,
            "stty -echo; printf '\\033[2J\\033[HLIVE\\n'; stty size\n",
        );
        await_text(&mut host, 2, live, "24 80");
        host.shutdown().unwrap();
    }
}
