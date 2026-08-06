use std::collections::BTreeSet;

use super::emulator::TerminalEmulator;
use super::geometry::{BackingScale, CellGridSize, LogicalCellSize, TerminalGeometry};
use super::key::{InputModifiers, KeyAction, KeyInput, OptionAsAltPolicy, PhysicalKey};

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
    fixture!("snapshot.damage-and-isolation", 6, [45, 46], "spaceterm-snapshot-contract", SemanticSnapshot),
    fixture!("geometry.logical-and-backing", 7, [34, 44], "ecma-48-and-xterm-window-ops", Geometry),
    fixture!("pty.initialization", 8, [38, 46], "posix-and-darwin-pty", Lifecycle),
    fixture!("pty.shutdown", 9, [39, 46], "posix-process-lifecycle", Lifecycle),
    fixture!("presentation.colors", 10, [3], "ecma-48-sgr", SemanticSnapshot),
    fixture!("presentation.text-attributes", 11, [4], "ecma-48-sgr", SemanticSnapshot),
    fixture!("presentation.decorations", 12, [4], "ecma-48-and-xterm-sgr", SemanticSnapshot),
    fixture!("unicode.graphemes", 13, [2], "unicode-uax-11-uax-29", SemanticSnapshot),
    fixture!("unicode.drawing-symbols", 14, [2], "unicode-blocks", Geometry),
    fixture!("cursor.negotiated-shape", 15, [5, 7], "dec-deccusr", SemanticSnapshot),
    fixture!("focus.terminal-input-focus", 16, [8, 9, 10, 11, 12, 13, 46], "apple-responder-and-spaceterm-focus", Native),
    fixture!("keyboard.vocabulary", 17, [1, 17, 18], "w3c-code-and-ghostty-key", Bytes),
    fixture!("keyboard.protocols", 18, [18, 19], "kitty-keyboard-fixterms-xterm", Bytes),
    fixture!("keyboard.macos-bridge", 19, [20, 21], "apple-nsevent", Native),
    fixture!("focus.dec-1004", 20, [14, 15, 16], "xterm-focus-event", Bytes),
    fixture!("cursor.blink-lifecycle", 21, [6, 7], "dec-deccusr-and-spaceterm-cadence", Lifecycle),
    fixture!("ime.marked-text", 22, [22], "apple-nstextinputclient", Native),
    fixture!("input.secure-event", 23, [23], "apple-secure-event-input", Security),
    fixture!("screen.scrollback-and-reflow", 24, [33, 34], "ecma-48-and-xterm-private-modes", SemanticSnapshot),
    fixture!("mouse.protocols", 25, [24, 25], "xterm-mouse-tracking", Bytes),
    fixture!("selection.semantic-ranges", 26, [26], "spaceterm-selection-contract", SemanticSnapshot),
    fixture!("mouse.precision-wheel", 27, [27], "apple-scroll-phases-and-xterm", Bytes),
    fixture!("links.osc-8", 29, [28], "vte-osc-8", Security),
    fixture!("clipboard.selection-copy", 30, [29], "apple-pasteboard", SemanticSnapshot),
    fixture!("paste.unified-safety", 31, [30], "xterm-bracketed-paste", Security),
    fixture!("paste.file-urls", 32, [31], "apple-file-url-and-posix-shell", Security),
    fixture!("clipboard.osc-52", 33, [32], "xterm-osc-52", Security),
    fixture!("metadata.osc-7-and-133", 34, [35], "osc-7-and-finalterm-osc-133", SemanticSnapshot),
    fixture!("shell.temporary-integration", 35, [36], "shell-startup-contracts", Lifecycle),
    fixture!("identity.terminfo-and-runtime", 36, [37], "ncurses-terminfo-and-xterm", Bytes),
    fixture!("attention.bell-and-notification", 37, [41], "ecma-48-bel-and-apple-notifications", Native),
    fixture!("services.native-actions", 38, [43], "apple-services-drag-and-quick-look", Native),
    fixture!("accessibility.editable-text", 39, [42], "apple-nsaccessibility", Native),
    fixture!("render.visibility-lifecycle", 40, [12, 13, 44, 45], "apple-window-visibility", Lifecycle),
    fixture!("failure.typed-local-diagnostics", 41, [40], "spaceterm-failure-contract", Security),
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
    observe: fn(&mut FixtureBudget) -> Result<Vec<Observation>, String>,
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
    let observed = (fixture.observe)(&mut budget)
        .map_err(|error| format!("fixture `{}` failed: {error}", fixture.spec.id))?;
    budget.verify(fixture.spec)?;

    for (expected, observed) in fixture.expected.iter().zip(&observed) {
        if expected.step != observed.step
            || expected.field != observed.field
            || expected.value != observed.value
        {
            return Err(format!(
                "fixture `{}` step {} `{}` mismatch:\nexpected: {}\nobserved: {}",
                fixture.spec.id,
                expected.step,
                expected.field,
                expected.value,
                observed.value
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

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

const EXECUTABLE_FIXTURES: &[ExecutableFixture] = &[ExecutableFixture {
    spec: &FIXTURES[12],
    expected: KEYBOARD_PROTOCOL_EXPECTED,
    observe: observe_keyboard_protocols,
}];

#[test]
fn registry_covers_every_advertised_capability_without_image_protocols() {
    let stories = FIXTURES
        .iter()
        .flat_map(|fixture| fixture.stories.iter().copied())
        .collect::<BTreeSet<_>>();
    let expected = (1..=46).collect::<BTreeSet<_>>();
    let ids = FIXTURES
        .iter()
        .map(|fixture| fixture.id)
        .collect::<BTreeSet<_>>();
    let forbidden = ["image", "sixel", "kitty-graphics", "iterm-image"];

    assert_eq!(stories, expected);
    assert_eq!(ids.len(), FIXTURES.len(), "fixture identifiers must be unique");
    for fixture in FIXTURES {
        assert!((6..42).contains(&fixture.issue) && fixture.issue != 28);
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
        observe,
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
        observe,
    };

    assert_eq!(
        run_fixture(&fixture),
        Err(
            "fixture `snapshot.damage-and-isolation` exceeded steps budget: 257 > 256"
                .to_owned()
        )
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
