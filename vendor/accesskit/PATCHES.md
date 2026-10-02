# AccessKit caret geometry

Source: crates.io `accesskit` 0.24.1, upstream revision
`a55d3e1a18bb9ef0e4bccc9083fb13c3e0ad8969`, directory `common`.
The original crate archive SHA-256 is
`d3b7f7f85a7e5f68090000ed7622545829afd484d210358702ae4cb97dd0c320`.
The license files are from that exact upstream revision.
Registry bookkeeping and the standalone lockfile are omitted.

The optional `Node::text_caret_bounds` rectangle describes the geometry at the
focus of a text selection in its owner's coordinate space. It allows terminal
carets to move beyond retained text without padding text or changing character
geometry. All existing properties retain their identifiers. Unset behavior is
unchanged. Getter, setter, clear, debug, serde and schema paths are covered.

The optional `Node::word_starts_u32` list exposes complete word boundaries in
text runs longer than 256 AccessKit characters. It uses the character indices
defined by `character_lengths`, including extra chunks of oversized graphemes.
The existing `word_starts` API remains intact. The optional wide list takes
precedence when set, including an explicitly empty list; clearing it restores
the existing property. Its typed getter, setter, clear, debug, serde and schema
paths are covered. Its identifier is appended without changing existing ones.

Retained README line endings and original manifest trailing blank lines are normalized for the repository whitespace gate; source behavior is unchanged.
