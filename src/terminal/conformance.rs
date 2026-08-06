use std::collections::BTreeSet;

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
