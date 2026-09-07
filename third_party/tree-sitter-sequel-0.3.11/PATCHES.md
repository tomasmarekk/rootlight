# SQL scanner qualification

The baseline is the published `tree-sitter-sequel` 0.3.11 archive, SHA-256
`9d198ad3c319c02e43c21efa1ec796b837afcb96ffaef1a40c1978fbdcec7d17`.
Its VCS metadata names `7b51ecda191d36b92f5a90a8d1bc3faef1c7b8b8` with a dirty
checkout. The published grammar, build script and highlight query match that
commit. The omitted MIT license is restored verbatim from the same commit.
Tree-sitter CLI 0.24.7 with ABI 14 reproduces the published parser, grammar JSON
and node types byte for byte. These generated files are not modified.

The scanner retains complete UTF-8 dollar delimiters in fixed-capacity state.
Snapshots have a version byte, a little-endian two-byte length and the exact
delimiter bytes. Empty state has zero serialized bytes. Only complete tags
that fit the existing 1024-byte native ABI frame can enter persistent state;
oversized tokens fail without mutating prior context. Malformed frames reset
the state instead of retaining a partial tag or an unchecked C string.

Serialization does not consume live context. Deserialization and scanning do
not allocate per delimiter; allocation failure at construction fails closed.
Literal scanning preserves overlapping dollar prefixes, embedded NUL body
bytes and case-sensitive Unicode delimiters. It leaves the enclosing function
delimiter intact when scanning a differently tagged literal.

The grammar's permissive punctuation tags are preserved. This scanner bounds
source tokens, not SQL dialect validity or database name resolution. The native
state and syntax regression suites live in `tests/native-grammars`; SQL text is
never executed by these tests. This package alone is not a structural adapter
or evidence of complete SQL semantic coverage.
