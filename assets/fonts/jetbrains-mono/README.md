# JetBrainsMono Nerd Font

Source: [Nerd Fonts 3.5.1](https://github.com/ryanoasis/nerd-fonts/releases/tag/v3.5.1),
[JetBrainsMono.tar.xz](https://github.com/ryanoasis/nerd-fonts/releases/download/v3.5.1/JetBrainsMono.tar.xz),
based on JetBrains Mono 2.304.

The four faces have private family and PostScript names under `SpaceTerm Default`.
Only name metadata and the font checksum differ from upstream; glyphs, metrics,
and layout tables are unchanged. This prevents the bundled font from replacing an
explicitly selected system font. `OFL.txt` contains the upstream license, also
included in the shipped `assets/THIRD-PARTY-NOTICES.txt`.

Regenerate from the release archive with `mise run fonts:prepare <archive>`.
The task pins the archive SHA-256 and fontTools version and checks that other tables
are unchanged. Add `--check` to compare the output with the committed artifacts.
`SHA256SUMS` records the prepared file hashes. Normal builds use the committed TTFs
and need no font preparation tools or downloads.
