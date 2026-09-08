# Scala scanner snapshot framing and layout positions

Baseline: published `tree-sitter-scala` 0.26.2 archive, SHA-256
`24e0ab4505990bfe30051761d40a7bf4033ce5a81c9eda9e20e987a5cdc84826`.
The archive's VCS metadata names revision
`b931fcc338390925eb893d70ad070033f5856ccf` with a dirty flag. The grammar,
generated parser/grammar/node types, original scanner, highlight/local/tag queries and
license are byte-identical to that upstream revision. With tree-sitter CLI
0.26.8, generation is byte-identical when the staging metadata uses 0.26.0,
the version embedded in the published parser. Using the package's metadata
version 0.26.2 changes only the generated language patch-version field.

The upstream scanner at
<https://github.com/tree-sitter/tree-sitter-scala/blob/b931fcc338390925eb893d70ad070033f5856ccf/src/scanner.c>
reads a five-word header without checking nonzero frame lengths first and
silently ignores a partial trailing indent word. Its signed sixteen-bit
layout fields also lose valid declaration ownership at column 32768.

The local snapshot format has a version byte, explicit presence/case flags
and little-endian uint32 positions. In-memory signed values preserve the
full lexer column domain plus the absent sentinel; the case flag is outside
that domain, and saved lookahead retains the complete Unicode scalar.
Newlines saturate at two because layout distinguishes only no newline, one
newline and a blank-line separation. Decoding validates framing, flags and
field domains before allocation or mutation; rejected snapshots leave reset
state. No aligned word loads are used. Snapshots are process-local, not a
persisted cross-version format.

The unchanged 1024-byte ABI holds at most 201 complete layout frames. Both
indent push paths refuse excess frames before publishing a token. Exhaustion
is an explicit parse error, not successful parsing with a truncated stack.
Native regressions cover all encodable lengths, unaligned input, invalid
framing/fields, full uint32 values, Unicode, wide indentation, long blank-line
runs, exact declaration owners, stack exhaustion and incremental/fresh tree
equivalence. The upstream syntax, highlighting and tag tests are unchanged.

Generated parser and grammar files are unchanged. Remove this local patch
when the pinned upstream scanner provides equivalent framing, position and
capacity guarantees and passes the native regressions. This package is under
isolated native qualification, not production registration or full Scala
language support.
