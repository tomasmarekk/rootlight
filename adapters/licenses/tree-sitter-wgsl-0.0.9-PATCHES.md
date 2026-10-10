# Retained WGSL source and local changes

This is a local unpublished static binding for the authenticated official
`gpuweb/tree-sitter-wgsl` revision
`52e3c620a9c316cc8c1d504dd1908eb8cebe255b`. The version `0.0.9` is the version
written in that revision's `package.json`, not a claimed Cargo or npm registry
publication. The original thirteen authored files, archive, Git identity data,
raw mode records and executable intent remain separately retained. The original
WASM and all archived Node lifecycle hooks are inert, not build inputs.

## Notices and preferred input

The exact original `LICENSE.md` is preserved at the package root and in
`upstream/authored/`. It refers separately to the W3C Software and Document
License and the GPU for the Web 3-Clause BSD License. Their authenticated raw
notice documents are retained under `notices/`; the package is not relabeled
BSD-only. The generated grammar preserves its existing W3C copyright statement
and source-license reference unchanged. The Tree-sitter MIT notice is separately
retained from the versioned runtime source for the generated support headers;
this does not establish the historical producer of the mirror grammar or WASM.

This software includes material copied from or derived from the WGSL grammar
and scanner at <https://github.com/gpuweb/tree-sitter-wgsl/tree/52e3c620a9c316cc8c1d504dd1908eb8cebe255b>.
The retained grammar states Copyright (C) [2025] World Wide Web Consortium;
<https://www.w3.org/copyright/software-license-2023/>. Local scanner modifications
and the new static binding are identified here and in the retained patch.
Original authorship, copyright ownership and license obligations are unchanged.

## Scanner patch

`patches/scanner-safety-and-trivia.patch` records the complete original-to-preferred
scanner change. The external-token order and grammar input are unchanged. The
preferred scanner keeps the existing capacity of 64, rejects overfull queues
without committing speculative state, checks allocations and serialized state,
and preserves modulo ring offsets and both LT/GT queue contents transactionally.
It handles nested comments, template trivia, valid-symbol fences and null state.
The two source-backed classifier repairs retain a following opening parenthesis
after singleton `&`/`|`, and record root adjacent `<<` as two non-template LT
entries rather than inventing a template closer.

The immutable original scanner is inert under `upstream/authored/src/scanner.c`.
The active `src/scanner.c` is the exact separately reviewed preferred version.
The original grammar's `_reserved` inline reference remains unchanged: the
actual canonical generation succeeded with its existing undefined-inline-rule
warning. No speculative grammar deletion or capacity increase is included.

## Generation and local static binding

The six retained static outputs were generated once from the exact original
grammar with Tree-sitter CLI 0.26.8, Node 24.11.1 and explicit ABI14. The original
generation receipt and its immutable source/tool hashes are recorded in neutral
provenance metadata. This is an observed canonical generation, not a twin-producer
reproducibility claim or reconstruction of the original mirror producer.

The new local Rust binding pins `tree-sitter-language = 0.1.7`, `cc = 1.2.67`
and Rust 1.90. Its C11 build compiles only the retained `src/parser.c` and
`src/scanner.c`. No source acquisition, grammar generation, WASM execution,
Node dependency installation or archived lifecycle hook runs during compilation.
`patches/local-binding.patch` records the three newly authored binding files;
there is no original Rust binding in the thirteen-file mirror source.
It exposes `LANGUAGE_WGSL`, `NODE_TYPES` and the unchanged upstream highlighting
query. Highlighting-query validity against the preferred table is not yet proven.

## Evidence boundary

The direct callback comparison actually classified eight original and eight
preferred groups independently; all preferred groups passed. The exact preferred
scanner and original canonical table then passed eight direct-C native groups,
ten full-source trees, bounded malformed/capacity recovery and the unchanged
body-only incremental/clean-equivalence witness. This is isolated native evidence,
not execution of this new relocated Rust package. Compiler warnings remain
recorded; warning-clean compilation is not claimed.

The original native receipt hashes five required compiled artifacts. Its Windows
linker also produced two inert export files (`.lib` and `.exp`) identified in a
separate closed observation. They were not compiler inputs or executed, and the
original five-artifact receipt is not retroactively a complete-output inventory.

Publication, relocated-package qualification, product integration, installed MCP,
whole-language coverage, empty dependency caches and independently blocked
network access remain separate acceptance boundaries. The Unicode property
association and historical mirror producer are not established by these witnesses.
