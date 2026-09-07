# TOML native syntax qualification

This directory retains the MIT-licensed `tree-sitter-toml-ng` crate `0.7.0`
for isolated native tests. It is not yet registered in the production adapter.

## Provenance

- Upstream: <https://github.com/tree-sitter-grammars/tree-sitter-toml>
- Commit: `64b56832c2cffe41758f28e05c756a3a98d16f41` (version `0.7.0`).
- Published crate SHA-256:
  `e9adc2c898ae49730e857d75be403da3f92bb81d8e37a2f918a08dd10de5ebb1`.
- The archive omits `LICENSE`; the license is copied verbatim from that commit.
  Registry cache markers and README are omitted.

## Local changes

The grammar accepts the following syntax from
[TOML 1.1](https://toml.io/en/v1.1.0):

- Basic strings accept `\e` and two-digit `\xHH` escapes, including multiline
  strings and quoted keys through their shared basic-string rule.
- Local and offset date-times and local times may omit seconds. A fractional
  part still requires explicit seconds.
- Inline tables allow newlines, comments and a trailing comma, including nested
  tables and tables within arrays.
- A multiline basic-string continuation accepts spaces and tabs between its
  backslash and newline, as already allowed by TOML 1.0.

The parser and metadata under `src/` are regenerated with Tree-sitter CLI
`0.25.10`, using `generate --abi 14`. The CLI also regenerates its C support
headers. Generated files are not hand-edited. The external scanner is unchanged;
it has no persistent state, allocates nothing, and serializes zero bytes.

## Scope and verification

`tests/native-grammars/tests/toml_syntax.rs` reproduces all four grammar gaps on
the unmodified published dependency. It also checks exact key/string/table ranges,
array-of-table occurrence separation, literal delimiters, malformed lexical input,
and Unicode-safe incremental edits against every node of a fresh parse. The
upstream syntax and highlighting goldens are retained without modification.

An error-free tree is not TOML semantic validation: duplicate keys/tables,
calendar validity and Unicode scalar validity require a separate interpretation
layer. Dotted-key syntax nodes do not themselves resolve semantic table ownership;
array-of-table children belong to the latest applicable parent occurrence. Native
qualification alone does not establish structural identity, MCP behavior, complete
language support or repository-wide coverage.
