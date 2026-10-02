# AccessKit AT-SPI text changes

Source: crates.io `accesskit_atspi_common` 0.19.1, upstream revision
`c88605b96d04431f9c3c792464a0f2f253480e94`, directory `platforms/atspi-common`.
The original crate archive SHA-256 is
`023da0e5097f46df7092d5280b02efb9bbf8d93298daeced42652463e357d636`.
The source and API are retained. The license files are from that exact upstream revision.
Registry bookkeeping and the crate's standalone lockfile are omitted.

The patch aligns unchanged TextRuns by node identity and content before calculating
text changes. Unique values also anchor content copied through stationary row
nodes. Unicode scalar offsets and complete document text are preserved.
Monotonic anchor selection maximizes retained Unicode scalar text using Fenwick
prefix maxima, so stationary blank rows do not outweigh surviving content rows.
Scrollback trimming and alternate-screen scrolling therefore report removed rows
and newly inserted output without announcing unchanged surviving rows again.

Regression tests exercise the public Adapter callback and Text interface, replaying
emitted changes to reconstruct the complete new document. They cover trimming,
Unicode, nested runs, simultaneous edits, node replacement and reordering.

The manifest consumes the adjacent retained AccessKit and consumer crates.
The AT-SPI adapter emits caret movement when explicit caret geometry changes
at a fixed text offset, without reporting a text selection change. Hidden and
unfocused updates remain silent; raw selection changes retain their existing
events. Public Adapter callback regressions cover these cases.

Resolved global caret offsets are compared independently of raw run positions
and geometry. Trimming or editing earlier text therefore announces the changed
offset for a retained focused row. Earlier-run updates also check their text
parent, with notifications deduplicated per parent and tree update. Offset-only
changes preserve selection events and respect visibility and focus.

Degenerate AT-SPI range queries at the current caret's Unicode scalar offset
preserve the selection focus's original run bias when explicit caret bounds
are present. This keeps caret geometry reachable at boundaries between runs.
Other offsets, non-degenerate ranges and absent explicit bounds retain the
original conversion. The public Text-interface regression covers ASCII and
Unicode run boundaries and all these fallback cases.
