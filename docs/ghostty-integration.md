# Ghostty integration

SpaceTerm maintains the unpublished Rust bindings and wrappers in `third_party/libghostty-vt-sys`
and `third_party/libghostty-vt`, derived from [libghostty-rs](https://github.com/Uzaaft/libghostty-rs).
Their licenses and packaged attribution remain intact.

The engine build uses the committed `third_party/ghostty` gitlink and applies SpaceTerm patches
in Cargo's build output. Working-tree edits inside the submodule are not build inputs; commit a
source change and update the gitlink, or maintain a reviewed patch.

## Updating the engine

1. Select an official Ghostty commit and align the Zig pin in [`.mise.toml`](../.mise.toml) with
   its `build.zig.zon` requirement.
2. Rebase [SpaceTerm's patches](../third_party/libghostty-vt-sys/patches) and remove extensions now
   supplied by upstream. Keep terminal protocol behavior in the engine.
3. Build the engine and regenerate matching bindings with the `engine` tasks in `.mise.toml`.
   Adapt wrappers and callers to the new contracts, including callback lifetimes and image generations.

[`build.rs`](../third_party/libghostty-vt-sys/build.rs) owns source overrides, build options, and
patch preparation. Streamed graphics transport keeps image loading separate from local file
authority; additional transports require an explicit authority design.
