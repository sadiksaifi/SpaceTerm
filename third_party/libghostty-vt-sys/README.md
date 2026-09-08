# libghostty-vt-sys

SpaceTerm-maintained C bindings for the official Ghostty terminal engine. This is an unpublished
workspace dependency, originally derived from [libghostty-rs](https://github.com/Uzaaft/libghostty-rs).
See [the integration guide](../../docs/ghostty-integration.md) for source and binding updates.

- Builds `libghostty-vt.a` from the pinned `../ghostty` submodule using official mise-managed Zig.
  A separate local build copy receives SpaceTerm's patches; the submodule stays clean.
- Exposes checked-in generated bindings in `src/bindings.rs`.
- Static linking is the baseline rather than a Cargo feature. Enable the
  additive `link-dynamic` feature to link the shared library instead.
- Set `GHOSTTY_SOURCE_DIR` to explicitly use a prepared, patched local Ghostty checkout.
- Set `GHOSTTY_ZIG_SYSTEM_DIR` to force Zig package resolution through a
  pre-fetched `zig build --system` directory. This is intended for Nix and other
  sandboxed package managers that cannot fetch during build scripts.
- Set `LIBGHOSTTY_VT_SYS_OPTIMIZE` to `Debug`, `ReleaseSafe`, `ReleaseFast`, or
  `ReleaseSmall` to override the Zig optimize mode used by vendored builds.
- Installed libraries are never discovered through `pkg-config`; builds use the matched source.
- libghostty-vt is pre-1.0, so these bindings do not guarantee compatibility
  with arbitrary installed C API revisions.

## SpaceTerm patch ledger

- `spaceterm-kitty-graphics.patch` exposes the bounded Kitty graphics seams used by SpaceTerm.
- `spaceterm-terminal-effects.patch` adds synchronous, bounded accepted-event effects for OSC 8
  URI resolution and OSC 133 semantic prompts. Progress uses Ghostty's upstream callback.
  Hyperlink input/output memory is
  callback-scoped and copied by libghostty before return; bounded resolver-only hyperlink metadata
  follows the retained page entry without entering formatted terminal content. Absent callbacks
  preserve upstream behavior exactly.
- `spaceterm-accessibility.patch` exposes bounded accessibility snapshots and selection operations.
