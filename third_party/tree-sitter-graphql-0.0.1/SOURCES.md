# GraphQL source provenance

The complete upstream tree and archive are retained at commit
`951bde9fb3145b5f676204231e35f8b21d21f7b3` from
`https://github.com/11bit/tree-sitter-graphql`.
The official exact-commit archive is 55,909 bytes, SHA-256
`c78d89e1e5f62dd75e9a6eb475032d3cd254dd2352bede0b56f4d0bd5e18b87a`.
All 28 regular upstream members, authored grammar, generated parser/header,
optional bindings and bootstrap metadata remain under `upstream/`. The exact
archive, including original directory metadata and executable intent, is
`upstream.tar.gz`. No upstream ignore rule removed members.

The root grammar, generated C/JSON and original parser header are exact copies.
The local Rust manifest and LanguageFn binding use the existing shared runtime;
they do not use the upstream Tree-sitter 0.20 Rust type. The build script compiles
only local parser C with its original paired header. Its one local include is
`src/tree_sitter/parser.h`; that header requires standard stdbool.h, stdint.h and
stdlib.h. There is no external scanner, download or generator invocation.

`LICENSE` preserves the upstream MIT text and bkegley copyright. Local binding
and Rootlight query/lowering code do not claim authorship of upstream source.
The unpublished local crate version follows upstream Cargo metadata 0.0.1;
the upstream npm 0.0.2 label is distinct and does not identify a registry crate.

The ABI14 parser is not regenerated. Its original optimization-off pragmas remain
unchanged; no indexing performance result is inferred. Escape handling is more
permissive than GraphQL, schema extensions are incomplete, and an executable field
accepts only one direct directive. Those limitations remain explicit provider
gaps, not full GraphQL support. Optional upstream Node/NAN and generator routes
are retained metadata but are never executed by the local Rust build path.
