# CSS identifier and whitespace patch

This is the Rust build payload of the MIT-licensed `tree-sitter-css` 0.25.0
crates.io archive, SHA-256
`a5cbc5e18f29a2c6d6435891f42569525cf95435a3e01c2f1947abcde178686f`.
The upstream release commit is `dda5cfc5722c429eaba1c910ca32c2c0c5bb1a3f`.
Registry bookkeeping, compiled objects and unrelated package-manager files are
omitted. Rust bindings, build script, headers, query and license are unchanged.

The grammar and stateless scanner use CSS whitespace (space, tab, LF, CR and
form feed), not Unicode or locale-dependent whitespace. Identifier ranges
include every non-ASCII Unicode scalar; descendant selectors admit underscore,
escapes and non-ASCII starts. Class-name escapes reject escaped newlines while
retaining the upstream explicit escape children. String escape rules are unchanged.
These boundaries follow [CSS Syntax Level 3](https://www.w3.org/TR/css-syntax-3/).

`src/parser.c`, `src/grammar.json` and `src/node-types.json` were regenerated,
not hand-edited. Reproduction uses the official Tree-sitter CLI **0.25.10**,
from this directory with its checked-in `package.json` and `tree-sitter.json`:

```sh
tree-sitter generate --abi 15
```

Original and patched outputs reproduce byte-for-byte; node types are unchanged.
The patch adds no scanner allocation, serialized state or resource limit.
Bounded Unicode, escape and incremental tests do not imply exhaustive CSS
syntax, cascade, selector matching or value resolution support. The grammar
also retains upstream permissive extensions such as JavaScript-style comments.
Remove the override when a released grammar passes these boundary regressions
and its generated source, license and native ABI are qualified again.
