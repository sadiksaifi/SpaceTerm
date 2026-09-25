# Native menu startup timing

The user reports File, Edit, View, Window and Help appearing after Workspace
content on 2026-09-25. The screenshot shows the completed menu bar; it does not
establish frame timing. Audit source: `e2b5bbd` on `perf/application-resources`.
An independent GPT-6 Sol high agent checked the same call paths.

## Source findings

`app::start_application` initializes appearance, fonts, controls, the desktop
profile and Services before calling `app::init`. That function registers menu
actions and installs the complete native menu before calling `app::open`.
The macOS adapter calls `cx.set_menus`, which synchronously invokes AppKit's
`setMainMenu:`. Icon decoration is synchronous too.

`app::open` then calls `cx.open_window`. GPUI requests `makeKeyAndOrderFront:`
while constructing the native window. SpaceTerm explicitly requests application
activation through `cx.activate(true)` only after window creation returns.
Thus window presentation is requested before explicit application activation,
although the menu is already installed. This is a plausible explanation for the
reported visible order, not a reproduced diagnosis.

Apple documents that [application activation can be asynchronous](https://developer.apple.com/documentation/appkit/nsapplication/activate%28ignoringotherapps%3A%29?language=objc).
Installing the menu and presenting it on screen are different events. Static call
order cannot establish which frame AppKit and WindowServer present first.

## Experiment proposed after the audit

Capture native source-build launch timing for menu installation, native window
presentation, activation and the first visible menu frame. Then compare requesting
activation before window creation. Do not claim that swapping calls guarantees a
menu-first frame, add artificial delays, or pump a nested event loop.

Earlier menu installation must retain registered actions and the desktop keymap:
GPUI derives native menu shortcuts from that keymap at construction. Changes to
`app::open` also affect reopen and New Workspace, so validate those paths. No
production code was changed for this audit.

Relevant owners: `src/app.rs`, `src/platform/macos_application_menu.rs`,
`third_party/gpui/src/platform/mac/platform.rs`, and
`third_party/gpui/src/platform/mac/window.rs`.

## Approved implementation

The user approved trying the activation reorder. `app::open` now requests
activation immediately before `cx.open_window`, with menus and shortcuts already
installed. It retains one activation request and adds no timer or nested event
loop. The common path also covers Dock reopen and headless New Workspace. On a
window-creation error the activation request has now already occurred; startup
still quits on that error, while restoration keeps its existing error handling.

Validation:

- `mise run test:one app::`: 21 tests passed, including startup, global commands,
  headless New Workspace, restoration and application quit behavior.
- `mise run fmt:check`, `mise run lint:rust`, `mise run build:release`, and
  `git diff --check`: passed.
- `mise run dev:macos`: source build opened on retry. The first invocation hit
  the previously observed artifact-accounting failure, although compilation
  completed. The retry staged and ran SpaceTerm Dev successfully.
- Native UI inspection confirmed the rendered Workspace and shell prompt, all
  six top-level menus, and an operable File menu. The menu was dismissed and the
  source app was left open for the user. This was a debug development build, not
  an optimized launch benchmark.
- Two independent GPT-6 Sol high reviews (`menu_startup_audit`, `pr_review_ui`)
  found no material issue in the reorder and affected call paths.

The native check establishes that the app and menu work after launch. It does
not establish which appeared on the first visible frame. That effect remains
an experiment for direct observation; no menu-first timing gain is claimed.
The earlier application resource measurements predate this activation change.
