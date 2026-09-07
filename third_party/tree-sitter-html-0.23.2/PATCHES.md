# HTML source grammar qualification

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

HTML title, textarea, xmp, iframe, noembed and noframes bodies retain literal
markup until a complete matching end tag. Plaintext consumes the remainder of
the source, including apparent closing tags. These bodies remain source `text`
nodes without DOM character-reference decoding or newline normalization. A start
tag slash does not disable their text mode. Raw scanning uses the lexer EOF
callback, so embedded NUL bytes cannot expose subsequent literal markup as tags.

New text modes are selected only outside native SVG/MathML scopes. Namespace
integration and scripting-dependent noscript behavior are not inferred by this
scanner; the Rootlight adapter reports those source scopes as coverage gaps.
Only text-mode candidates inspect the ancestor stack. No serialized fields or
global resource limits are added.

The grammar, parser, node types and generated headers reproduce with the pinned
Tree-sitter CLI 0.25.10 and ABI 14. These fixes qualify source syntax and scanner
state, not browser DOM construction, embedded-language analysis or full HTML/MCP
coverage. Names exceeding the native buffer remain explicit parse errors.

Regression tests are in `tests/native-grammars`. Rust build and Tree-sitter CLI
metadata both track the additional scanner headers.
