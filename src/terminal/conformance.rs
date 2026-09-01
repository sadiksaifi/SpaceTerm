use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::accessibility::{
    AccessibilityCell, AccessibilityGeometry, AccessibilityLine, TerminalAccessibilityModel,
};
use super::attention::{AttentionEvent, AttentionFacts, AttentionState};
use super::emulator::{
    ActiveScreenSnapshot, CellSnapshot, CursorShapeSnapshot, TerminalColor, TerminalEmulator,
    TerminalUnderlineSnapshot,
};
use super::failure::{DiagnosticBundle, FailureClass, Recoverability, TerminalFailure};
use super::file_insertion::prepare_file_insertion;
use super::geometry::{
    BackingPosition, BackingScale, BackingSize, CellGridPosition, CellGridSize, LogicalCellSize,
    LogicalPosition, LogicalSize, TerminalGeometry,
};
use super::hyperlink::{HyperlinkKind, HyperlinkTarget, detect_url_cells};
use super::identity::{
    COLORTERM, COMPATIBILITY_PROGRAM_NAME, TERM_FALLBACK, TERM_NAME, XTVERSION, XtGetTcapObserver,
    launch_identity,
};
use super::key::{InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, PhysicalKey};
use super::metadata::{
    DirectoryProvenance, TerminalLocalFileCapabilities, parse_osc7_directory, sanitize_title,
};
use super::native_services::{
    NativeContextActions, NativeInsertion, NativeInsertionError, QuickLookTarget,
};
use super::osc52::{
    Osc52AccessPolicy, Osc52AuthorizationPolicy, Osc52Effect, Osc52Filter, Osc52Operation,
    Osc52Rejection, Osc52Target,
};
use super::paste::{MAX_PASTE_BYTES, PasteRejection, PreparedPaste};
use super::selection::{SelectionCopyOptions, TrailingSpacePolicy};
use super::session::{
    PointerButton, PointerInput, PointerPhase, ShiftSelectionPolicy, SurfacePosition, WheelInput,
    WheelPhase,
};
use crate::platform::macos_keyboard::{
    KeyTranslation, MacosKeyboardBridge, NativeKeyEvent, NativeModifiers,
};
use crate::platform::shell_integration::{
    ShellEnvironment, ShellIntegrationMode, ShellIntegrationStatus, ShellKind,
    plan_shell_integration, resource_root,
};
use crate::ui::{
    RenderLifecycle, ScaleChange, SurfaceVisibility, TerminalFocusBlocker,
    TerminalFocusCoordinator, TerminalFocusFacts, conformance_ime_observation,
};

