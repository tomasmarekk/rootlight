# Local Assembly source changes

This local unpublished binding retains the MIT-licensed `tree-sitter-asm 0.24.0`
publication and the exact `RubixDev/tree-sitter-asm` authored revision
`5bb5b03e3c1ce5853b5282b9fba060f7c7bbf11e`. Retention and local modification do not
transfer original authorship or exclusive copyright ownership.

Original package association was checked against the primary
[registry version metadata](https://crates.io/api/v1/crates/tree-sitter-asm/0.24.0),
[registry index](https://index.crates.io/tr/ee/tree-sitter-asm), published VCS
record and exact
[authored revision](https://github.com/RubixDev/tree-sitter-asm/commit/5bb5b03e3c1ce5853b5282b9fba060f7c7bbf11e).
No cryptographic upstream signature was verified. Source association is not a
general claim that upstream inputs are harmless.

## Retained originals

Both original archives and all 66 regular archive members remain byte-exact
under `upstream/`. The authored archive's 20 directory records, including its
root, remain represented in the inventory. Published regular modes are `0644`,
authored regular modes are `0664`, and authored directory modes are `0775`.
No original regular member declares an executable bit. Archive modes and hook
intent are retained separately from local filesystem mode representation.

Original normalized and authored Cargo manifests, locks, Rust bindings,
non-Rust bindings, package/build hooks, queries, corpus, generated sources and
README are inert retained inputs, not the active local build graph. The crate
omits the MIT notice; the exact authored notice is retained as `notices/LICENSE`.
Its copyright remains `Copyright (c) 2023 RubixDev`.

## Preferred grammar and generated tables

The active `grammar.js` is a local derivative of the retained original grammar.
`patches/physical-owners.patch` adds physical MASM procedure, macro, segment,
constant and data owners with written name/header/body/value fields. It also
admits leading blank/comment lines without a missing first item.
`patches/body-conflict.patch` declares the singleton `masm_body` ambiguity needed
by the owner body repetition. Existing instruction rules and all original corpus
bytes remain unchanged. The complete patches and preferred grammar are retained;
the original grammar is not overwritten or relabelled as the derivative.

Six active `src/` outputs are copied literally from two independently generated,
byte-identical source outputs produced with reviewed Tree-sitter CLI `0.26.8`,
Node `24.11.1` and explicit ABI14. No generated parser is hand-edited. The parser
exports `tree_sitter_asm`, has 179 states and 84 symbols, and declares no external
scanner. The generated allocator and array headers remain retained even though
the selected parser compilation includes only `tree_sitter/parser.h` and declared
system C headers. Exact input/output hashes are recorded in source provenance.

## Active static binding

The root manifest and `bindings/rust/{build,lib}.rs` are local wrappers, not the
original upstream bindings. The manifest is unpublished, uses edition 2024 and
MSRV 1.90, and pins `tree-sitter-language =0.1.7` and build dependency `cc =1.2.67`.
It activates neither the upstream optional WASM build tool nor the upstream
development dependencies. `build.rs` compiles only local `src/parser.c` as C11,
uses local headers, and declares retained source/header rerun inputs. It performs
no preferred-source generation, network fetch, package hook or module loading.

The Rust binding exposes `LANGUAGE_ASSEMBLY` and exact generated `NODE_TYPES`.
Its static factory contract is unchanged from the isolated syntax witness;
paths and archive name change for the local package layout. A separate relocated
consumer gate is required before the wrapper is described as build-qualified.

## Qualification boundary

The isolated native syntax gate passed the same seven groups on Rust 1.97.1 and
1.90.0 plus scoped Clippy. It checked six synthetic sources and all 32 upstream
corpus sections, 22 exact operand contracts and five physical MASM owners, with
no native ERROR or missing nodes. Two fixture-only corrections required named
`label` and `const` nodes when collecting declarations; anonymous literal tokens
remain in source-conservation evidence and were not hidden or removed.

These measured witnesses do not establish every Assembly dialect, assembler
binding, macro expansion, complete repository coverage, installed MCP behavior,
or empty-cache independently network-denied build acceptance. Relocated binding,
production registration, query/lowering behavior and installed evidence remain
separate gates. Private execution bodies and machine paths are not publication
inputs; only neutral receipt/output hashes and measured summaries belong in
source provenance.
