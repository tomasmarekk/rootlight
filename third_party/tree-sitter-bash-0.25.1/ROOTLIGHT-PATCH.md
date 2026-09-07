# Bash scanner state, Unicode and heredoc groups

This is the Rust build payload of the MIT-licensed `tree-sitter-bash` 0.25.1
crates.io archive, SHA-256
`9e5ec769279cc91b561d3df0d8a5deb26b0ad40d183127f409494d6d8fc53062`.
The upstream release commit is `a06c2e4415e9bc0346c6b86d401879ffb44058f7`.
Registry bookkeeping and the upstream development lockfile are omitted.
Headers, Rust bindings, build script, queries and license retain their published
bytes. The original generated parser, grammar JSON and node types reproduce
exactly from the published grammar with Tree-sitter CLI 0.25.10, ABI 15.

The scanner changes are in `src/scanner.c`:

- Empty restores erase all scanner fields. Shorter restores release removed
  heredocs while retaining reusable buffers for surviving entries.
- State lengths and delimiter terminators are checked before allocation or
  mutation. Invalid state resets to empty instead of retaining partial context.
- Every heredoc removal frees its owned delimiter buffer; an unused temporary
  comparison buffer is removed.
- Delimiter storage encodes the lexer's Unicode scalars as UTF-8, preserving the
  existing serialized byte layout. End matching compares complete scalars and
  requires the whole delimiter line, not merely a matching prefix.
- Additional heredocs in one command form a FIFO group. Nested command
  substitutions push independent groups and resume their parent's pending
  inputs when finished. Group continuation uses bit 1 of the existing serialized
  started-state byte; bit 0 retains its original meaning. No state bytes are added.

`grammar.js` admits interspersed heredoc headers, arguments and file redirects.
Distinct external continuation/end tokens separate group boundaries without
turning pending outer input into an inner command's body. Header-only statement
rules defer bodies across pipes and conditional operators until the entire
connected command header has been read. Ordinary arguments remain owned by their
command, while substitutions retain independent nested groups. Generated
`src/parser.c`, `src/grammar.json` and `src/node-types.json` are regenerated,
never hand-edited. Reproduce from this directory using the pinned CLI:

```sh
tree-sitter generate --abi 15
```

The public node vocabulary is unchanged; the heredoc `descriptor` field can now
occur more than once. Connected header contexts expose the existing statement
alternatives and optional arguments after redirects. Ordered source-range, quoted-expansion, nested-lifetime,
invalid-input and incremental tests exercise the compiled native parser.

The reset and Unicode contracts follow the upstream
[external scanner API](https://tree-sitter.github.io/tree-sitter/creating-parsers/4-external-scanners.html).
The affected implementation is the
[release scanner](https://github.com/tree-sitter/tree-sitter-bash/blob/a06c2e4415e9bc0346c6b86d401879ffb44058f7/src/scanner.c).
The runtime's 1024-byte serialization bound and the upstream exact-boundary
rejection remain unchanged. This is not an on-disk Rootlight state migration.
Remove the override when a released grammar passes the native regressions and
its source and ABI have been qualified again.

The Rootlight syntax adapter and isolated native tests consume this grammar.
Registration provides structural evidence, not shell evaluation. Bounded state, Unicode,
source-range and incremental tests do not establish complete Bash conformance,
shell evaluation or semantic resolution. Tests cover multiple inputs on one
command and connected pipeline/conditional commands; they do not exhaust every
compound-statement or pending-input combination. The upstream corpus's adjacent closing-parenthesis delimiter case
differs from strict whole-line matching: Bash itself emits an unterminated-heredoc
warning for that form. Both the previous and grouped candidates disagree with that
one upstream golden tree; it is not silently counted as a passing case.
No shell fixture is executed by the native tests.
