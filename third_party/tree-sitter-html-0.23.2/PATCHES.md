# HTML scanner qualification

The baseline is `tree-sitter-html` 0.23.2, upstream commit
`5a5ca8551a179998360b4a4ca2c0f366a35acc03`. The published archive SHA-256 is
`261b708e5d92061ede329babaaa427b819329a9d427a1d710abb0f67bbef63ee`.
The archive omits LICENSE; its unmodified text is restored from that commit.

The scanner stores complete tag identities within Tree-sitter's existing
1024-byte serialization buffer. Versioned, byte-encoded snapshots have a
three-byte header, one-byte built-in tags and length-prefixed UTF-8 custom names.
Neither names nor stack entries are truncated or replaced with anonymous tags.
Oversized start-tag state is rejected before mutation. Deserialization validates
the complete bounded frame before allocation and clears malformed state.

Tag scanning uses ASCII case folding and preserves other Unicode code points
as UTF-8, independently of the C locale. Raw script/style text recognizes an
end-tag name only at its delimiter; overlapping less-than prefixes are retained.
Implicit custom-tag matching compares names, not just the CUSTOM enum value.
Comment delimiter counting saturates after two dashes.

The generated grammar, parser and node types are unchanged. These fixes qualify
native syntax and state handling; they are not browser DOM construction, embedded
language analysis or Rootlight structural/MCP integration. Names requiring more
than the native state buffer remain explicit parse errors, not complete coverage.

Regression tests are in `tests/native-grammars`. Rust build and Tree-sitter CLI
metadata both track the additional scanner headers.
