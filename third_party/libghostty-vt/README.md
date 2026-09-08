# libghostty-vt

SpaceTerm-maintained safe Rust wrappers over the local `libghostty-vt-sys` integration.
This unpublished workspace dependency originated in
[libghostty-rs](https://github.com/Uzaaft/libghostty-rs); the original license is retained.
See [the integration guide](../../docs/ghostty-integration.md) for the upgrade workflow.

Handle types (`Terminal`, `RenderState`, `KeyEncoder`, etc.) are `!Send + !Sync` by design. Callers should drive all operations from a single thread.
