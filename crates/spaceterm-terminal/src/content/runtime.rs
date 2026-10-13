//! Worker-local VT composition. Native launch and filesystem authority remain
//! injected by the owning server; no content value supplies either capability.
use super::{
    copy::{CopyJob, CopyStep},
    find::{FindJob, FindStep},
    graphics::{GraphicsJob, GraphicsStep},
    pages::ContentScanBudget,
};
use crate::runtime::{budget::BudgetLease, ports::*};
use crate::runtime_contracts::*;
use crate::{
    TerminalEmulator,
    geometry::*,
    key::{InputModifiers, KeyAction, KeyInput, PhysicalKey},
    keyboard_protocol::KeyboardProtocolEncoder,
    local_filesystem::LocalFilesystemAuthority,
    metadata::{CommandState, MetadataFreshness, PromptZone, TerminalMetadataContext},
    pointer_input::{PointerButton, PointerPhase},
};
use libghostty_vt::{
    key::Mods,
    mouse,
    style::{Palette, RgbColor},
    terminal::Mode,
};
use spaceterm_core::{
    authority::CloseWorkGeneration,
    content::*,
    content_queries::*,
    geometry::MeasuredGrid,
    observations::*,
    organization::{FailureClass, ServerDirectory},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// Process-wide checked allocation avoids collisions when one authenticated
// connection requests Copy from several Session owners. Epoch changes are
// separately fenced; a failed allocation never wraps back to a live identity.
static NEXT_JOB: AtomicU64 = AtomicU64::new(0);
const JOB_SCRATCH_BYTES: usize = 1024 * 1024;
const MAX_JOBS: usize = 8;
const MAX_HELD_KEYS: usize = 256;
enum Job {
    Copy(Box<CopyJob>),
    Find(Box<FindJob>),
    Graphics(ResourceTransferId, Box<GraphicsJob>),
}
/// Production VT adapter. The emulator and every scan remain on its worker.
pub struct SemanticRuntimeEngine {
    emulator: TerminalEmulator,
    clock: Arc<dyn RuntimeClock>,
    base: ContentBase,
    clock_origin: MonotonicTick,
    instant_origin: Instant,
    jobs: Vec<(EngineJobId, Job)>,
    keyboard: KeyboardProtocolEncoder,
    mouse: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    held_keys: Vec<KeyInput>,
    held_button: Option<mouse::Button>,
    last_mouse_position: mouse::Position,
    metadata_revision: Option<u64>,
    directory_observation_sequence: Option<u64>,
    work_observation_sequence: Option<u64>,
    hidden_input: bool,
    observation_sequence: u64,
    work_generation: CloseWorkGeneration,
    work_state: CloseWorkState,
}
impl SemanticRuntimeEngine {
    /// Capture explicit owning-machine metadata and native authority. The VT is
    /// constructed by the worker only after measured runtime admission.
    pub fn launch(
        metadata: TerminalMetadataContext,
        local_filesystem: LocalFilesystemAuthority,
    ) -> EngineLaunchContext {
        EngineLaunchContext::new(move |context| {
            if metadata.initial_directory().as_bytes() != context.directory.bytes() {
                return Err(RuntimeFailure::new(
                    FailureClass::InvalidRequest,
                    RuntimeStage::Launch,
                ));
            }
            let instant_origin = Instant::now();
            let clock_origin = context.clock.now().map_err(|_| counter_failure())?;
            let mut emulator = TerminalEmulator::new_with_local_filesystem(
                geometry(context.grid),
                metadata,
                "",
                crate::identity::TERM_FALLBACK,
                instant_origin,
                local_filesystem,
                appearance(&context.appearance),
            )
            .map_err(|_| engine_failure())?;
            set_palette(&mut emulator, &context.appearance)?;
            emulator
                .set_content_session(context.session)
                .map_err(content_failure)?;
            let base = emulator.content_base().map_err(content_failure)?;
            Ok(Box::new(Self {
                emulator,
                clock: context.clock,
                base,
                clock_origin,
                instant_origin,
                jobs: Vec::new(),
                keyboard: KeyboardProtocolEncoder::new().map_err(|_| engine_failure())?,
                mouse: mouse::Encoder::new().map_err(|_| engine_failure())?,
                mouse_event: mouse::Event::new().map_err(|_| engine_failure())?,
                held_keys: Vec::new(),
                held_button: None,
                last_mouse_position: mouse::Position { x: 0.0, y: 0.0 },
                metadata_revision: None,
                directory_observation_sequence: None,
                work_observation_sequence: None,
                hidden_input: false,
                // The owner reserves sequence 1 for its initial Unknown fact.
                observation_sequence: 1,
                work_generation: CloseWorkGeneration::new(0),
                work_state: CloseWorkState::Unknown,
            }))
        })
    }
    fn now(&self) -> Result<(MonotonicTick, Instant), RuntimeFailure> {
        let tick = self.clock.now().map_err(|_| counter_failure())?;
        let elapsed = tick
            .checked_elapsed_since(self.clock_origin)
            .ok_or_else(counter_failure)?;
        let instant = self
            .instant_origin
            .checked_add(Duration::from_millis(elapsed))
            .ok_or_else(counter_failure)?;
        Ok((tick, instant))
    }
    fn update_base(&mut self) -> Result<(), RuntimeFailure> {
        self.base = self.emulator.content_base().map_err(content_failure)?;
        Ok(())
    }
    fn observations(
        &mut self,
        tick: MonotonicTick,
        hidden_changed: bool,
    ) -> Result<EngineObservations, RuntimeFailure> {
        let metadata = self.emulator.metadata();
        let mut result = EngineObservations {
            confirmed_directory: self.emulator.take_confirmed_directory(),
            ..Default::default()
        };
        let (directory_source, work_source) = self
            .emulator
            .metadata_observation_sequences()
            .ok_or_else(counter_failure)?;
        let metadata_changed = self.metadata_revision != Some(metadata.revision) || hidden_changed;
        let directory_changed = self.directory_observation_sequence != Some(directory_source);
        let work_changed = self.work_observation_sequence != Some(work_source) || hidden_changed;
        // Presentation timers and unrelated reports never refresh another lane.
        if !metadata_changed && !directory_changed && !work_changed {
            return Ok(result);
        }
        self.metadata_revision = Some(metadata.revision);
        self.directory_observation_sequence = Some(directory_source);
        self.work_observation_sequence = Some(work_source);
        self.observation_sequence = self
            .observation_sequence
            .checked_add(1)
            .ok_or_else(counter_failure)?;
        let sequence = RuntimeObservationSequence::new(self.observation_sequence)
            .map_err(|_| counter_failure())?;
        if metadata_changed {
            let identity = |value: &str| {
                RuntimeMetadataIdentity::new(value.to_owned()).map_err(|_| engine_failure())
            };
            let origin = match metadata.context.origin() {
                crate::metadata::TerminalOrigin::Local { user, host } => {
                    RuntimeMetadataOrigin::Local {
                        user: user.map(identity).transpose()?,
                        host: host.map(identity).transpose()?,
                    }
                }
                crate::metadata::TerminalOrigin::Remote { user, host } => {
                    RuntimeMetadataOrigin::Remote {
                        user: user.map(identity).transpose()?,
                        host: identity(host)?,
                    }
                }
            };
            result.metadata = Some(
                RuntimeMetadataSnapshot::new(RuntimeMetadataParts {
                    session: self.base.session,
                    sequence,
                    observed_at: tick,
                    // Directory basenames retain their full spelling in the source.
                    // Bound presentation independently; it cannot retire the PTY or
                    // change the directory's restoration authority.
                    title: crate::metadata::TitleMetadata {
                        value: Arc::from(crate::metadata::sanitize_title(&metadata.title.value)),
                        provenance: metadata.title.provenance,
                    },
                    origin,
                    progress: metadata.progress,
                    freshness: metadata.freshness,
                    prompt_zone: metadata.prompt_zone,
                    hidden_input: self.hidden_input,
                })
                .map_err(|_| engine_failure())?,
            );
        }
        if directory_changed
            && let Ok(directory) = ServerDirectory::new(
                self.base.session.pane.server(),
                metadata.directory.path.as_bytes().to_vec(),
            )
        {
            result.display_directory = Some(
                DisplayDirectoryObservation::new(
                    self.base.session,
                    sequence,
                    if directory_source == 0 {
                        self.clock_origin
                    } else {
                        tick
                    },
                    directory,
                    if metadata.context.is_local() {
                        DirectoryPresentation::OwningServer
                    } else {
                        DirectoryPresentation::ReportedRemote
                    },
                )
                .map_err(|_| counter_failure())?,
            );
        }
        if work_changed {
            let state = if metadata.freshness == MetadataFreshness::Stale {
                CloseWorkState::Unknown
            } else if self.hidden_input
                || metadata
                    .command
                    .as_ref()
                    .is_some_and(|command| command.state == CommandState::Running)
            {
                CloseWorkState::RunningWork
            } else if matches!(
                metadata.prompt_zone,
                PromptZone::Prompt | PromptZone::CommandInput
            ) {
                CloseWorkState::Idle
            } else {
                CloseWorkState::Unknown
            };
            if state != self.work_state {
                self.work_generation = self
                    .work_generation
                    .checked_successor()
                    .ok_or_else(counter_failure)?;
                self.work_state = state;
            }
            result.close_work = Some(
                CloseWorkObservation::new(
                    self.base.session,
                    sequence,
                    self.work_generation,
                    if work_source == 0 && !hidden_changed {
                        self.clock_origin
                    } else {
                        tick
                    },
                    state,
                )
                .map_err(|_| counter_failure())?,
            );
        }
        Ok(result)
    }
    fn key(&mut self, input: &KeyInput, bytes: &mut Vec<u8>) -> Result<(), RuntimeFailure> {
        let position = self
            .held_keys
            .iter()
            .position(|key| key.physical_key == input.physical_key);
        if input.physical_key != PhysicalKey::Unidentified
            && input.action != KeyAction::Release
            && position.is_none()
        {
            if self.held_keys.len() >= MAX_HELD_KEYS {
                return Err(RuntimeFailure::capacity(RuntimeStage::Input));
            }
            self.held_keys
                .try_reserve(1)
                .map_err(|_| RuntimeFailure::capacity(RuntimeStage::Input))?;
        }
        self.keyboard
            .encode(&self.emulator.terminal, input, bytes)
            .map_err(|_| RuntimeFailure::new(FailureClass::InvalidRequest, RuntimeStage::Input))?;
        if input.action == KeyAction::Release {
            if let Some(position) = position {
                self.held_keys.remove(position);
            }
        } else if input.physical_key != PhysicalKey::Unidentified && position.is_none() {
            // Release state contains no retained terminal/IME text or native key code.
            let mut release = KeyInput::text_input("");
            release.physical_key = input.physical_key;
            release.logical_key.clear();
            release.text = None;
            release.unshifted_codepoint = input.unshifted_codepoint;
            release.action = KeyAction::Release;
            self.held_keys.push(release);
        }
        Ok(())
    }
    fn position(
        &self,
        stamp: QueryStamp,
        cell: CellRef,
    ) -> Result<mouse::Position, RuntimeFailure> {
        self.emulator
            .semantic_content
            .check(&self.emulator.terminal, stamp)
            .map_err(content_failure)?;
        let row = self
            .emulator
            .semantic_content
            .resolve_cell(&self.emulator.terminal, cell)
            .map_err(content_failure)?;
        let snapshot = self
            .emulator
            .semantic_content
            .reader
            .capture(&self.emulator.terminal)
            .map_err(|_| engine_failure())?;
        let y = row
            .document_row
            .checked_sub(snapshot.active_start)
            .filter(|y| *y < u32::from(self.emulator.geometry().grid().rows))
            .ok_or_else(|| content_failure(ContentReadFailure::InvalidRange))?;
        let size = self.emulator.geometry().backing_cell_size();
        Ok(mouse::Position {
            x: (f32::from(cell.column) + 0.5) * size.width as f32,
            y: (y as f32 + 0.5) * size.height as f32,
        })
    }
    fn mouse(
        &mut self,
        action: mouse::Action,
        button: Option<mouse::Button>,
        position: mouse::Position,
        modifiers: InputModifiers,
        bytes: &mut Vec<u8>,
    ) -> Result<(), RuntimeFailure> {
        let geometry = self.emulator.geometry();
        let cell = geometry.backing_cell_size();
        let grid = geometry.grid();
        self.mouse
            .set_options_from_terminal(&self.emulator.terminal)
            .set_size(mouse::EncoderSize {
                screen_width: u32::from(grid.cols) * cell.width,
                screen_height: u32::from(grid.rows) * cell.height,
                cell_width: cell.width,
                cell_height: cell.height,
                padding_top: 0,
                padding_bottom: 0,
                padding_left: 0,
                padding_right: 0,
            })
            .set_any_button_pressed(self.held_button.is_some());
        let mut mods = Mods::empty();
        mods.set(Mods::SHIFT, modifiers.shift);
        mods.set(Mods::ALT, modifiers.alt);
        mods.set(Mods::CTRL, modifiers.control);
        mods.set(Mods::SUPER, modifiers.platform);
        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(mods)
            .set_position(position);
        self.mouse
            .encode_to_vec(&self.mouse_event, bytes)
            .map_err(|_| engine_failure())?;
        self.last_mouse_position = position;
        Ok(())
    }
}
fn geometry(grid: MeasuredGrid) -> TerminalGeometry {
    TerminalGeometry::from_viewport(
        LogicalSize::new(
            f32::from(grid.columns()) * f32::from(grid.cell_pixels().width()),
            f32::from(grid.rows()) * f32::from(grid.cell_pixels().height()),
        ),
        LogicalCellSize::new(
            f32::from(grid.cell_pixels().width()),
            f32::from(grid.cell_pixels().height()),
        ),
        BackingScale::ONE,
        CellGridSize::new(grid.columns(), grid.rows()),
    )
}
fn appearance(value: &RuntimeAppearance) -> crate::TerminalAppearanceUpdate {
    use crate::appearance::{Color, ResolvedTerminalAppearance, TerminalColors};
    let color = |value: Rgb| Color::from_rgb_components(value.r, value.g, value.b);
    let fg = color(value.foreground);
    let bg = color(value.background);
    let colors = TerminalColors {
        foreground: fg,
        background: bg,
        normal: std::array::from_fn(|i| color(value.palette[i])),
        bright: std::array::from_fn(|i| color(value.palette[i + 8])),
        dim: std::array::from_fn(|i| color(value.palette[i])),
        bright_foreground: fg,
        dim_foreground: fg,
        cursor: fg,
        cursor_text: None,
        selection_background: bg,
        selection_foreground: None,
        find_match_background: bg,
        find_match_foreground: None,
        find_active_match_background: bg,
        find_active_match_foreground: None,
        hyperlink: fg,
        visual_bell: fg,
    };
    crate::TerminalAppearanceUpdate::new(
        value.generation,
        Arc::new(ResolvedTerminalAppearance {
            appearance: value.appearance,
            colors,
            bold_as_bright: value.bold_as_bright,
        }),
    )
}
fn set_palette(
    emulator: &mut TerminalEmulator,
    value: &RuntimeAppearance,
) -> Result<(), RuntimeFailure> {
    emulator
        .terminal
        .set_default_color_palette(Some(Palette(value.palette.map(|c| RgbColor {
            r: c.r,
            g: c.g,
            b: c.b,
        }))))
        .map_err(|_| engine_failure())?;
    Ok(())
}
fn engine_failure() -> RuntimeFailure {
    RuntimeFailure::internal(RuntimeStage::Engine)
}
fn counter_failure() -> RuntimeFailure {
    RuntimeFailure::internal(RuntimeStage::Counter)
}
fn content_failure(reason: ContentReadFailure) -> RuntimeFailure {
    let class = match reason {
        ContentReadFailure::Capacity => FailureClass::ResourceLimit,
        ContentReadFailure::StaleAuthority | ContentReadFailure::Cancelled => {
            FailureClass::StaleAuthority
        }
        ContentReadFailure::MissingResource | ContentReadFailure::Retired => {
            FailureClass::RuntimeUnavailable
        }
        ContentReadFailure::InvalidRange => FailureClass::InvalidRequest,
        ContentReadFailure::Exhausted => FailureClass::Internal,
    };
    RuntimeFailure::new(class, RuntimeStage::Content)
}
impl RuntimeEngine for SemanticRuntimeEngine {
    fn feed(&mut self, bytes: &[u8]) -> Result<EngineUpdate, RuntimeFailure> {
        let (tick, now) = self.now()?;
        self.emulator
            .feed_observed_at(bytes, tick, now)
            .map_err(content_failure)?;
        if self.emulator.graphics_failure().is_some() {
            return Err(RuntimeFailure::capacity(RuntimeStage::Engine));
        }
        self.update_base()?;
        Ok(EngineUpdate {
            replies: self.emulator.take_pty_responses(),
            changed: !bytes.is_empty(),
            observations: self.observations(tick, false)?,
        })
    }
    fn take_attention(&mut self) -> Vec<crate::attention::AttentionEvent> {
        self.emulator.take_attention_events()
    }
    fn apply(&mut self, command: &RuntimeCommand) -> Result<EngineEffect, RuntimeFailure> {
        let before = self.base;
        let mut bytes = Vec::new();
        match command {
            RuntimeCommand::Key(input) => self.key(input, &mut bytes)?,
            RuntimeCommand::InputMethodCommit(input) => {
                self.key(&KeyInput::input_method_commit(input.as_str()), &mut bytes)?
            }
            RuntimeCommand::RequestPaste(text) => {
                // The worker has approved and sanitized this immutable payload.
                // Its retained input lease covers the text plus 12 framing bytes.
                // Encode mechanically without legacy selection/viewport changes.
                let bracketed = self.bracketed_paste()?;
                let capacity = text
                    .as_str()
                    .len()
                    .checked_add(12)
                    .ok_or_else(|| RuntimeFailure::capacity(RuntimeStage::Input))?;
                bytes
                    .try_reserve_exact(capacity)
                    .map_err(|_| RuntimeFailure::capacity(RuntimeStage::Input))?;
                if bracketed {
                    bytes.extend_from_slice(b"\x1b[200~");
                }
                bytes.extend(text.as_str().bytes().map(|byte| {
                    if byte == b'\n' && !bracketed {
                        b'\r'
                    } else {
                        byte
                    }
                }));
                if bracketed {
                    bytes.extend_from_slice(b"\x1b[201~");
                }
            }
            RuntimeCommand::Pointer(input) => {
                let position = self.position(input.stamp, input.cell)?;
                let button = input.button.map(|button| match button {
                    PointerButton::Left => mouse::Button::Left,
                    PointerButton::Middle => mouse::Button::Middle,
                    PointerButton::Right => mouse::Button::Right,
                });
                match input.phase {
                    PointerPhase::Press => {
                        self.mouse(
                            mouse::Action::Press,
                            button,
                            position,
                            input.modifiers,
                            &mut bytes,
                        )?;
                        self.held_button = button;
                    }
                    PointerPhase::Motion => self.mouse(
                        mouse::Action::Motion,
                        self.held_button,
                        position,
                        input.modifiers,
                        &mut bytes,
                    )?,
                    PointerPhase::Release => {
                        self.mouse(
                            mouse::Action::Release,
                            button.or(self.held_button),
                            position,
                            input.modifiers,
                            &mut bytes,
                        )?;
                        self.held_button = None;
                    }
                }
            }
            RuntimeCommand::Wheel(input) => {
                let position = self.position(input.stamp, input.cell)?;
                if input.horizontal_steps.unsigned_abs() > 100
                    || input.vertical_steps.unsigned_abs() > 100
                {
                    return Err(RuntimeFailure::capacity(RuntimeStage::Input));
                }
                if self
                    .emulator
                    .terminal
                    .is_mouse_tracking()
                    .map_err(|_| engine_failure())?
                {
                    for (steps, positive, negative) in [
                        (
                            input.horizontal_steps,
                            mouse::Button::Six,
                            mouse::Button::Seven,
                        ),
                        (
                            input.vertical_steps,
                            mouse::Button::Four,
                            mouse::Button::Five,
                        ),
                    ] {
                        let button = if steps > 0 { positive } else { negative };
                        for _ in 0..steps.unsigned_abs() {
                            self.mouse(
                                mouse::Action::Press,
                                Some(button),
                                position,
                                input.modifiers,
                                &mut bytes,
                            )?;
                        }
                    }
                } else if self.base.screen == Screen::Alternate
                    && self
                        .emulator
                        .terminal
                        .mode(Mode::ALT_SCROLL)
                        .map_err(|_| engine_failure())?
                {
                    let mut key = KeyInput::text_input("");
                    key.text = None;
                    key.physical_key = if input.vertical_steps > 0 {
                        PhysicalKey::ArrowUp
                    } else {
                        PhysicalKey::ArrowDown
                    };
                    for _ in 0..input.vertical_steps.unsigned_abs() {
                        self.keyboard
                            .encode(&self.emulator.terminal, &key, &mut bytes)
                            .map_err(|_| engine_failure())?;
                    }
                }
            }
            RuntimeCommand::Appearance(value) => {
                self.emulator
                    .apply_appearance(appearance(value))
                    .map_err(|_| engine_failure())?;
                set_palette(&mut self.emulator, value)?;
            }
            RuntimeCommand::FocusReport(focused) => {
                bytes = self
                    .emulator
                    .focus(*focused)
                    .map_err(|_| engine_failure())?
                    .bytes
            }
            RuntimeCommand::ClearScreenAndScrollback => {
                bytes = self.emulator.clear_screen_and_scrollback().bytes
            }
            RuntimeCommand::Resize(_)
            | RuntimeCommand::CellPixelMetrics(_)
            | RuntimeCommand::ResolvePaste(..)
            | RuntimeCommand::CompleteNativeRequest(_) => {
                // The worker owns geometry reservation, paste approval and native
                // completion. Reaching this adapter through those arms is invalid.
                return Err(RuntimeFailure::new(
                    FailureClass::InvalidRequest,
                    RuntimeStage::Input,
                ));
            }
        }
        self.update_base()?;
        bytes.extend(self.emulator.take_pty_responses());
        Ok(EngineEffect {
            bytes,
            changed: self.base != before,
        })
    }
    fn resize(&mut self, grid: MeasuredGrid) -> Result<(), RuntimeFailure> {
        self.emulator
            .resize_content(geometry(grid))
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Resize))?;
        self.update_base()
    }
    fn release_input(&mut self) -> Result<Vec<u8>, RuntimeFailure> {
        let mut bytes = Vec::new();
        for key in self.held_keys.drain(..) {
            self.keyboard
                .encode(&self.emulator.terminal, &key, &mut bytes)
                .map_err(|_| engine_failure())?;
        }
        if let Some(button) = self.held_button {
            self.mouse(
                mouse::Action::Release,
                Some(button),
                self.last_mouse_position,
                InputModifiers::default(),
                &mut bytes,
            )?;
        }
        self.held_button = None;
        Ok(bytes)
    }
    fn content_base(&self) -> ContentBase {
        self.base
    }
    fn bracketed_paste(&self) -> Result<bool, RuntimeFailure> {
        self.emulator
            .bracketed_paste_mode()
            .map_err(|_| engine_failure())
    }
    fn poll(&mut self, hidden_input: bool) -> Result<EngineUpdate, RuntimeFailure> {
        let (tick, now) = self.now()?;
        let mut changed = self
            .emulator
            .advance_content_timers(now)
            .map_err(content_failure)?;
        let hidden_changed = self
            .emulator
            .set_content_hidden_input(hidden_input)
            .map_err(content_failure)?;
        changed |= hidden_changed;
        self.hidden_input = hidden_input;
        if self.emulator.content_revision() != self.base.revision {
            self.update_base()?;
        }
        Ok(EngineUpdate {
            replies: self.emulator.take_pty_responses(),
            changed,
            observations: self.observations(tick, hidden_changed)?,
        })
    }
    fn read_page(
        &mut self,
        request: &ContentPageRequest,
    ) -> Result<ContentPageResult, RuntimeFailure> {
        self.emulator.content_page(request).map_err(content_failure)
    }
    fn read_topology(
        &mut self,
        request: &TopologyPageRequest,
    ) -> Result<TopologyPageResult, RuntimeFailure> {
        self.emulator
            .topology_page(request)
            .map_err(content_failure)
    }
    fn begin_resource(
        &mut self,
        request: &ResourceRequest,
        lease: &BudgetLease,
    ) -> Result<EngineJobId, RuntimeFailure> {
        // The worker retains this non-cloneable lease until cancellation and
        // through queued chunks. It covers two chunks plus bounded scan scratch.
        if lease.bytes() < JOB_SCRATCH_BYTES || self.jobs.len() >= MAX_JOBS {
            return Err(RuntimeFailure::capacity(RuntimeStage::Content));
        }
        self.jobs
            .try_reserve(1)
            .map_err(|_| RuntimeFailure::capacity(RuntimeStage::Content))?;
        let id = NEXT_JOB
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                old.checked_add(1)
            })
            .map_err(|_| counter_failure())?
            .checked_add(1)
            .ok_or_else(counter_failure)?;
        let transfer = ResourceTransferId::new(id).map_err(|_| counter_failure())?;
        let job = match request {
            ResourceRequest::Copy(request) => Job::Copy(Box::new(
                self.emulator
                    .begin_copy(request, transfer)
                    .map_err(content_failure)?,
            )),
            ResourceRequest::Find(request) => Job::Find(Box::new(
                self.emulator.begin_find(request).map_err(content_failure)?,
            )),
            ResourceRequest::Graphics(request) => {
                if self.jobs.iter().any(
                    |(_, job)| matches!(job, Job::Graphics(id, _) if *id == request.transfer()),
                ) {
                    return Err(RuntimeFailure::new(
                        FailureClass::InvalidRequest,
                        RuntimeStage::Content,
                    ));
                }
                Job::Graphics(
                    request.transfer(),
                    Box::new(
                        self.emulator
                            .begin_graphics(request)
                            .map_err(content_failure)?,
                    ),
                )
            }
        };
        let id = EngineJobId(id);
        self.jobs.push((id, job));
        Ok(id)
    }
    fn step_resource(
        &mut self,
        id: EngineJobId,
        budget: ContentWorkBudget,
    ) -> Result<ResourceStep, RuntimeFailure> {
        let job = self
            .jobs
            .iter_mut()
            .find(|(key, _)| *key == id)
            .map(|(_, job)| job)
            .ok_or_else(|| content_failure(ContentReadFailure::Cancelled))?;
        let budget = ContentScanBudget {
            rows: budget.rows,
            cells: budget.cells,
            bytes: budget.bytes,
        };
        Ok(match job {
            Job::Copy(job) => match self
                .emulator
                .step_copy(job, budget)
                .map_err(content_failure)?
            {
                CopyStep::Pending => ResourceStep::Pending,
                CopyStep::Complete => ResourceStep::Complete,
                CopyStep::Item(item) => ResourceStep::Item(ResourceItem::Copy(Box::new(item))),
            },
            Job::Find(job) => match self
                .emulator
                .step_find(job, budget)
                .map_err(content_failure)?
            {
                FindStep::Pending => ResourceStep::Pending,
                FindStep::Complete => ResourceStep::Complete,
                FindStep::Item(item) => ResourceStep::Item(ResourceItem::Find(Box::new(item))),
            },
            Job::Graphics(_, job) => match self
                .emulator
                .step_graphics(job, budget)
                .map_err(content_failure)?
            {
                GraphicsStep::Complete => ResourceStep::Complete,
                GraphicsStep::Item(item) => {
                    ResourceStep::Item(ResourceItem::Graphics(Box::new(item)))
                }
            },
        })
    }
    fn cancel_resource(&mut self, id: EngineJobId) {
        self.jobs.retain(|(key, _)| *key != id);
    }
}