const MAX_FIXTURE_STEPS: usize = 1_024;
const MAX_FIXTURE_INPUT_BYTES: usize = 64 * 1024;
const MAX_FIXTURE_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OracleKind {
    Bytes,
    SemanticSnapshot,
    Geometry,
    Lifecycle,
    Security,
    Native,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FixtureSpec {
    id: &'static str,
    issue: u8,
    stories: &'static [u8],
    authority: &'static str,
    oracle: OracleKind,
    max_steps: usize,
    max_input_bytes: usize,
    max_output_bytes: usize,
}

macro_rules! fixture {
    ($id:literal, $issue:literal, [$($story:literal),+], $authority:literal, $oracle:ident) => {
        FixtureSpec {
            id: $id,
            issue: $issue,
            stories: &[$($story),+],
            authority: $authority,
            oracle: OracleKind::$oracle,
            max_steps: 256,
            max_input_bytes: 16 * 1024,
            max_output_bytes: 16 * 1024,
        }
    };
}

const FIXTURES: &[FixtureSpec] = &[
    fixture!(
        "snapshot.damage-and-isolation",
        6,
        [45, 46],
        "spaceterm-snapshot-contract",
        SemanticSnapshot
    ),
    fixture!(
        "geometry.logical-and-backing",
        7,
        [34, 44],
        "ecma-48-and-xterm-window-ops",
        Geometry
    ),
    fixture!(
        "pty.initialization",
        8,
        [38, 46],
        "posix-and-darwin-pty",
        Lifecycle
    ),
    fixture!(
        "pty.shutdown",
        9,
        [39, 46],
        "posix-process-lifecycle",
        Lifecycle
    ),
    fixture!(
        "presentation.colors",
        10,
        [3],
        "ecma-48-sgr",
        SemanticSnapshot
    ),
    fixture!(
        "presentation.text-attributes",
        11,
        [4],
        "ecma-48-sgr",
        SemanticSnapshot
    ),
    fixture!(
        "presentation.decorations",
        12,
        [4],
        "ecma-48-and-xterm-sgr",
        SemanticSnapshot
    ),
    fixture!(
        "unicode.graphemes",
        13,
        [2],
        "unicode-uax-11-uax-29",
        SemanticSnapshot
    ),
    fixture!(
        "unicode.drawing-symbols",
        14,
        [2],
        "unicode-blocks",
        Geometry
    ),
    fixture!(
        "cursor.negotiated-shape",
        15,
        [5, 7],
        "dec-deccusr",
        SemanticSnapshot
    ),
    fixture!(
        "focus.terminal-input-focus",
        16,
        [8, 9, 10, 11, 12, 13, 46],
        "apple-responder-and-spaceterm-focus",
        Native
    ),
    fixture!(
        "keyboard.vocabulary",
        17,
        [1, 17, 18],
        "w3c-code-and-ghostty-key",
        Bytes
    ),
    fixture!(
        "keyboard.protocols",
        18,
        [18, 19],
        "kitty-keyboard-fixterms-xterm",
        Bytes
    ),
    fixture!(
        "keyboard.macos-bridge",
        19,
        [20, 21],
        "apple-nsevent",
        Native
    ),
    fixture!(
        "focus.dec-1004",
        20,
        [14, 15, 16],
        "xterm-focus-event",
        Bytes
    ),
    fixture!(
        "cursor.blink-lifecycle",
        21,
        [6, 7],
        "dec-deccusr-and-spaceterm-cadence",
        Lifecycle
    ),
    fixture!(
        "ime.marked-text",
        22,
        [22],
        "apple-nstextinputclient",
        Native
    ),
    fixture!(
        "input.secure-event",
        23,
        [23],
        "apple-secure-event-input",
        Security
    ),
    fixture!(
        "screen.scrollback-and-reflow",
        24,
        [33, 34],
        "ecma-48-and-xterm-private-modes",
        SemanticSnapshot
    ),
    fixture!(
        "mouse.protocols",
        25,
        [24, 25],
        "xterm-mouse-tracking",
        Bytes
    ),
    fixture!(
        "selection.semantic-ranges",
        26,
        [26],
        "spaceterm-selection-contract",
        SemanticSnapshot
    ),
    fixture!(
        "mouse.precision-wheel",
        27,
        [27],
        "apple-scroll-phases-and-xterm",
        Bytes
    ),
    fixture!("links.osc-8", 29, [28], "vte-osc-8", Security),
    fixture!(
        "clipboard.selection-copy",
        30,
        [29],
        "apple-pasteboard",
        SemanticSnapshot
    ),
    fixture!(
        "paste.unified-safety",
        31,
        [30],
        "xterm-bracketed-paste",
        Security
    ),
    fixture!(
        "paste.file-urls",
        32,
        [31],
        "apple-file-url-and-posix-shell",
        Security
    ),
    fixture!("clipboard.osc-52", 33, [32], "xterm-osc-52", Security),
    fixture!(
        "metadata.osc-7-and-133",
        34,
        [35],
        "osc-7-and-finalterm-osc-133",
        SemanticSnapshot
    ),
    fixture!(
        "shell.temporary-integration",
        35,
        [36],
        "shell-startup-contracts",
        Lifecycle
    ),
    fixture!(
        "identity.terminfo-and-runtime",
        36,
        [37],
        "ncurses-terminfo-and-xterm",
        Bytes
    ),
    fixture!(
        "attention.bell-and-notification",
        37,
        [41],
        "ecma-48-bel-and-apple-notifications",
        Native
    ),
    fixture!(
        "services.native-actions",
        38,
        [43],
        "apple-services-drag-and-quick-look",
        Native
    ),
    fixture!(
        "accessibility.editable-text",
        39,
        [42],
        "apple-nsaccessibility",
        Native
    ),
    fixture!(
        "render.visibility-lifecycle",
        40,
        [12, 13, 44, 45],
        "apple-window-visibility",
        Lifecycle
    ),
    fixture!(
        "failure.typed-local-diagnostics",
        41,
        [40],
        "spaceterm-failure-contract",
        Security
    ),
    fixture!(
        "graphics.kitty-static",
        89,
        [2, 34, 44, 45, 46],
        "kitty-graphics-protocol",
        SemanticSnapshot
    ),
];

const KEYBOARD_PROTOCOL_EXPECTED: &[ExpectedObservation] = &[
    ExpectedObservation {
        step: 1,
        field: "legacy-printable-bytes",
        value: "61",
    },
    ExpectedObservation {
        step: 2,
        field: "application-cursor-up-bytes",
        value: "1b 4f 41",
    },
];

const SEMANTIC_SNAPSHOT_EXPECTED: &[ExpectedObservation] = &[
    ExpectedObservation {
        step: 1,
        field: "cells",
        value: "r0[c0=A{fg=palette:1,bg=palette:4,bold,italic,underline=single} c2=é{width=1} c4=界{width=2}] r1[] r2[]",
    },
    ExpectedObservation {
        step: 1,
        field: "cursor",
        value: "visible=true shape=bar blinking=false column=6 row=0 width=1",
    },
    ExpectedObservation {
        step: 1,
        field: "metadata",
        value: "title=Corpus active-screen=primary rows=3 cols=12",
    },
    ExpectedObservation {
        step: 1,
        field: "frame",
        value: "generation=PresentationGeneration(1) row-count=3",
    },
];

const KITTY_GRAPHICS_EXPECTED: &[ExpectedObservation] = &[
    ExpectedObservation {
        step: 1,
        field: "rgba",
        value: "ff 00 00 ff",
    },
    ExpectedObservation {
        step: 1,
        field: "placement",
        value: "image=89 placement=7 origin=0,0 destination=1x1 z=0 virtual=false",
    },
    ExpectedObservation {
        step: 1,
        field: "damage",
        value: "content=true geometry=true text=Clean",
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
struct Observation {
    step: usize,
    field: &'static str,
    value: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpectedObservation {
    step: usize,
    field: &'static str,
    value: &'static str,
}

struct ExecutableFixture {
    spec: &'static FixtureSpec,
    expected: &'static [ExpectedObservation],
    observe: fn(&FixtureSpec, &mut FixtureBudget) -> Result<Vec<Observation>, String>,
}

#[derive(Debug)]
struct FixtureBudget {
    steps: usize,
    input_bytes: usize,
    output_bytes: usize,
}

impl FixtureBudget {
    const fn new() -> Self {
        Self {
            steps: 0,
            input_bytes: 0,
            output_bytes: 0,
        }
    }

    fn record(&mut self, input_bytes: usize, output_bytes: usize) {
        self.steps = self.steps.saturating_add(1);
        self.input_bytes = self.input_bytes.saturating_add(input_bytes);
        self.output_bytes = self.output_bytes.saturating_add(output_bytes);
    }

    fn verify(&self, spec: &FixtureSpec) -> Result<(), String> {
        for (name, actual, limit) in [
            ("steps", self.steps, spec.max_steps),
            ("input bytes", self.input_bytes, spec.max_input_bytes),
            ("output bytes", self.output_bytes, spec.max_output_bytes),
        ] {
            if actual > limit {
                return Err(format!(
                    "fixture `{}` exceeded {name} budget: {actual} > {limit}",
                    spec.id
                ));
            }
        }
        Ok(())
    }
}

fn run_fixture(fixture: &ExecutableFixture) -> Result<(), String> {
    let mut budget = FixtureBudget::new();
    let observed = (fixture.observe)(fixture.spec, &mut budget)
        .map_err(|error| format!("fixture `{}` failed: {error}", fixture.spec.id))?;
    budget.verify(fixture.spec)?;

    for (expected, observed) in fixture.expected.iter().zip(&observed) {
        if expected.step != observed.step
            || expected.field != observed.field
            || expected.value != observed.value
        {
            return Err(format!(
                "fixture `{}` step {} `{}` mismatch:\nexpected: {}\nobserved: {}",
                fixture.spec.id, expected.step, expected.field, expected.value, observed.value
            ));
        }
    }
    if fixture.expected.len() != observed.len() {
        return Err(format!(
            "fixture `{}` observation count mismatch: expected {}, observed {}",
            fixture.spec.id,
            fixture.expected.len(),
            observed.len()
        ));
    }
    Ok(())
}

fn observe_keyboard_protocols(
    _spec: &FixtureSpec,
    budget: &mut FixtureBudget,
) -> Result<Vec<Observation>, String> {
    let geometry = TerminalGeometry::from_grid(
        CellGridSize::new(80, 24),
        LogicalCellSize::new(8.0, 16.0),
        BackingScale::ONE,
    );
    let mut emulator = TerminalEmulator::new(geometry).map_err(|error| error.to_string())?;
    let printable = emulator.key(KeyInput {
        action: KeyAction::Press,
        physical_key: PhysicalKey::A,
        native_key_code: Some(0),
        logical_key: "a".to_owned(),
        text: Some("a".to_owned()),
        unshifted_codepoint: Some('a'),
        modifiers: InputModifiers::default(),
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::None,
    })?;
    budget.record(1, printable.bytes.len());

    const APPLICATION_CURSOR_MODE: &[u8] = b"\x1b[?1h";
    emulator.feed(APPLICATION_CURSOR_MODE);
    let cursor_up = emulator.key(KeyInput {
        action: KeyAction::Press,
        physical_key: PhysicalKey::ArrowUp,
        native_key_code: Some(126),
        logical_key: "ArrowUp".to_owned(),
        text: None,
        unshifted_codepoint: None,
        modifiers: InputModifiers::default(),
        consumed_modifiers: InputModifiers::default(),
        option_as_alt: OptionAsAltPolicy::None,
    })?;
    budget.record(APPLICATION_CURSOR_MODE.len(), cursor_up.bytes.len());

    Ok(vec![
        Observation {
            step: 1,
            field: "legacy-printable-bytes",
            value: hex_bytes(&printable.bytes),
        },
        Observation {
            step: 2,
            field: "application-cursor-up-bytes",
            value: hex_bytes(&cursor_up.bytes),
        },
    ])
}

fn observe_semantic_snapshot(
    _spec: &FixtureSpec,
    budget: &mut FixtureBudget,
) -> Result<Vec<Observation>, String> {
    let geometry = TerminalGeometry::from_grid(
        CellGridSize::new(12, 3),
        LogicalCellSize::new(8.0, 16.0),
        BackingScale::ONE,
    );
    let mut emulator = TerminalEmulator::new(geometry).map_err(|error| error.to_string())?;
    let input = concat!(
        "\x1b]2;Corpus\x07",
        "\x1b[31;44;1;3;4mA\x1b[0m e\u{301} 界",
        "\x1b[6 q"
    );
    emulator.feed(input.as_bytes());
    let snapshot = emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "terminal did not publish the semantic snapshot".to_owned())?;

    let cells = snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            let cells = row
                .iter()
                .enumerate()
                .filter(|(_, cell)| cell.text != " " && !cell.spacer_tail)
                .map(|(column, cell)| format!("c{column}={}", canonical_cell(column, row, cell)))
                .collect::<Vec<_>>()
                .join(" ");
            format!("r{row_index}[{cells}]")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let cursor = snapshot
        .cursor
        .position
        .map(|position| {
            format!(
                "visible={} shape={} blinking={} column={} row={} width={}",
                snapshot.cursor.visible,
                cursor_shape(snapshot.cursor.shape),
                snapshot.cursor.blinking,
                position.column,
                position.row,
                position.width_cells
            )
        })
        .unwrap_or_else(|| "visible=false position=none".to_owned());
    let metadata = format!(
        "title={} active-screen={} rows={} cols={}",
        snapshot.title,
        match snapshot.active_screen {
            ActiveScreenSnapshot::Primary => "primary",
            ActiveScreenSnapshot::Alternate => "alternate",
        },
        snapshot.size.rows,
        snapshot.size.cols
    );
    let observations = vec![
        Observation {
            step: 1,
            field: "cells",
            value: cells,
        },
        Observation {
            step: 1,
            field: "cursor",
            value: cursor,
        },
        Observation {
            step: 1,
            field: "metadata",
            value: metadata,
        },
        Observation {
            step: 1,
            field: "frame",
            value: format!(
                "generation={:?} row-count={}",
                snapshot.generation,
                snapshot.rows.len()
            ),
        },
    ];
    budget.record(
        input.len(),
        observations
            .iter()
            .map(|observation| observation.value.len())
            .sum(),
    );
    Ok(observations)
}

fn observe_kitty_graphics(
    _spec: &FixtureSpec,
    budget: &mut FixtureBudget,
) -> Result<Vec<Observation>, String> {
    let _guard = super::graphics::test_lock();
    let geometry = TerminalGeometry::from_grid(
        CellGridSize::new(12, 3),
        LogicalCellSize::new(8.0, 16.0),
        BackingScale::ONE,
    );
    let mut emulator = TerminalEmulator::new(geometry).map_err(|error| error.to_string())?;
    emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "terminal did not publish its initial snapshot".to_owned())?;
    let input = b"\x1b_Ga=T,t=d,f=24,i=89,p=7,s=1,v=1,C=1;/wAA\x1b\\";
    emulator.feed(input);
    let replies = emulator.take_pty_responses();
    budget.record(input.len(), replies.len());
    let snapshot = emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "terminal did not publish Kitty graphics".to_owned())?;
    let image = snapshot
        .graphics
        .images
        .first()
        .ok_or_else(|| "snapshot omitted decoded Kitty image".to_owned())?;
    let placement = snapshot
        .graphics
        .placements
        .first()
        .ok_or_else(|| "snapshot omitted Kitty placement".to_owned())?;

    Ok(vec![
        Observation {
            step: 1,
            field: "rgba",
            value: hex_bytes(&image.rgba),
        },
        Observation {
            step: 1,
            field: "placement",
            value: format!(
                "image={} placement={} origin={},{} destination={}x{} z={} virtual={}",
                placement.image.image_id,
                placement.placement_id,
                placement.viewport_col,
                placement.viewport_row,
                placement.destination_width,
                placement.destination_height,
                placement.z,
                placement.unicode_placeholder,
            ),
        },
        Observation {
            step: 1,
            field: "damage",
            value: format!(
                "content={} geometry={} text={:?}",
                snapshot.damage.graphics_content,
                snapshot.damage.graphics_geometry,
                snapshot.damage.content,
            ),
        },
    ])
}

fn canonical_cell(column: usize, row: &[CellSnapshot], cell: &CellSnapshot) -> String {
    let styled = cell.foreground_source != TerminalColor::Default
        || cell.background_source != TerminalColor::Default
        || cell.bold
        || cell.italic
        || cell.underline != TerminalUnderlineSnapshot::None;
    if !styled {
        let width = usize::from(row.get(column + 1).is_some_and(|next| next.spacer_tail)) + 1;
        return format!("{}{{width={width}}}", cell.text);
    }
    format!(
        "{}{{fg={},bg={},{}{}underline={}}}",
        cell.text,
        terminal_color(cell.foreground_source),
        terminal_color(cell.background_source),
        if cell.bold { "bold," } else { "" },
        if cell.italic { "italic," } else { "" },
        underline(cell.underline)
    )
}

fn terminal_color(color: TerminalColor) -> String {
    match color {
        TerminalColor::Default => "default".to_owned(),
        TerminalColor::Palette(index) => format!("palette:{index}"),
        TerminalColor::Rgb(color) => format!("rgb:{:08x}", color.rgba_hex()),
    }
}

const fn underline(value: TerminalUnderlineSnapshot) -> &'static str {
    match value {
        TerminalUnderlineSnapshot::None => "none",
        TerminalUnderlineSnapshot::Single => "single",
        TerminalUnderlineSnapshot::Double => "double",
        TerminalUnderlineSnapshot::Curly => "curly",
        TerminalUnderlineSnapshot::Dotted => "dotted",
        TerminalUnderlineSnapshot::Dashed => "dashed",
    }
}

