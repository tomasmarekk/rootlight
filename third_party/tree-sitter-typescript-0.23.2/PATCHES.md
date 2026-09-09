# Native module export grammar

The Rust build payload is imported from the MIT-licensed
`tree-sitter-typescript` 0.23.2 archive (SHA-256
`6c5f76ed8d947a75cc446d5fccd8b602ebf0cde64ccf2ffa434d873d7a575eff`).
Its source commit is `f975a621f4e7f532fe322e13c4f79495e0a7b2e7`.
The license omitted from the crate is restored from that exact commit.
Registry bookkeeping and compiled objects are excluded.

The JavaScript grammar dependency is the MIT-licensed npm release 0.23.1,
with verified integrity
`sha512-/bnhbrTD9frUYHQTiYnPcxyHORIw157ERBa6dqzaKxvR/x3PC4Yzd+D1pZIMS6zNg2v3a8BZ0oK7jHqsQo9fWA==`.
Its grammar, queries and license are retained under `javascript/`; grammar
imports and query metadata use that local path. No package install script or
network access is required to regenerate or build the native parser.

The TypeScript override admits `export type * from` and
`export type * as Name from`, including the existing string-valued namespace
name grammar. This follows the
[TypeScript 5.0 syntax](https://www.typescriptlang.org/docs/handbook/release-notes/typescript-5-0.html)
also discussed in [upstream issue 348](https://github.com/tree-sitter/tree-sitter-typescript/issues/348).

Both inherited and TypeScript reserved-identifier alternatives no longer prefer
`export` as an expression identifier at a statement boundary. Otherwise automatic semicolon
insertion consumes a valid multiline export as an identifier expression.
Property and public module-name positions still admit the keyword through
their existing identifier-name rules; native regressions cover these positions.
An optional, non-emitting external context marker exposes a pending module
`from` clause to the existing automatic-semicolon scanner. In that context only,
four-character keyword lookahead prevents insertion before `from`; longer
identifier spellings still allow insertion. Returning to ordinary lexing keeps
the keyword and intervening comments source-visible. The marker adds no scanner
payload, serialized state, allocation or resource limit.
Semicolon lookahead uses ECMAScript whitespace, including an interior U+FEFF,
rather than locale-dependent C whitespace classification.

Generated parser C, grammar JSON and node types are produced, not hand-edited,
with official Tree-sitter CLI **0.24.7**, ABI **14**. From this directory:

```sh
cd typescript && tree-sitter generate --abi 14
cd ../tsx && tree-sitter generate --abi 14
```

The unmodified source reproduces both published parsers, grammar JSON files
and node-type files byte-for-byte with this generator and JavaScript version.
The Rust bindings, build script and scanner entry points are unchanged.
Remove these source patches after a qualified upstream release passes the
multiline, type-only, identifier-name and incremental regressions.
The upstream contextual lexer remains permissive for invalid keyword identifiers
in some expression positions; this patch is not a complete syntax validator.
Error-free parsing is not proof of complete type or module semantics;
namespace-object resolution is independently qualified by the project adapter.
