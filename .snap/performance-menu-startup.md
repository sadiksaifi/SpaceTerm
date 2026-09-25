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

## Next experiment

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