const fn cursor_shape(value: CursorShapeSnapshot) -> &'static str {
    match value {
        CursorShapeSnapshot::Bar => "bar",
        CursorShapeSnapshot::Block => "block",
        CursorShapeSnapshot::Underline => "underline",
        CursorShapeSnapshot::BlockHollow => "hollow-block",
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

const PASS_EXPECTED: &[ExpectedObservation] = &[ExpectedObservation {
    step: 1,
    field: "capability",
    value: "pass",
}];

fn observe_advertised_capability(
    spec: &FixtureSpec,
    budget: &mut FixtureBudget,
) -> Result<Vec<Observation>, String> {
    match spec.issue {
        6 => check_snapshot_isolation()?,
        7 => check_geometry()?,
        8 => check_pty_initialization()?,
        9 => check_pty_shutdown()?,
        10..=15 => check_presentation(spec.issue)?,
        16 => check_terminal_focus()?,
        17..=18 => check_keyboard_encoding()?,
        19 => check_macos_keyboard_bridge()?,
        20 => check_focus_reporting()?,
        21 => check_cursor_blink_lifecycle()?,
        22 => check_ime()?,
        23 => check_secure_input()?,
        24 => check_screens_scrollback_and_reflow()?,
        25 => check_mouse_protocols()?,
        26 => check_selection_and_copy(false)?,
        27 => check_precision_wheel()?,
        29 => check_hyperlinks()?,
        30 => check_selection_and_copy(true)?,
        31 => check_paste_safety()?,
        32 => check_file_insertion()?,
        33 => check_osc52()?,
        34 => check_metadata()?,
        35 => check_shell_integration()?,
        36 => check_identity()?,
        37 => check_attention()?,
        38 => check_native_services()?,
        39 => check_accessibility()?,
        40 => check_render_lifecycle()?,
        41 => check_typed_failures()?,
        89 => check_kitty_graphics()?,
        issue => {
            return Err(format!(
                "issue #{issue} has no executable conformance driver"
            ));
        }
    }
    budget.record(0, 4);
    Ok(vec![Observation {
        step: 1,
        field: "capability",
        value: "pass".to_owned(),
    }])
}

fn check_kitty_graphics() -> Result<(), String> {
    run_fixture(&ExecutableFixture {
        spec: &FIXTURES[35],
        expected: KITTY_GRAPHICS_EXPECTED,
        observe: observe_kitty_graphics,
    })
}

fn require(
    condition: bool,
    field: &'static str,
    detail: impl std::fmt::Display,
) -> Result<(), String> {
    condition
        .then_some(())
        .ok_or_else(|| format!("step 1 `{field}` mismatch: {detail}"))
}

fn require_eq<T>(field: &'static str, actual: T, expected: T) -> Result<(), String>
where
    T: std::fmt::Debug + PartialEq,
{
    require(
        actual == expected,
        field,
        format!("expected {expected:?}, observed {actual:?}"),
    )
}

fn emulator(cols: u16, rows: u16) -> Result<TerminalEmulator, String> {
    TerminalEmulator::new(TerminalGeometry::from_grid(
        CellGridSize::new(cols, rows),
        LogicalCellSize::new(8.0, 18.0),
        BackingScale::ONE,
    ))
    .map_err(|error| error.to_string())
}

fn check_snapshot_isolation() -> Result<(), String> {
    let mut first = emulator(8, 2)?;
    let mut second = emulator(8, 2)?;
    first.feed(b"alpha");
    second.feed(b"beta");
    let first_snapshot = first
        .snapshot()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "first Session published no snapshot".to_owned())?;
    let second_snapshot = second
        .snapshot()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "second Session published no snapshot".to_owned())?;
    require_eq("first-cell", first_snapshot.rows[0][0].text.as_str(), "a")?;
    require_eq("second-cell", second_snapshot.rows[0][0].text.as_str(), "b")?;
    require(
        !std::sync::Arc::ptr_eq(&first_snapshot.rows, &second_snapshot.rows),
        "snapshot-isolation",
        "two Sessions shared row storage",
    )?;
    require(
        first_snapshot.damage.content != super::emulator::ContentDamageSnapshot::Clean,
        "snapshot-damage",
        format!("observed {:?}", first_snapshot.damage.content),
    )
}

