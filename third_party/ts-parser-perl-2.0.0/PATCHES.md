# Perl scanner corrections

The source is the published `ts-parser-perl` 2.0.0 crate from
https://github.com/tree-sitter-perl/tree-sitter-perl, under its included MIT license.
Archive SHA-256: `db3cd8574afc19af4d3db44fe0cf94a5e8fc3f4056c0326baf5ba01a29666129`.
The crate records revision `50904961d6a87c5191e611276aa2ecb9d66ca4ff`
with `dirty: true`; that revision is provenance, not a claim of byte equality.
The archive's generated parser and all other published files are unchanged.

`src/scanner.c` differs to preserve exact state and source boundaries:

- Empty reset clears recovery state as well as quote and heredoc state.
- Deserialize validates complete frame lengths, counts, enum values and boolean
  representations before copying or publishing native state. Invalid frames reset.
- Heredoc delimiters store complete UTF-8 rather than an eight-codepoint prefix
  plus length. Equal-length delimiters with the same prefix cannot terminate each
  other's strings and expose string bodies as definitions.
- Variable-length delimiter serialization shares the existing 1024-byte runtime
  frame with quote state. Admission rejects an unrepresentable delimiter, quote
  stack or full eight-entry heredoc queue rather than truncating or overwriting it.
  This yields parse recovery, not proof of support for resource-exhausting syntax.

The scanner frame encoding changes and must not share persisted parser artifacts
with the unpatched grammar. Production integration must bind the changed scanner
hash into provider identity. The isolated native tests cover reset, malformed and
truncated frames, exact delimiters, UTF-8/CRLF coordinates, incremental edits,
included ranges, cancellation and parser reuse; they do not prove Perl semantic
analysis or MCP support. Runtime frame capacity and queue size are unchanged.
