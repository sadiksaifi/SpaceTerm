# Ghostty integration

SpaceTerm owns its Rust integration with the Ghostty terminal engine. The native engine comes
from the official `third_party/ghostty` submodule. The workspace gitlink pins its exact revision;
`.mise.toml` pins the compatible official Zig compiler. No installed Ghostty library or
Homebrew Zig is required for the normal build.

`third_party/libghostty-vt-sys` contains the C bindings and native build integration.
`third_party/libghostty-vt` contains the safe Rust wrappers. Both are unpublished path dependencies
maintained with SpaceTerm. They originated in [libghostty-rs](https://github.com/Uzaaft/libghostty-rs);
their licenses and attribution remain in the source and packaged notices. The package version
records the original wrapper baseline, not the native Ghostty version.

`libghostty-vt` is the embeddable terminal-state library built by the official Ghostty project.
It shares protocol implementation with the Ghostty application. Its C API does not provide
SpaceTerm's GPUI renderer, Session scheduling, or application policies.

## Source and build ownership

`mise run setup:macos` installs the pinned tools, initializes the submodules, and verifies Apple
tooling. Other platforms use `mise run setup`. The ordinary `mise run dev`, `check`, `test`, and
packaging tasks compile the native dependency as needed through Cargo.

The build script makes a local Git clone of the committed submodule tree inside Cargo's build
output, then applies SpaceTerm's patches there. Source changes and patch contents invalidate the
prepared copy. The submodule remains untouched, and the build does not fetch a replacement
Ghostty revision. Zig may download the dependencies pinned in Ghostty's `build.zig.zon` on a
first build. Native builds use a baseline CPU target suitable for distribution.

Changes made directly in the submodule working tree are not build inputs. Commit an upstream
source change and update the gitlink, or maintain a reviewed patch. A prepared patched checkout
can be selected explicitly with `GHOSTTY_SOURCE_DIR` while developing the integration.
`GHOSTTY_ZIG_SYSTEM_DIR` supplies prefetched Zig dependencies for network-restricted packaging.

## Updating the engine

1. Select an official Ghostty commit and check its `build.zig.zon` compiler requirement. Check out
   that commit in `third_party/ghostty` and update the Zig pin in `.mise.toml` if needed.
   Record the source change in the parent repository's gitlink.
2. Review upstream C headers, terminal behavior, and relevant wrapper improvements. Rebase the
   patches in `third_party/libghostty-vt-sys/patches`; remove extensions replaced by upstream.
   Keep protocol behavior in Ghostty and expose only the additional operations SpaceTerm needs.
3. Compile the patched engine with `mise run deps:build`. During patch development,
   `mise run deps:engine <prepared-source>` compiles a separate prepared source tree.
   Use a separate copy with `--features=-kitty-graphics` to verify that non-graphics extensions
   also compile when upstream graphics support is disabled.
4. Regenerate bindings with `mise run deps:bindings`, using the headers from the matching native
   build. Adapt safe wrappers and callers to changed contracts, especially callback lifetimes,
   sized structures, optional data, and image generations. An explicit `GHOSTTY_INCLUDE_DIR`
   can select prepared headers while resolving a binding bootstrap incompatibility.
5. Keep dependency changes narrow. Refresh the lockfile only if manifests require it. Run
   `mise run deps:fmt`, focused regression tests, and then `mise run validate:macos`,
   `mise run test:conformance`, and `mise run package:macos` before shipping a macOS update.
   Run supported non-macOS validation on the corresponding hosts.

## Regression evidence

Successful compilation does not establish rendering correctness. Exercise images on the source
build using Yazi, Chafa, Pi, and Kitty's `icat` tooling. Include direct images and Unicode
placeholders, multiple Panes, replacement and deletion, animation, relative placements, scrolling,
resizing, clipping, and main/alternate screens. Keep small synthetic protocol regressions in tests.

Terminal metadata effects and accessibility are part of this integration and must keep passing
their existing tests when the engine changes. Resource limits and filesystem authority remain
SpaceTerm policy. A remote-provided path never grants access to a local file.

## Graphics policy

The engine implements Kitty protocol handling; SpaceTerm supplies image decoding, rendering,
animation wakeups, and resource policy. Unicode placeholders are image cells, so the text renderer
must not draw their reserved codepoint or combining marks as visible glyphs.

SpaceTerm enables direct, streamed image transport. File, temporary-file, and shared-memory
transports remain disabled for both local and remote Sessions. Use `kitty +kitten icat
--transfer-mode=stream <image>` when selecting a transport explicitly. Supporting additional
transports requires an explicit local-authority design, not merely enabling an upstream flag.

Graphics admission uses actual retained engine and snapshot bytes across Sessions, with a
384 MiB aggregate residency limit and a separate 128 MiB APC scratch limit. After a Session
receives an APC sequence, its native input feed holds a shared admission lock through decoding
so concurrent Sessions cannot each claim the same capacity. This bounds residency but can
serialize input processing across those Sessions.
RGB-to-RGBA conversion for animation also admits its retained growth against the native limit.
PNG decoding scratch is released when each decode finishes, including failed decodes.
Animation schedules advance visible Panes without requiring new terminal output; hidden Panes
suspend presentation work and resume when shown.
Chunked frame uploads retain the image's identity across playback ticks, while actual image
replacement invalidates the upload.
Snapshots deferred by retained UI pixels retry when capacity is released, without polling or
requiring further terminal output.

PTY pixel dimensions and terminal size-query replies use the same integer cell dimensions as
Ghostty. GPUI rendering and pointer coordinates retain fractional geometry. Keeping the protocol
reports consistent prevents tools that divide the PTY pixel width by its column count from
placing more image cells than the Pane can hold.