fn check_geometry() -> Result<(), String> {
    let geometry = TerminalGeometry::from_viewport(
        LogicalSize::new(101.0, 52.0),
        LogicalCellSize::new(7.5, 17.25),
        BackingScale::new(2.0).ok_or_else(|| "invalid scale".to_owned())?,
        CellGridSize::new(2, 2),
    );
    require_eq("grid", geometry.grid(), CellGridSize::new(13, 3))?;
    require_eq(
        "backing-cell",
        geometry.backing_cell_size(),
        BackingSize::new(15, 35),
    )?;
    require_eq(
        "logical-to-backing",
        geometry.to_backing_position(LogicalPosition::new(11.25, 8.625)),
        BackingPosition::new(22.5, 17.25),
    )?;
    require_eq(
        "backing-to-cell",
        geometry.cell_at_backing_position(BackingPosition::new(22.5, 35.0)),
        CellGridPosition::new(1, 1),
    )
}

fn check_pty_initialization() -> Result<(), String> {
    let observation = crate::platform::macos_pty::conformance_initialization_observation();
    for expected in [
        "argv=[\"/bin/zsh\", \"-l\"]",
        "cwd=/tmp",
        "term=xterm-256color",
        "colorterm=truecolor",
        "program=ghostty",
        "spaceterm=1",
        "controlling-tty=true",
    ] {
        require(
            observation.contains(expected),
            "pty-initialization",
            format!("expected `{expected}` in `{observation}`"),
        )?;
    }
    Ok(())
}

fn check_pty_shutdown() -> Result<(), String> {
    require_eq(
        "pty-shutdown",
        crate::platform::macos_pty::conformance_shutdown_observation(),
        "first=true duplicate=true signals=1 disposition=Graceful revoked=true".to_owned(),
    )
}

fn check_presentation(issue: u8) -> Result<(), String> {
    let mut emulator = emulator(12, 3)?;
    let bytes: &[u8] = match issue {
        10 => b"\x1b[38;2;1;2;3;48;5;17mC\x1b[7mR",
        11 => b"\x1b[1;2;3;5;8mA",
        12 => b"\x1b[4:3;9;53mD",
        13 => "e\u{301} 😀 界".as_bytes(),
        14 => "─│┌┘".as_bytes(),
        15 => b"\x1b[6 q\x1b[31m\x1b]12;#010203\x07X",
        _ => return Err(format!("issue #{issue} is not a presentation fixture")),
    };
    emulator.feed(bytes);
    let snapshot = emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "presentation produced no snapshot".to_owned())?;
    match issue {
        10 => {
            require(
                snapshot.rows[0][0].foreground_source
                    == TerminalColor::Rgb(crate::theme::Color::from_rgb_components(1, 2, 3)),
                "color-source",
                "truecolor foreground was not preserved",
            )?;
            require_eq(
                "palette-background",
                snapshot.rows[0][0].background_source,
                TerminalColor::Palette(17),
            )?;
            require(
                snapshot.rows[0][1].inverse,
                "reverse",
                "reverse attribute was not preserved",
            )?;
        }
        11 => {
            let cell = &snapshot.rows[0][0];
            require(
                cell.bold && cell.faint && cell.italic && cell.blinking && cell.invisible,
                "text-attributes",
                format!("observed {cell:?}"),
            )?;
        }
        12 => {
            let cell = &snapshot.rows[0][0];
            require_eq(
                "underline",
                cell.underline,
                TerminalUnderlineSnapshot::Curly,
            )?;
            require(
                cell.strikethrough && cell.overline,
                "decorations",
                format!("observed {cell:?}"),
            )?;
        }
        13 => {
            require_eq(
                "combined-grapheme",
                snapshot.rows[0][0].text.as_str(),
                "e\u{301}",
            )?;
            require(
                snapshot.rows[0][3].spacer_tail,
                "emoji-width",
                "emoji did not occupy two cells",
            )?;
            require(
                snapshot.rows[0][6].spacer_tail,
                "wide-width",
                "CJK scalar did not occupy two cells",
            )?;
        }
        14 => {
            require_eq(
                "drawing-symbols",
                snapshot.rows[0][..4]
                    .iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>(),
                "─│┌┘".to_owned(),
            )?;
        }
        15 => {
            require_eq(
                "cursor-shape",
                snapshot.cursor.shape,
                CursorShapeSnapshot::Bar,
            )?;
            require(
                !snapshot.cursor.blinking,
                "cursor-blink",
                "steady DECSCUSR cursor blinked",
            )?;
            require_eq(
                "cursor-width",
                snapshot
                    .cursor
                    .position
                    .map(|position| position.width_cells),
                Some(1),
            )?;
        }
        _ => unreachable!(),
    }
    Ok(())
}

