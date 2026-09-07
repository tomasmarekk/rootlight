# Bash scanner state and Unicode patch

This is the Rust build payload of the MIT-licensed `tree-sitter-bash` 0.25.1
crates.io archive, SHA-256
`9e5ec769279cc91b561d3df0d8a5deb26b0ad40d183127f409494d6d8fc53062`.
The upstream release commit is `a06c2e4415e9bc0346c6b86d401879ffb44058f7`.
Registry bookkeeping and the upstream development lockfile are omitted.
The generated parser, grammar, node types, headers, Rust bindings, build script,
queries and license retain their published bytes. Exact parser regeneration
from the release tag is not claimed.

The only native change is in `src/scanner.c`:

- Empty restores erase all scanner fields. Shorter restores release removed
  heredocs while retaining reusable buffers for surviving entries.
- State lengths and delimiter terminators are checked before allocation or
  mutation. Invalid state resets to empty instead of retaining partial context.
- Every heredoc removal frees its owned delimiter buffer; an unused temporary
  comparison buffer is removed.
- Delimiter storage encodes the lexer's Unicode scalars as UTF-8, preserving the
  existing serialized byte layout. End matching compares complete scalars and
  requires the whole delimiter line, not merely a matching prefix.

The reset and Unicode contracts follow the upstream
[external scanner API](https://tree-sitter.github.io/tree-sitter/creating-parsers/4-external-scanners.html).
The affected implementation is the
[release scanner](https://github.com/tree-sitter/tree-sitter-bash/blob/a06c2e4415e9bc0346c6b86d401879ffb44058f7/src/scanner.c).
The runtime's 1024-byte serialization bound and the upstream exact-boundary
rejection remain unchanged. This is not an on-disk Rootlight state migration.
Remove the override when a released grammar passes the native regressions and
its source and ABI have been qualified again.

Only the isolated native test package currently consumes this candidate.
It is not registered as a Rootlight language adapter. Bounded state, Unicode,
source-range and incremental tests do not establish complete Bash conformance,
shell evaluation or semantic resolution. In particular, multiple heredoc inputs
on one command remain a known failing grammar case, not a successful coverage
result. No shell fixture is executed by the native tests.
