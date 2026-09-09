# Astro native source qualification

The baseline is the published `tree-sitter-astro-next` 0.1.1 archive at commit
`15a3b95bf444b68b698dfb2ae6921d33e464b9f8`. Its SHA-256 is
`794a4a59fc2d88e49b4bc41fef9522d77184a36f4e68bbaf545cd1eb2364c46e`.
The generated parser is unchanged; its SHA-256 is
`7935187a9e7a62ffc97c1a3e8476064cf0579354d7f0204405cba1ba9e55387e`.
The published archive and pinned fork do not contain the grammar generator input;
this qualification does not claim regeneration of the C parser from a grammar DSL.

The fork's Apache-2.0 license is retained in `LICENSE`. The original grammar's
MIT notice is restored as `LICENSE-MIT` from
`virchau13/tree-sitter-astro` commit
`947e93089e60c66e681eba22283f4037841451e7`; its SHA-256 is
`9ec95a90150fa19e2cee78faf49138fc480f6f7a2354006a0ab150c44239447e`.
The local manifest uses the combined SPDX expression `Apache-2.0 AND MIT`
instead of the published non-SPDX `MIT or Apache-2.0`. Both notices must accompany
distribution. The local workspace declaration permits standalone upstream tests.

## Scanner changes

- Complete case-sensitive UTF-8 tag identities replace locale-dependent scalar
  truncation. Overlong names and unrepresentable state fail token admission.
- Version 2 snapshots retain the entire tag stack and each interpolation's compact
  lexical context within Tree-sitter's unchanged 1024-byte serialization buffer.
  Validation precedes allocation and restoration; malformed or old private frames
  clear all state. Tag, fragment and interpolation admission accounts for payloads.
- Nested JavaScript braces resume their parent's resulting lexical context.
  Nested HTML components preserve the outer context and resume as values, so
  following division is not treated as a new regex literal.
- Attribute, frontmatter, template and interpolation scanning distinguish tested
  operand, property, control-parenthesis and block contexts. Regex classes and
  escapes cannot expose their braces or comment markers as host delimiters.
  JavaScript whitespace and line terminators are independent of the C locale.
- Template scanning is iterative. Its explicit frame ceiling matches the existing
  runtime hard syntax-depth ceiling; overflow and unterminated strings fail tokens
  instead of publishing prefixes. This does not raise runtime resource allowances.
- Script/style closing-tag prefixes require a complete tag-name delimiter.
  Escaped backticks and dollars retain their original source boundaries.
- The build script tracks every added scanner header and `tag.h`.

## Verification scope

The `astro_*` tests in `tests/native-grammars` cover exact source ranges, native
snapshot boundaries, malformed lexical payloads, Unicode identity, nested templates
on a small thread stack, incremental/fresh full-node equivalence and upstream
grammar/query behavior. Shared expressions include regex/division, spread, control
blocks and Unicode separators. These are syntax and scanner-state tests, not a
complete ECMAScript conformance suite or proof of production language support.

Rootlight registry admission, source-mapped host/embedded analysis, discovery,
durable index behavior and MCP semantics require separate integration and tests.
This vendor import alone does not advertise an Astro capability. Runtime errors
and resource-limited source must remain explicit coverage gaps, never silent loss.