fn check_terminal_focus() -> Result<(), String> {
    let focused = TerminalFocusFacts {
        active_workspace: true,
        active_window: true,
        focused_pane: true,
        responder: true,
        operating_system_window_key: true,
        application_active: true,
        blocker: None,
    };
    require(
        TerminalFocusCoordinator::is_focused(focused),
        "focus-positive",
        "all focus facts were true",
    )?;
    require(
        !TerminalFocusCoordinator::is_focused(TerminalFocusFacts {
            blocker: Some(TerminalFocusBlocker::ContextMenu),
            ..focused
        }),
        "focus-blocker",
        "Context Menu did not take Terminal Input Focus",
    )
}

fn check_keyboard_encoding() -> Result<(), String> {
    let spec = &FIXTURES[12];
    let mut budget = FixtureBudget::new();
    let observed = observe_keyboard_protocols(spec, &mut budget)?;
    require_eq("keyboard-golden", observed[0].value.as_str(), "61")?;
    require_eq("cursor-golden", observed[1].value.as_str(), "1b 4f 41")
}

fn check_macos_keyboard_bridge() -> Result<(), String> {
    let bridge = MacosKeyboardBridge::new(OptionAsAltPolicy::Left);
    let translation = bridge.translate(NativeKeyEvent {
        action: KeyAction::Press,
        native_key_code: 0,
        characters: Some("å".to_owned()),
        characters_ignoring_modifiers: Some("a".to_owned()),
        unmodified_characters: Some("a".to_owned()),
        characters_without_option: Some("a".to_owned()),
        modifiers: NativeModifiers {
            alt: true,
            alt_left: true,
            ..NativeModifiers::default()
        },
    });
    let KeyTranslation::Encoded(input) = translation else {
        return Err(format!(
            "expected encoded macOS key, observed {translation:?}"
        ));
    };
    require_eq("physical-key", input.physical_key, PhysicalKey::A)?;
    require_eq("option-as-alt-text", input.text.as_deref(), Some("a"))?;
    require(
        input.modifiers.alt && !input.consumed_modifiers.alt,
        "modifier-routing",
        format!("observed {input:?}"),
    )
}

fn check_focus_reporting() -> Result<(), String> {
    let mut emulator = emulator(10, 2)?;
    require_eq("focus-disabled", emulator.focus(true)?.bytes, Vec::new())?;
    emulator.feed(b"\x1b[?1004h");
    require_eq(
        "focus-gained",
        emulator.focus(true)?.bytes,
        b"\x1b[I".to_vec(),
    )?;
    require_eq(
        "focus-lost",
        emulator.focus(false)?.bytes,
        b"\x1b[O".to_vec(),
    )
}

fn check_cursor_blink_lifecycle() -> Result<(), String> {
    let mut emulator = emulator(10, 2)?;
    emulator.feed(b"\x1b[1 q");
    let blinking = emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .unwrap();
    require(
        blinking.cursor.blinking,
        "cursor-blink-mode",
        "blinking block cursor was steady",
    )?;
    emulator.feed(b"\x1b[2 q");
    let steady = emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .unwrap();
    require(
        !steady.cursor.blinking,
        "cursor-steady-mode",
        "steady block cursor blinked",
    )
}

fn check_ime() -> Result<(), String> {
    let observation = conformance_ime_observation();
    for expected in ["marked=A界B", "selection=2..2", "commit=Some(\"A界B\")"] {
        require(
            observation.contains(expected),
            "ime-state",
            format!("expected `{expected}` in `{observation}`"),
        )?;
    }
    Ok(())
}

fn check_secure_input() -> Result<(), String> {
    require_eq(
        "secure-input-balance",
        crate::platform::macos_secure_input::conformance_secure_input_observation(),
        "transitions=[true, false, true, false] enabled=false".to_owned(),
    )
}

fn check_screens_scrollback_and_reflow() -> Result<(), String> {
    let mut emulator = emulator(5, 3)?;
    emulator.feed(b"one\r\ntwo\r\nthree\r\nfour");
    let before = emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .unwrap();
    require(
        before.scrollbar.total_rows > before.scrollbar.visible_rows,
        "scrollback",
        format!("observed {:?}", before.scrollbar),
    )?;
    emulator.feed(b"\x1b[?1049hALT");
    let alternate = emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .unwrap();
    require_eq(
        "alternate-screen",
        alternate.active_screen,
        ActiveScreenSnapshot::Alternate,
    )?;
    emulator
        .resize(TerminalGeometry::from_grid(
            CellGridSize::new(10, 3),
            LogicalCellSize::new(8.0, 18.0),
            BackingScale::ONE,
        ))
        .map_err(|error| error.to_string())?;
    emulator.feed(b"\x1b[?1049l");
    let restored = emulator
        .snapshot()
        .map_err(|error| error.to_string())?
        .unwrap();
    require_eq(
        "primary-screen",
        restored.active_screen,
        ActiveScreenSnapshot::Primary,
    )?;
    require_eq("resized-columns", restored.size.cols, 10)
}

fn pointer_input(
    emulator: &TerminalEmulator,
    phase: PointerPhase,
    button: Option<PointerButton>,
    x: f32,
    y: f32,
    shift: bool,
) -> PointerInput {
    PointerInput {
        generation: emulator.presentation_generation(),
        phase,
        button,
        position: SurfacePosition { x, y },
        modifiers: InputModifiers {
            shift,
            ..InputModifiers::default()
        },
        shift_selection: ShiftSelectionPolicy::default(),
    }
}

