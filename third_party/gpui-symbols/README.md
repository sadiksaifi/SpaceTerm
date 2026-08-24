# gpui-symbols

macOS SF Symbols rendering for GPUI applications.

Vendored from crates.io `gpui-symbols` 0.6.1 and substituted through the workspace
`[patch.crates-io]` table. The upstream README, examples, and licenses are preserved verbatim; only
the files named in the patch ledger below differ from the published crate, and
`patches/spaceterm-monochrome-rendering.patch` reproduces that difference from a pristine 0.6.1
checkout.

The crate is not a workspace member, so `just check` and `just test` build it as a dependency but do
not run its own tests. Run the patch's regression test explicitly:

```
cargo test --package gpui-symbols -- --ignored monochrome_should_not_dim
```

## SpaceTerm patch ledger

- `spaceterm-monochrome-rendering.patch` makes `RenderingMode::Monochrome` actually render
  monochrome. Upstream built the configuration as
  `preferringMonochrome.configurationByApplyingConfiguration(hierarchicalColor:)`, but
  `configurationByApplyingConfiguration:` gives precedence to the applied configuration and
  `configurationWithHierarchicalColor:` selects a rendering mode as well as a color. The applied
  tint therefore discarded `preferringMonochrome`, and `RenderingMode::Monochrome` produced
  bitmaps byte-identical to `RenderingMode::Hierarchical`: multi-layer symbols painted their
  secondary layer at reduced alpha whatever mode the caller asked for. Single-layer symbols such
  as `magnifyingglass` have no secondary layer and were unaffected, so the defect surfaced only as
  badged symbols (`folder.badge.plus`, `exclamationmark.triangle.fill`,
  `plus.rectangle.on.folder`) rendering two-tone beside their neighbors. Reversing the two
  configurations keeps the tint color and preserves the monochrome rendering mode.

  The patch also adds `monochrome_should_not_dim_the_secondary_layer_of_a_badged_symbol` to
  `src/symbol.rs`, which compares the two rendering modes for a badged symbol and uses a
  single-layer symbol as a control. It fails against unpatched 0.6.1 with a largest per-channel
  difference of 0.

  This should go upstream; drop the vendored crate once a released version carries the fix.
