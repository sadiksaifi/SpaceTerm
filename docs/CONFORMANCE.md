# Conventional Terminal Conformance

This document is canonical for the conventional terminal capability corpus. Product and domain
decisions remain canonical in `CONTEXT.md` and `docs/UBIQUITOUS_LANGUAGE.md`.

## Contract

The corpus covers US-01 through US-46 with deterministic, offline fixtures. Every fixture executes
a real SpaceTerm mechanism and declares:

- a stable fixture identifier;
- the prerequisite GitHub issue and covered user stories;
- one authority key from the catalog below;
- an oracle class: bytes, semantic snapshot, geometry, lifecycle, security, or native behavior;
- bounded step, input-byte, and output-byte budgets.

The registry rejects missing story coverage, duplicate fixture identifiers, unbounded fixtures,
and unknown prerequisite issues. Static Kitty graphics fixtures are admitted only under the
dedicated protocol authority; unsupported image protocols remain excluded. The executable gate runs every
registry entry. Golden byte fixtures compare encoded terminal input exactly. Semantic snapshot
goldens retain row order, cell column, cell width, style sources, Presentation Generation, Cursor,
active screen, grid size, title, and Terminal Metadata. A mismatch identifies the fixture, step,
field, expected value, and observed value.

The suite uses no network, login shell, pasteboard, notification center, accessibility server, or
user configuration. AppKit and PTY behavior that would otherwise mutate external state is tested
through test-only deterministic adapters around the same reducers used by production. `just
conformance` is the focused loop. `just test` includes the same tests, so `just validate` is the
required release gate.

## Authority policy

