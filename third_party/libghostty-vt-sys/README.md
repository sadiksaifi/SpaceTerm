# libghostty-vt-sys

Derived from [libghostty-rs](https://github.com/Uzaaft/libghostty-rs).
[LICENSE](LICENSE) retains its original license.

## Engine maintenance

[`build.rs`](build.rs) owns how the build prepares the Ghostty source and applies [`patches`](patches).
Edits in the submodule working tree do not reach the build.
Commit the source change and update the gitlink, or maintain a patch.

To update the engine, select an official Ghostty commit and align the Zig pin in
[`.mise.toml`](../../.mise.toml) with its `build.zig.zon`.
Rebase the patches and drop extensions that upstream now supplies.
Then rebuild and regenerate bindings with the `engine` tasks.

Keep terminal protocol behavior in the engine.
Streamed graphics transport keeps image loading separate from local file authority.
Additional transports require an explicit authority design.
