# JSON lexical grammar corrections

This directory retains the parser inputs and Rust bindings from the MIT-licensed
`tree-sitter-json` crate version `0.24.8`. Rootlight's production grammar registry
uses these checked inputs for structural JSON indexing.

## Provenance

- Upstream: <https://github.com/tree-sitter/tree-sitter-json>
- Tag: `v0.24.8`
- Commit: `ee35a6ebefcef0c5c416c0d1ccec7370cfca5a24`
- Published crate SHA-256:
  `4d727acca406c0020cffc6cf35516764f36c8e3dc4408e5ebe2cb35a947ec471`
- The published archive omits `LICENSE`; the retained license is copied verbatim
  from the same upstream commit. Registry cache markers and README are omitted.

## Local changes

The grammar applies the number and string lexical rules from
[RFC 8259 sections 6 and 7](https://www.rfc-editor.org/rfc/rfc8259.html):

- Exponents accept either sign, including valid values such as `1e+2`.
- A decimal point requires at least one following digit.
- Unicode escapes consume exactly four hexadecimal digits after `\u`, preserving
  the complete escape as one source-bound syntax node.
- Unescaped control characters U+0000 through U+001F are excluded from string
  content. Closing quotes are immediate tokens so whitespace extras cannot hide
  a raw tab or newline inside a string.

`src/parser.c`, `src/grammar.json` and `src/node-types.json` are generated from
`grammar.js` with Tree-sitter CLI `0.25.10`, using `generate --abi 14`. Generated
files are not hand-edited. The generator also refreshes its C support headers
under `src/tree_sitter/`. There is no external scanner or scanner-state format.

## Scope and verification

`tests/native-grammars/tests/json_syntax.rs` preserves duplicate object members,
nested array/object source ranges, empty and escaped keys, raw Unicode and string
whitespace. Regression tests reproduce four failures on the unmodified published
grammar. Deterministic edits compare every node's kind, byte range, source points,
child count and error/missing flags with a fresh parse, including malformed edits.

The upstream editor extensions remain intentional: comments, empty documents and
multiple top-level values are accepted. An error-free syntax tree is not proof of
strict JSON validity. Four hexadecimal digits describe a UTF-16 code unit; this
grammar does not validate surrogate pairing or decode keys into Unicode scalars.
The production query pack separately captures object members and data-container
scopes; adapter tests verify distinct duplicate keys and array elements. No
complete language semantics, MCP or corpus-wide coverage claim follows from the
isolated native tests alone.
