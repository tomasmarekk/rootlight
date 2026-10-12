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

One complete reference grammar/output lineage is selected as static package
input for bounded native qualification. Its preferred grammar has SHA-256
`bcc99be09ea7e21ccd9d04b170bee0b72c74a6fe1783791abd4221c6af166a03`.
The six generated files are `src/grammar.json`, `src/node-types.json`,
`src/parser.c`, `src/tree_sitter/alloc.h`, `src/tree_sitter/array.h` and
`src/tree_sitter/parser.h`; their exact lengths and hashes are recorded in
`SOURCE-PROVENANCE.json`. All six are retained unchanged from that lineage,
including the generated allocation header whose bytes equal the original.
The parser table declares ABI 14, 1,506 parse states and the original ten-token
external scanner ordering. The scanner, Rust bindings and dependencies are
unchanged by this grammar/output selection.

Preparation uses the declared Tree-sitter CLI 0.26.8 and Node 24.11.1 tools with
`generate --abi 14 --js-runtime node grammar.js`, separately from compilation.
Two independent reference generations completed successfully with empty stderr
and byte-identical outputs for all six generated files. Their closed producer
observations and live output comparisons authenticate this selected lineage;
they do not reproduce the original upstream tables or establish equivalence to
other grammar variants. Grammar generation remains a separate preparation step,
not a compile-time action or independently network-denied acceptance proof.

## Bounded qualification

The existing `tests/native-grammars/tests/sas_syntax.rs` suite passed all 13 tests
on Rust 1.97.1 and the minimum Rust 1.90.0 with the selected parser and unchanged
scanner. These observations cover written function, subroutine, macro and DATA
headers, exact source spans, opaque body boundaries and malformed-owner fences.
They do not establish complete SAS conformance or upstream-tree equivalence.

The production query-registry exact test
`reviewed_packs_compile_with_the_exact_closed_capture_contract` and the private
SAS owner-cache exact test
`owner_cache_reuses_proofs_obeys_capture_bound_and_checks_cancellation` each
passed on both toolchains. The ten tests in
`crates/rootlight-adapter-treesitter/tests/analyzer_e2e/sas_native.rs` produced
five passes and five failures per toolchain in the recorded targeted run.
Production-analyzer qualification remains incomplete; passing native syntax,
query compilation and cache checks is not full analyzer or installed-MCP
acceptance. This metadata update changes no compiled grammar or binding input.
The remaining structured DATA-reference gap concerns SET and OUTPUT statements
whose dataset spellings are not exposed as distinct native dataset nodes.
Lexing a whole statement does not establish exact dataset-reference evidence;
complete source retention and product query behavior require separate checks.

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