fn check_mouse_protocols() -> Result<(), String> {
    let mut emulator = emulator(12, 3)?;
    emulator.feed(b"hello world\x1b[?1000h\x1b[?1006h");
    _ = emulator.snapshot().map_err(|error| error.to_string())?;
    let report = emulator.pointer(pointer_input(
        &emulator,
        PointerPhase::Press,
        Some(PointerButton::Left),
        2.0,
        10.0,
        false,
    ))?;
    require_eq("sgr-mouse-press", report.bytes, b"\x1b[<0;1;1M".to_vec())?;
    _ = emulator.pointer(pointer_input(
        &emulator,
        PointerPhase::Release,
        Some(PointerButton::Left),
        2.0,
        10.0,
        false,
    ))?;
    let selection = emulator.pointer(pointer_input(
        &emulator,
        PointerPhase::Press,
        Some(PointerButton::Left),
        2.0,
        10.0,
        true,
    ))?;
    require(
        selection.bytes.is_empty() && selection.screen_changed,
        "shift-selection-override",
        format!("observed {selection:?}"),
    )
}

fn select_hello(emulator: &mut TerminalEmulator) -> Result<(), String> {
    emulator.feed(b"hello world");
    _ = emulator.snapshot().map_err(|error| error.to_string())?;
    for input in [
        pointer_input(
            emulator,
            PointerPhase::Press,
            Some(PointerButton::Left),
            2.0,
            10.0,
            false,
        ),
        pointer_input(emulator, PointerPhase::Motion, None, 48.0, 10.0, false),
        pointer_input(
            emulator,
            PointerPhase::Release,
            Some(PointerButton::Left),
            48.0,
            10.0,
            false,
        ),
    ] {
        emulator.pointer(input)?;
    }
    Ok(())
}

fn check_selection_and_copy(copy_semantics: bool) -> Result<(), String> {
    let mut emulator = emulator(12, 3)?;
    select_hello(&mut emulator)?;
    let copy = emulator
        .selection_copy(SelectionCopyOptions {
            trailing_spaces: TrailingSpacePolicy::Trim,
            include_html: copy_semantics,
            ..SelectionCopyOptions::default()
        })?
        .ok_or_else(|| "selection produced no copy payload".to_owned())?;
    require_eq("selection-text", copy.plain_text.as_str(), "hello")?;
    require_eq("selection-html", copy.html.is_some(), copy_semantics)
}

fn check_precision_wheel() -> Result<(), String> {
    let mut emulator = emulator(10, 2)?;
    emulator.feed(b"\x1b[?1000h\x1b[?1006h");
    let action = emulator.wheel(WheelInput {
        generation: emulator.presentation_generation(),
        horizontal_steps: 0,
        vertical_steps: 2,
        phase: WheelPhase::MomentumChanged,
        position: SurfacePosition { x: 1.0, y: 1.0 },
        modifiers: InputModifiers::default(),
        shift_selection: ShiftSelectionPolicy::ReportToApplication,
    })?;
    require_eq(
        "precision-wheel-bytes",
        action.bytes,
        b"\x1b[<64;1;1M\x1b[<64;1;1M".to_vec(),
    )
}

fn check_hyperlinks() -> Result<(), String> {
    let local_files = TerminalLocalFileCapabilities::Enabled;
    let target = HyperlinkTarget::url("https://example.test/path")
        .ok_or_else(|| "valid HTTPS link was rejected".to_owned())?;
    require_eq("link-kind", target.kind, HyperlinkKind::Url)?;
    require_eq(
        "activation-url",
        target.activation_url(local_files),
        Some("https://example.test/path".to_owned()),
    )?;
    let cells = ["go ".to_owned(), "https://example.test/path".to_owned()];
    let detected = detect_url_cells(&cells);
    require(
        detected[0].is_none() && detected[1].as_ref() == Some(&target),
        "link-cell-mapping",
        format!("observed {detected:?}"),
    )?;
    require(
        HyperlinkTarget::url("file:///tmp/secret").is_none(),
        "link-scheme",
        "file URL passed URL authorization",
    )?;

    let directory = std::env::temp_dir().join(format!(
        "spaceterm-conformance-hyperlink-{}",
        std::process::id()
    ));
    _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let file = directory.join("preview file.txt");
    std::fs::write(&file, b"preview").map_err(|error| error.to_string())?;
    let result = (|| {
        let local = HyperlinkTarget::osc8("file:preview%20file.txt", &directory, None, local_files)
            .ok_or_else(|| "valid local OSC 8 target was rejected".to_owned())?;
        require_eq("local-link-kind", local.kind, HyperlinkKind::LocalPath)?;
        require_eq(
            "local-link-path",
            local.value.as_str(),
            file.canonicalize()
                .map_err(|error| error.to_string())?
                .to_str()
                .ok_or_else(|| "canonical fixture path was not UTF-8".to_owned())?,
        )?;
        let retained = HyperlinkTarget::from_local_emission_metadata(
            &local
                .local_emission_metadata(local_files)
                .ok_or_else(|| "valid local target metadata exceeded its bound".to_owned())?,
            local_files,
        )
        .ok_or_else(|| "resolver-only local target metadata did not decode".to_owned())?;
        require_eq("local-link-emission-identity", retained, local.clone())?;
        require(
            HyperlinkTarget::osc8(
                &format!("file://remote.test{}", file.to_string_lossy()),
                &directory,
                Some("mac.local"),
                local_files,
            )
            .is_none()
                && HyperlinkTarget::osc8("file:missing.txt", &directory, None, local_files)
                    .is_none(),
            "local-link-rejections",
            "remote or missing local OSC 8 target was accepted",
        )?;

        let next_directory = directory.join("next");
        std::fs::create_dir_all(&next_directory).map_err(|error| error.to_string())?;
        let next_file = next_directory.join("preview file.txt");
        std::fs::write(&next_file, b"next").map_err(|error| error.to_string())?;
        let next = HyperlinkTarget::osc8(
            "file:preview%20file.txt",
            &next_directory,
            None,
            local_files,
        )
        .ok_or_else(|| "second valid local OSC 8 target was rejected".to_owned())?;
        let local_url = local
            .activation_url(local_files)
            .ok_or_else(|| "fresh local OSC 8 target became inert".to_owned())?;
        let next_url = next
            .activation_url(local_files)
            .ok_or_else(|| "fresh second local OSC 8 target became inert".to_owned())?;
        require_eq(
            "local-link-canonical-emission-directory",
            (local.canonical_file_url(), next.canonical_file_url()),
            (Some(local_url), Some(next_url)),
        )
    })();
    _ = std::fs::remove_dir_all(directory);
    result
}

fn check_paste_safety() -> Result<(), String> {
    let bracketed_multiline =
        PreparedPaste::prepare("one\ntwo".to_owned()).map_err(|error| error.to_string())?;
    require(
        !bracketed_multiline.requires_confirmation(true),
        "paste-confirmation-bracketed-multiline",
        "ordinary bracketed multiline paste required confirmation",
    )?;
    let prepared = PreparedPaste::prepare("one\r\ntwo\x1b[201~".to_owned())
        .map_err(|error| error.to_string())?;
    require(
        prepared.requires_confirmation(true),
        "paste-confirmation-bracketed-fence",
        "bracketed closing-fence paste was treated as safe",
    )?;
    require_eq(
        "paste-normalization",
        prepared.into_text(),
        "one\ntwo\x1b[201~".to_owned(),
    )?;
    require_eq(
        "paste-size-limit",
        PreparedPaste::prepare("x".repeat(MAX_PASTE_BYTES + 1)),
        Err(PasteRejection::TooLarge {
            limit: MAX_PASTE_BYTES,
        }),
    )
}

