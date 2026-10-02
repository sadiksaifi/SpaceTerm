# Inter

Source: [Inter 4.1](https://github.com/rsms/inter/releases/tag/v4.1),
[Inter-4.1.zip](https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip).
Archive SHA-256: `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e`.

The static Regular, Medium, SemiBold, and Bold OTF faces from `extras/otf/`
provide the Linux Application Chrome font. Their family and face names are
changed to `SpaceTerm UI` to prevent collisions with installed Inter versions.
Only name metadata and the font checksum differ from upstream; glyphs, metrics,
and layout tables are unchanged.

`OFL.txt` is the upstream archive's `LICENSE.txt`, copied verbatim. The full
license is also included in `assets/THIRD-PARTY-NOTICES.txt`.

Regenerate with `mise run fonts:ui:prepare <archive>`. The task pins the archive
SHA-256 and fontTools version and verifies every other table is unchanged.
Add `--check` to compare with the committed artifacts. `SHA256SUMS` records the
prepared font and license hashes. Normal builds use these committed files
without downloads or font preparation tools.