Published standards and platform documentation are authoritative. Ghostty is an audited
behavioral reference at the fixed revision
[`a887df42c56f6de86c0fe6da9c4eeca37931e083`](https://github.com/ghostty-org/ghostty/tree/a887df42c56f6de86c0fe6da9c4eeca37931e083),
not a source of protocol truth. Updating that revision requires a deliberate audit and this
document changing in the same commit.

Static direct-media Kitty graphics is included under the published Kitty protocol. Sixel, iTerm
inline images, Kitty host-file media, and Kitty animation remain excluded; the security fixtures
verify that unsupported media cannot read host files or produce a false capability response.

## Authority catalog

| Key | Authoritative source |
| --- | --- |
| `spaceterm-snapshot-contract` | [SpaceTerm immutable snapshot decision](../CONTEXT.md#immutable-terminal-presentation-snapshots) |
| `ecma-48-and-xterm-window-ops` | [ECMA-48](https://ecma-international.org/publications-and-standards/standards/ecma-48/) and [XTerm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `posix-and-darwin-pty` | [POSIX terminal interface](https://pubs.opengroup.org/onlinepubs/9799919799/basedefs/termios.h.html), [Apple `openpty`](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/openpty.3.html), and [SpaceTerm capability and compatibility identity](../CONTEXT.md#terminal-capability-and-compatibility-identity) |
| `posix-process-lifecycle` | [POSIX `wait`](https://pubs.opengroup.org/onlinepubs/9799919799/functions/wait.html) and [POSIX `kill`](https://pubs.opengroup.org/onlinepubs/9799919799/functions/kill.html) |
| `ecma-48-sgr` | [ECMA-48 Select Graphic Rendition](https://ecma-international.org/publications-and-standards/standards/ecma-48/) |
| `ecma-48-and-xterm-sgr` | [ECMA-48](https://ecma-international.org/publications-and-standards/standards/ecma-48/) and [XTerm SGR extensions](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `unicode-uax-11-uax-29` | [Unicode UAX #11](https://www.unicode.org/reports/tr11/) and [Unicode UAX #29](https://www.unicode.org/reports/tr29/) |
| `unicode-blocks` | [Unicode block elements](https://www.unicode.org/charts/PDF/U2500.pdf) |
| `dec-deccusr` | [XTerm DECSCUSR](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `apple-responder-and-spaceterm-focus` | [Apple responder chain](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/EventOverview/EventArchitecture/EventArchitecture.html) and [SpaceTerm Terminal Input Focus](UBIQUITOUS_LANGUAGE.md#terminal-input-focus) |
| `w3c-code-and-ghostty-key` | [W3C UI Events KeyboardEvent code values](https://www.w3.org/TR/uievents-code/) |
| `kitty-keyboard-fixterms-xterm` | [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) and [XTerm modifyOtherKeys](https://invisible-island.net/xterm/modified-keys.html) |
| `apple-nsevent` | [Apple `NSEvent`](https://developer.apple.com/documentation/appkit/nsevent) |
| `xterm-focus-event` | [XTerm focus event mode](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `dec-deccusr-and-spaceterm-cadence` | [XTerm DECSCUSR](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) and [SpaceTerm negotiated Cursor decision](../CONTEXT.md#negotiated-cursor-presentation) |
| `apple-nstextinputclient` | [Apple `NSTextInputClient`](https://developer.apple.com/documentation/appkit/nstextinputclient) |
| `apple-secure-event-input` | [Apple Secure Event Input](https://developer.apple.com/library/archive/technotes/tn2150/_index.html) |
| `ecma-48-and-xterm-private-modes` | [ECMA-48](https://ecma-international.org/publications-and-standards/standards/ecma-48/) and [XTerm private modes](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `xterm-mouse-tracking` | [XTerm mouse tracking](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `spaceterm-selection-contract` | [SpaceTerm transactional presentation decision](../CONTEXT.md#transactional-terminal-presentation) |
| `apple-scroll-phases-and-xterm` | [Apple scroll wheel events](https://developer.apple.com/documentation/appkit/nsevent) and [XTerm mouse tracking](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `vte-osc-8` | [VTE OSC 8 hyperlinks](https://gnome.pages.gitlab.gnome.org/vte/gtk4/hyperlinks.html) |
| `apple-pasteboard` | [Apple pasteboard programming guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/PasteboardGuide106/Articles/pbConcepts.html) |
| `xterm-bracketed-paste` | [XTerm bracketed paste mode](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `apple-file-url-and-posix-shell` | [Apple file URLs](https://developer.apple.com/documentation/foundation/url) and [POSIX shell command language](https://pubs.opengroup.org/onlinepubs/9799919799/utilities/V3_chap02.html) |
| `xterm-osc-52` | [XTerm OSC 52](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `osc-7-and-finalterm-osc-133` | [OSC 7 working directory](https://gitlab.freedesktop.org/terminal-wg/specifications/-/blob/master/proposals/working-directory-uri.md) and [FinalTerm OSC 133 semantics](https://iterm2.com/documentation-shell-integration.html) |
| `shell-startup-contracts` | [Zsh startup files](https://zsh.sourceforge.io/Doc/Release/Files.html), [Bash startup files](https://www.gnu.org/software/bash/manual/html_node/Bash-Startup-Files.html), and [XDG base directories](https://specifications.freedesktop.org/basedir-spec/latest/) |
| `ncurses-terminfo-and-xterm` | [ncurses terminfo](https://invisible-island.net/ncurses/man/terminfo.5.html) and [XTerm XTGETTCAP](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) |
| `ecma-48-bel-and-apple-notifications` | [ECMA-48 BEL](https://ecma-international.org/publications-and-standards/standards/ecma-48/) and [Apple user notifications](https://developer.apple.com/documentation/usernotifications) |
| `apple-services-drag-and-quick-look` | [Apple Services](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/SysServices/introduction.html), [drag and drop](https://developer.apple.com/documentation/appkit/drag_and_drop), and [Quick Look](https://developer.apple.com/documentation/quicklook) |
| `apple-nsaccessibility` | [Apple accessibility protocol](https://developer.apple.com/documentation/appkit/nsaccessibilityprotocol) |
| `apple-window-visibility` | [Apple window occlusion state](https://developer.apple.com/documentation/appkit/nswindow/occlusionstate) |
| `spaceterm-failure-contract` | [SpaceTerm error-handling principle](../CONTEXT.md#error-handling) |
| `kitty-graphics-protocol` | [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) and [SpaceTerm static graphics decision](../CONTEXT.md#static-kitty-graphics) |

## Fixture matrix

| Fixture | Issue | User stories | Authority | Oracle |
| --- | ---: | --- | --- | --- |
| `snapshot.damage-and-isolation` | #6 | US-45, US-46 | `spaceterm-snapshot-contract` | Semantic snapshot |
| `geometry.logical-and-backing` | #7 | US-34, US-44 | `ecma-48-and-xterm-window-ops` | Geometry |
| `pty.initialization` | #8 | US-38, US-46 | `posix-and-darwin-pty` | Lifecycle |
| `pty.shutdown` | #9 | US-39, US-46 | `posix-process-lifecycle` | Lifecycle |
| `presentation.colors` | #10 | US-03 | `ecma-48-sgr` | Semantic snapshot |
| `presentation.text-attributes` | #11 | US-04 | `ecma-48-sgr` | Semantic snapshot |
| `presentation.decorations` | #12 | US-04 | `ecma-48-and-xterm-sgr` | Semantic snapshot |
| `unicode.graphemes` | #13 | US-02 | `unicode-uax-11-uax-29` | Semantic snapshot |
| `unicode.drawing-symbols` | #14 | US-02 | `unicode-blocks` | Geometry |
| `cursor.negotiated-shape` | #15 | US-05, US-07 | `dec-deccusr` | Semantic snapshot |
| `focus.terminal-input-focus` | #16 | US-08–US-13, US-46 | `apple-responder-and-spaceterm-focus` | Native |
| `keyboard.vocabulary` | #17 | US-01, US-17, US-18 | `w3c-code-and-ghostty-key` | Bytes |
| `keyboard.protocols` | #18 | US-18, US-19 | `kitty-keyboard-fixterms-xterm` | Bytes |
| `keyboard.macos-bridge` | #19 | US-20, US-21 | `apple-nsevent` | Native |
| `focus.dec-1004` | #20 | US-14–US-16 | `xterm-focus-event` | Bytes |
| `cursor.blink-lifecycle` | #21 | US-06, US-07 | `dec-deccusr-and-spaceterm-cadence` | Lifecycle |
| `ime.marked-text` | #22 | US-22 | `apple-nstextinputclient` | Native |
| `input.secure-event` | #23 | US-23 | `apple-secure-event-input` | Security |
| `screen.scrollback-and-reflow` | #24 | US-33, US-34 | `ecma-48-and-xterm-private-modes` | Semantic snapshot |
| `mouse.protocols` | #25 | US-24, US-25 | `xterm-mouse-tracking` | Bytes |
| `selection.semantic-ranges` | #26 | US-26 | `spaceterm-selection-contract` | Semantic snapshot |
| `mouse.precision-wheel` | #27 | US-27 | `apple-scroll-phases-and-xterm` | Bytes |
| `links.osc-8` | #29 | US-28 | `vte-osc-8` | Security |
| `clipboard.selection-copy` | #30 | US-29 | `apple-pasteboard` | Semantic snapshot |
| `paste.unified-safety` | #31 | US-30 | `xterm-bracketed-paste` | Security |
| `paste.file-urls` | #32 | US-31 | `apple-file-url-and-posix-shell` | Security |
| `clipboard.osc-52` | #33 | US-32 | `xterm-osc-52` | Security |
| `metadata.osc-7-and-133` | #34 | US-35 | `osc-7-and-finalterm-osc-133` | Semantic snapshot |
| `shell.temporary-integration` | #35 | US-36 | `shell-startup-contracts` | Lifecycle |
| `identity.terminfo-and-runtime` | #36 | US-37 | `ncurses-terminfo-and-xterm` | Bytes |
| `attention.bell-and-notification` | #37 | US-41 | `ecma-48-bel-and-apple-notifications` | Native |
| `services.native-actions` | #38 | US-43 | `apple-services-drag-and-quick-look` | Native |
| `accessibility.editable-text` | #39 | US-42 | `apple-nsaccessibility` | Native |
| `render.visibility-lifecycle` | #40 | US-12, US-13, US-44, US-45 | `apple-window-visibility` | Lifecycle |
| `failure.typed-local-diagnostics` | #41 | US-40 | `spaceterm-failure-contract` | Security |
| `graphics.kitty-static` | #89 | US-02, US-34, US-44–US-46 | `kitty-graphics-protocol` | Semantic snapshot |

## Maintenance

When an advertised capability changes, update its executable driver, golden observation when
applicable, registry mapping, and this matrix together. Add the narrowest failing fixture first,
then implement the behavior. Do not weaken expected values to accept multiple protocol outputs.
If a published specification and the audited Ghostty reference differ, preserve the specification
behavior and document the reference difference here.