fn check_file_insertion() -> Result<(), String> {
    let insertion =
        prepare_file_insertion(&[PathBuf::from("/tmp/a b'c"), PathBuf::from("/tmp/界")])?;
    require_eq(
        "file-shell-quoting",
        insertion.text,
        "'/tmp/a b'\"'\"'c' '/tmp/界'".to_owned(),
    )?;
    require(
        prepare_file_insertion(&[PathBuf::from("relative")]).is_err(),
        "absolute-file-gate",
        "relative path was accepted",
    )
}

fn check_osc52() -> Result<(), String> {
    require_eq(
        "osc52-policy",
        Osc52AuthorizationPolicy::default(),
        Osc52AuthorizationPolicy {
            read: Osc52AccessPolicy::Deny,
            write: Osc52AccessPolicy::Deny,
        },
    )?;
    let mut filter = Osc52Filter::default();
    let effects = filter.feed(b"\x1b]52;s;aGVsbG8=\x07");
    require(
        effects.iter().any(|effect| matches!(effect, Osc52Effect::Operation(Osc52Operation::Write { target: Osc52Target::Selection, text }) if text == "hello")),
        "osc52-write",
        format!("observed {effects:?}"),
    )?;
    let rejected = filter.feed(b"\x1b]52;c;abc\x07");
    require(
        rejected
            .iter()
            .any(|effect| matches!(effect, Osc52Effect::Rejected(Osc52Rejection::InvalidBase64))),
        "osc52-invalid-base64",
        format!("observed {rejected:?}"),
    )
}

fn check_metadata() -> Result<(), String> {
    require_eq(
        "safe-title",
        sanitize_title("  cargo\u{1b}]2;forged\u{7}  "),
        "cargo]2;forged".to_owned(),
    )?;
    let directory = parse_osc7_directory("file://localhost/Users/me/My%20Project", None)
        .ok_or_else(|| "local OSC 7 directory was rejected".to_owned())?;
    require_eq("osc7-path", directory.path.as_ref(), "/Users/me/My Project")?;
    require_eq(
        "osc7-provenance",
        directory.provenance,
        DirectoryProvenance::Osc7,
    )?;
    require(
        parse_osc7_directory("file://remote.test/tmp", None).is_none(),
        "remote-osc7",
        "remote authority was accepted",
    )
}

fn check_shell_integration() -> Result<(), String> {
    let resources = resource_root();
    let inherited = ShellEnvironment::default();
    let zsh = plan_shell_integration(
        Path::new("/bin/zsh"),
        &resources,
        ShellIntegrationMode::Automatic,
        &inherited,
    );
    require_eq(
        "zsh-integration",
        zsh.status,
        ShellIntegrationStatus::Applied(ShellKind::Zsh),
    )?;
    let disabled = plan_shell_integration(
        Path::new("/bin/zsh"),
        &resources,
        ShellIntegrationMode::Disabled,
        &inherited,
    );
    require_eq(
        "disabled-integration",
        disabled.status,
        ShellIntegrationStatus::Disabled,
    )
}

fn check_identity() -> Result<(), String> {
    require(
        XTVERSION.starts_with("SpaceTerm "),
        "program-name",
        XTVERSION,
    )?;
    require_eq(
        "compatibility-program-name",
        COMPATIBILITY_PROGRAM_NAME,
        "ghostty",
    )?;
    require_eq("colorterm", COLORTERM, "truecolor")?;
    let identity = launch_identity(&resource_root());
    require(
        matches!(identity.term, TERM_NAME | TERM_FALLBACK),
        "term-identity",
        format!("observed {}", identity.term),
    )?;
    let mut observer = XtGetTcapObserver::new(identity.term);
    let mut replies = Vec::new();
    observer.feed(b"\x1bP+q544e;436f;524742\x1b\\", &mut replies);
    let reply = String::from_utf8(replies).map_err(|error| error.to_string())?;
    require(
        reply.contains("1+r544e=")
            && reply.contains("1+r436f=323536")
            && reply.contains("1+r524742=38"),
        "terminfo-runtime-agreement",
        reply,
    )
}

fn check_attention() -> Result<(), String> {
    let epoch = Instant::now();
    let mut state = AttentionState::default();
    let first = state.observe(AttentionEvent::Bell, AttentionFacts::default(), epoch);
    let storm = state.observe(
        AttentionEvent::Bell,
        AttentionFacts::default(),
        epoch + Duration::from_millis(20),
    );
    require(
        first.audio_bell
            && first.visual_bell
            && first.request_dock_attention
            && first.notification.is_some(),
        "attention-first",
        format!("observed {first:?}"),
    )?;
    require(
        !storm.audio_bell && storm.unread_count == 1,
        "attention-storm",
        format!("observed {storm:?}"),
    )
}

fn check_native_services() -> Result<(), String> {
    let local_files = TerminalLocalFileCapabilities::Enabled;
    let url = HyperlinkTarget::url("https://example.test").unwrap();
    require_eq(
        "context-actions",
        NativeContextActions::from_presence(local_files, true, Some(&url)),
        NativeContextActions {
            copy: true,
            open_link: true,
            quick_look: false,
        },
    )?;
    let insertion = NativeInsertion::dropped_files(&[PathBuf::from("/tmp/a b")], true, local_files)
        .map_err(|error| format!("native insertion failed: {error:?}"))?;
    require_eq("native-file-insertion", insertion.text(), "'/tmp/a b'")?;
    require_eq(
        "native-focus-gate",
        NativeInsertion::service_text("ignored", false),
        Err(NativeInsertionError::TerminalUnfocused),
    )?;

    let directory = std::env::temp_dir().join(format!(
        "spaceterm-conformance-quick-look-{}",
        std::process::id()
    ));
    _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let file = directory.join("preview.txt");
    std::fs::write(&file, b"preview").map_err(|error| error.to_string())?;
    let result = (|| {
        let local = HyperlinkTarget::osc8(
            &format!("file://{}", file.to_string_lossy()),
            &directory,
            None,
            local_files,
        )
        .ok_or_else(|| "valid Quick Look link was rejected".to_owned())?;
        require_eq(
            "quick-look-local-regular-file",
            NativeContextActions::from_presence(local_files, false, Some(&local)).quick_look,
            true,
        )?;
        std::fs::remove_file(&file).map_err(|error| error.to_string())?;
        require(
            QuickLookTarget::from_link(&local, local_files).is_none()
                && NativeContextActions::from_presence(local_files, false, Some(&local)).quick_look,
            "quick-look-stale-path",
            "missing local file remained executable or immutable eligibility was lost",
        )
    })();
    _ = std::fs::remove_dir_all(directory);
    result
}

