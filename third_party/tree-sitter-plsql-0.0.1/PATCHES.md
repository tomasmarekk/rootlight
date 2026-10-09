# PL/SQL grammar provenance and local binding

The complete authored tree comes from
[`andreasmaierde/tree-sitter-plsql`](https://github.com/andreasmaierde/tree-sitter-plsql/tree/28aebef209be57169600e1aa41ca1431cc6c916f),
commit `28aebef209be57169600e1aa41ca1431cc6c916f`, Git tree
`03885a317dd35a07601d2449e3849ec8e2329b7e`. All 27 original blob identities,
sizes, modes and SHA-256 values are recorded in `SOURCE-PROVENANCE.json`.
The supplied MIT license and 2022 AndreasMaierDe attribution remain unchanged
in `LICENSE`. Local retention does not imply first-party authorship, exclusive
ownership, a signature verification or complete legal-notice qualification.

All original files are retained, including README, ignore metadata, Node/build
inputs, examples, corpus and image. They remain inert upstream input data, not
project instructions, source-inventory filters or invoked package hooks. The
three replaced Rust binding inputs, preferred grammar and four original
generated files retain exact `.orig` copies. Original fixtures, license and image
remain unchanged. The locally generated ABI14 table has 10,623 states and no
external scanner tokens. The generated `alloc.h` and `array.h` are new local
generator outputs, not claimed as original upstream files.

## Declared local changes

- `Cargo.toml`: local unpublished edition-2024 binding with the project's Rust
  1.90 baseline, exact already-vendored `tree-sitter-language` 0.1.7 and `cc`
  1.2.67 dependencies. The repository URL identifies the actual authored tree,
  not the upstream manifest's generic template repository. The original manifest
  remains `Cargo.toml.orig`; its obsolete Tree-sitter 0.20 dependency is not used.
- `bindings/rust/lib.rs`: `LanguageFn` exposes the static
  `tree_sitter_plsql` C factory without returning an obsolete Rust `Language`
  representation by value. `NODE_TYPES` includes the exact locally generated metadata.
  The original binding remains `bindings/rust/lib.rs.orig`.
- `bindings/rust/build.rs`: compile only the locally generated parser C table with its
  local header, declared compiler and versioned `cc`. Both local C inputs are
  rebuild triggers. No external scanner, generator, Node hook or network source
  acquisition is invoked. The original remains `bindings/rust/build.rs.orig`.
- `grammar.js`: standalone `CREATE PROCEDURE` and `CREATE FUNCTION` accept the
  single `END [name];` already consumed by their body, instead of requiring a
  second `END`. Their alternate external call-specification branch consumes its
  terminating semicolon. The original remains `grammar.js.orig`.
- Conditional compilation: `$IF`/`$ELSIF`/`$ELSE`/`$END` preserve separately typed
  physical statement, package-specification declaration and local/body
  declaration-section branches. Nested directives recurse within the same
  context. Directives do not require an extra ordinary-statement semicolon;
  ordinary branch statements and declarations retain their own delimiters.
  `$$identifier` is a distinct lexical inquiry expression, not a declaration or
  inferred reference target. Its base-expression precedence follows existing
  identifier-expression precedence. No conditions are evaluated or branches
  selected; `$ERROR` and conditional datatype fragments remain outside this slice.
- Function properties: `RESULT_CACHE` admits an optional `RELIES_ON (...)`
  dependency clause. Physical conditional-compilation branches recurse separately
  through function-header properties or cache-dependency fragments, not through
  a statement/declaration union. Written dependencies do not establish catalog
  identity, cache invalidation semantics or an active compilation branch.
- Grammar ambiguity: the original declared conflicts remain intact, with one
  additional singleton `result_cache_clause` conflict. An optional cache dependency
  and a following conditional property can share the same directive prefix;
  retaining both native alternatives does not infer which condition is active.
  Original extras remain unchanged.

## Declared preparation-only generation

Two fresh generation directories used the same patched preferred input and the
already declared Tree-sitter CLI 0.24.4 with Node.js 24.11.1:

```text
tree-sitter generate --abi 14 --js-runtime node grammar.js
```

Both executions exited successfully with empty stderr. All six outputs were
independently byte-identical: `src/grammar.json`, `src/node-types.json`,
`src/parser.c`, and `src/tree_sitter/{alloc.h,array.h,parser.h}`. Exact tool,
preferred-input, output and generation-receipt hashes are recorded in
`SOURCE-PROVENANCE.json`. The four replaced original generated files remain at
their original paths with `.orig` appended. No generated C table is hand-edited.

This proves equality of these two local generation results, not reproduction of
the original upstream tables, an independently verified tool signature, an
OS-enforced network denial, source-to-binary reproduction or a product build.

Compilation has no source acquisition or grammar generation. This package is
not claimed as a published registry distribution; no registry checksum is
fabricated. The grammar lock authenticates the complete local file inventory,
including original and locally replaced inputs, with its canonical manifest
digest. Parent Cargo lockfiles and the versioned registry-source inventory bind
the existing transitive Rust inputs separately.

This native grammar describes physical written syntax, not all Oracle PL/SQL,
SQL dialect legality, compiler semantics or runtime dispatch. Parenthesized
`ref_call` nodes do not independently distinguish function calls from collection
indexing. Conditional branch selection, dynamic SQL text, wrapped sources, overload
selection, package initialization and unsupported SQL forms require separate
source-scoped coverage and binding evidence. Neither retention nor this binding
qualifies those semantics, whole-language coverage, source-to-binary reproduction,
empty caches, OS-enforced network denial or complete product-source acceptance.
