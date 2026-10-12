# SAS grammar provenance and local binding

This namespace retains the complete published `tree-sitter-sas` 0.4.2 crate
alongside declared local binding inputs. `SOURCE-PROVENANCE.json` records the
original archive, exact retained-file mapping and separate local changes.

The archive comes from
[`tree-sitter-sas` 0.4.2](https://static.crates.io/crates/tree-sitter-sas/tree-sitter-sas-0.4.2.crate)
and has SHA-256
`cb4d6233b3cf8a9d495ecc6ed8fec7e6a29ec15a0c5808ef046504c28e52334c`.
All 19 original regular files, totaling 1,768,173 bytes, remain byte-exact.
Their archived executable intent is false. The retained MIT `LICENSE` preserves
`Copyright (c) 2026 Ix`.

The archive's `.cargo_vcs_info.json` declares commit
`975034cc87a74447bfd49e2f4d208716c37850f2` in
[`ix-infrastructure/tree-sitter-sas`](https://github.com/ix-infrastructure/tree-sitter-sas).
That declaration is not an independently verified commit object, repository tree
or signature. Retention covers this published crate, not a reconstructed entire
upstream repository or all legal notices. Local retention does not imply
first-party authorship or exclusive ownership.

## Retained original inputs

The published normalized `Cargo.toml` remains at `Cargo.toml.upstream`.
The archive itself already contains `Cargo.toml.orig`; that file retains its
original meaning and bytes. Replaced Rust build/binding inputs have `.orig`
copies. The original preferred grammar, generated grammar/node metadata,
parser C table and array header also have exact `.orig` copies reserved for
declared local replacements. The original parser header is retained as
`src/tree_sitter/parser.h.orig`. Other original files retain their paths.

The complete archive remains at `upstream/tree-sitter-sas-0.4.2.crate`.
Original README, Cargo lockfile, Tree-sitter metadata and query assets are inert
source data, not instructions, authoritative parent dependency locks or invoked
package hooks. The materializer-derived `.cargo-checksum.json` is not an
original archive file and is not included in the 19-file original inventory.

## Declared local Rust binding changes

- `Cargo.toml`: unpublished edition-2024 path binding with Rust 1.90 and exact
  existing `tree-sitter-language` 0.1.7 and `cc` 1.2.67 dependencies. It does not
  claim that the modified local namespace is the published registry package.
- `bindings/rust/lib.rs`: the minimal static `LanguageFn` handle preserves the
  `tree_sitter_sas` factory and exposes exact retained `NODE_TYPES`. The
  edition-2024 unsafe extern declaration and static-table safety contract do not
  certify parser behavior, language conformance or runtime binding. Original
  upstream query assets are retained but are not exported or adopted as
  Rootlight semantic assertions by this binding.
- `bindings/rust/build.rs`: compile only local `src/parser.c` and
  `src/scanner.c` with the local headers, declared C compiler and versioned
  `cc`. Its source acquisition, grammar generation and upstream package-hook
  invocation are absent. This code inspection is not a successful build or
  independently enforced offline-acceptance receipt.

## Preferred grammar and generated tables

The preferred `grammar.js` adds written FCMP function and subroutine headers,
formal parameters, character return lengths, physical terminators and opaque
body statements. Macro definitions expose written headers, formals, defaults
and terminators. DATA headers expose each written output and retain adjacent
macro leaves without assigning expanded dataset identity. Opaque lexical
statements exclude physical declaration boundaries so malformed owners cannot
borrow a following declaration or terminator. These rules describe source
structure, not execution or compiler acceptance.

DATA bodies expose SET, MERGE, UPDATE and OUTPUT dataset operands as native
leaves. The opaque fallback excludes these exact initial words, while keyword
expressions and longer identifiers remain opaque. A trie preserves exclusions
with shared prefixes, including SET and SUBROUTINE. Named block and percent
comments have lexical priority at token boundaries so leading comments cannot
hide the next dataset verb; quoted tokens remain atomic. Repeating a single
separator with an optional word preserves the accepted language without the
redundant nested repetition that made grammar preparation expensive.

One complete grammar/output lineage is selected as static package input.
Its preferred grammar has SHA-256
`f7ed33c8f68c19732a19445038786f4d51dacf64d5f2c7b80308496f7040f936`.
The six generated files are `src/grammar.json`, `src/node-types.json`,
`src/parser.c`, `src/tree_sitter/alloc.h`, `src/tree_sitter/array.h` and
`src/tree_sitter/parser.h`; exact lengths and hashes are recorded in
`SOURCE-PROVENANCE.json`. The parser declares ABI 14, 1,505 parse states and the
original ten-token external scanner ordering. The scanner, Rust bindings and
dependencies remain unchanged by this grammar/output selection.

Preparation uses the declared Tree-sitter CLI 0.27.1 and Node 26.2.0 tools with
`generate --abi 14 --js-runtime node grammar.js`, separately from compilation.
Two independent generations completed successfully with empty stderr and
byte-identical outputs for all six generated files. Exact preferred-source and
tool hashes bind these comparisons. They do not reproduce the original upstream
tables or establish equivalence to unrelated grammar variants. Generation is a
separate preparation step, never a compile-time action.

## Bounded qualification

All 15 tests in `tests/native-grammars/tests/sas_syntax.rs` pass on Rust 1.97.1
and minimum Rust 1.90.0. They retain the original 13 controls and add direct
DATA operand leaves, keyword-variable rejection, physical spans and comment
boundaries. All ten public tests in
`crates/rootlight-adapter-treesitter/tests/analyzer_e2e/sas_native.rs` pass on
both toolchains, including the exact unresolved dataset-reference evidence.
These observations preserve physical owner fences, written names and source
identity without claiming resolved catalog bindings or executed macro behavior.

The exact query-registry test
`reviewed_packs_compile_with_the_exact_closed_capture_contract` and SAS
owner-cache test
`owner_cache_reuses_proofs_obeys_capture_bound_and_checks_cancellation` pass
on both toolchains. Native syntax, query compilation, bounded cache behavior
and these ten analyzer fixtures do not establish complete SAS conformance,
installed-MCP acceptance or indexing performance.

## Source identity and acceptance boundary

The original registry archive checksum authenticates the retained original
archive only. The unpublished local path binding uses the grammar lock's
canonical complete vendored-source inventory instead: SHA-256 over
`rootlight-vendored-source-v1\0`, followed by sorted UTF-8 relative paths,
NUL, lowercase file SHA-256 and NUL for every local file. The inventory includes
original retained inputs, original archive, local replacements and these
metadata files; no self-referential digest is embedded here. Parent Cargo locks
and versioned registry-source inventories bind transitive Rust inputs separately.

Physical declarations, headers, raw names and terminators are not proof of
SAS compiler acceptance, FCMP execution, expanded macros, catalog identity,
runtime procedure selection, dynamic dataset names or resolved calls.
Retention and this local binding do not establish full SAS coverage, installed
MCP semantics, source-to-binary reproduction, complete product source closure,
empty dependency caches or independently blocked network acceptance.