fn check_accessibility() -> Result<(), String> {
    let model = TerminalAccessibilityModel::new(
        vec![AccessibilityLine::new(
            vec![
                AccessibilityCell::new("A", 1, false),
                AccessibilityCell::new("😀", 2, true),
                AccessibilityCell::new("B", 1, false),
            ],
            false,
        )],
        0..1,
        Some((0, 3)),
    );
    require_eq("accessibility-text", model.text(), "A😀B")?;
    require_eq(
        "accessibility-selection",
        model.selection_range(),
        Some(1..3),
    )?;
    require_eq("accessibility-cursor", model.cursor_range(), 3..3)?;
    let geometry = AccessibilityGeometry::new(10.0, 20.0, 8.0, 18.0).unwrap();
    require_eq(
        "accessibility-bounds",
        model.bounds_for_range(1..3, geometry),
        Some((18.0, 20.0, 16.0, 18.0)),
    )
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

fn check_render_lifecycle() -> Result<(), String> {
    let mut lifecycle = RenderLifecycle::new(SurfaceVisibility {
        occluded: true,
        ..visible_surface()
    });
    let generation = super::emulator::PresentationGeneration::test(7);
    require(
        !lifecycle.observe_snapshot(generation).request_redraw,
        "hidden-redraw",
        "occluded Surface requested redraw",
    )?;
    let restored = lifecycle.update_visibility(visible_surface());
    require(
        restored.request_redraw && restored.animations_active,
        "restore-redraw",
        format!("observed {restored:?}"),
    )?;
    require_eq("latest-frame", lifecycle.take_frame(), Some(generation))?;
    require_eq(
        "scale-resources",
        lifecycle.update_scale(2.0),
        ScaleChange::ScaleResources,
    )?;
    lifecycle.release();
    require(
        lifecycle.take_frame().is_none() && !lifecycle.effects().animations_active,
        "released-renderer",
        "released renderer retained work",
    )
}

fn check_typed_failures() -> Result<(), String> {
    let failure = TerminalFailure::pty("read-shell-output");
    require_eq("failure-class", failure.class(), FailureClass::Pty)?;
    require_eq(
        "failure-recoverability",
        failure.recoverability(),
        Recoverability::Fatal,
    )?;
    require(
        !failure.to_string().contains("terminal content"),
        "failure-redaction",
        failure.to_string(),
    )?;
    let mut diagnostics = DiagnosticBundle::default();
    for _ in 0..DiagnosticBundle::MAX_RECORDS + 10 {
        diagnostics.record(&failure);
    }
    require_eq(
        "diagnostic-record-bound",
        diagnostics.record_count(),
        DiagnosticBundle::MAX_RECORDS,
    )?;
    require(
        diagnostics.encoded_len() <= DiagnosticBundle::MAX_BYTES,
        "diagnostic-byte-bound",
        diagnostics.encoded_len(),
    )
}

const EXECUTABLE_FIXTURES: &[ExecutableFixture] = &[
    ExecutableFixture {
        spec: &FIXTURES[12],
        expected: KEYBOARD_PROTOCOL_EXPECTED,
        observe: observe_keyboard_protocols,
    },
    ExecutableFixture {
        spec: &FIXTURES[4],
        expected: SEMANTIC_SNAPSHOT_EXPECTED,
        observe: observe_semantic_snapshot,
    },
    ExecutableFixture {
        spec: &FIXTURES[35],
        expected: KITTY_GRAPHICS_EXPECTED,
        observe: observe_kitty_graphics,
    },
];

#[test]
fn registry_covers_every_advertised_capability() {
    let stories = FIXTURES
        .iter()
        .flat_map(|fixture| fixture.stories.iter().copied())
        .collect::<BTreeSet<_>>();
    let expected = (1..=46).collect::<BTreeSet<_>>();
    let ids = FIXTURES
        .iter()
        .map(|fixture| fixture.id)
        .collect::<BTreeSet<_>>();
    let forbidden = ["sixel", "iterm-image"];

    assert_eq!(stories, expected);
    assert_eq!(
        ids.len(),
        FIXTURES.len(),
        "fixture identifiers must be unique"
    );
    for fixture in FIXTURES {
        assert!(((6..42).contains(&fixture.issue) && fixture.issue != 28) || fixture.issue == 89);
        assert!(!fixture.authority.is_empty());
        assert!(
            forbidden
                .iter()
                .all(|token| !fixture.id.contains(token) && !fixture.authority.contains(token))
        );
        assert!((1..=MAX_FIXTURE_STEPS).contains(&fixture.max_steps));
        assert!((1..=MAX_FIXTURE_INPUT_BYTES).contains(&fixture.max_input_bytes));
        assert!((1..=MAX_FIXTURE_OUTPUT_BYTES).contains(&fixture.max_output_bytes));
        assert!(matches!(
            fixture.oracle,
            OracleKind::Bytes
                | OracleKind::SemanticSnapshot
                | OracleKind::Geometry
                | OracleKind::Lifecycle
                | OracleKind::Security
                | OracleKind::Native
        ));
    }
}

#[test]
fn runner_identifies_the_exact_fixture_step_and_oracle_mismatch() {
    fn observe(budget: &mut FixtureBudget) -> Result<Vec<Observation>, String> {
        budget.record(3, 4);
        Ok(vec![Observation {
            step: 2,
            field: "pty-bytes",
            value: "1b 5b 4f 41".to_owned(),
        }])
    }

    let fixture = ExecutableFixture {
        spec: &FIXTURES[12],
        expected: &[ExpectedObservation {
            step: 2,
            field: "pty-bytes",
            value: "1b 5b 41",
        }],
        observe: |_, budget| observe(budget),
    };

    assert_eq!(
        run_fixture(&fixture),
        Err(
            "fixture `keyboard.protocols` step 2 `pty-bytes` mismatch:\nexpected: 1b 5b 41\nobserved: 1b 5b 4f 41"
                .to_owned()
        )
    );
}

#[test]
fn runner_rejects_a_fixture_that_exceeds_its_deterministic_budget() {
    fn observe(budget: &mut FixtureBudget) -> Result<Vec<Observation>, String> {
        for _ in 0..=FIXTURES[0].max_steps {
            budget.record(0, 0);
        }
        Ok(Vec::new())
    }

    let fixture = ExecutableFixture {
        spec: &FIXTURES[0],
        expected: &[],
        observe: |_, budget| observe(budget),
    };

    assert_eq!(
        run_fixture(&fixture),
        Err("fixture `snapshot.damage-and-isolation` exceeded steps budget: 257 > 256".to_owned())
    );
}

#[test]
fn golden_byte_fixtures_match_protocol_authorities() {
    for fixture in EXECUTABLE_FIXTURES {
        if fixture.spec.oracle == OracleKind::Bytes
            && let Err(error) = run_fixture(fixture)
        {
            panic!("{error}");
        }
    }
}

#[test]
fn semantic_snapshot_fixtures_match_terminal_state() {
    for fixture in EXECUTABLE_FIXTURES {
        if fixture.spec.oracle == OracleKind::SemanticSnapshot
            && let Err(error) = run_fixture(fixture)
        {
            panic!("{error}");
        }
    }
}

#[test]
fn every_advertised_capability_has_a_passing_executable_fixture() {
    for spec in FIXTURES {
        let fixture = ExecutableFixture {
            spec,
            expected: PASS_EXPECTED,
            observe: observe_advertised_capability,
        };
        if let Err(error) = run_fixture(&fixture) {
            panic!("{error}");
        }
    }
}
