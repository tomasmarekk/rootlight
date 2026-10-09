# Local Pascal preferred-source and binding changes

The complete published version 0.10.2 archive, primary metadata, all 15 member
identities, original manifests, Rust wrappers, LICENSE, and preferred/static
sources remain retained. `UPSTREAM-INVENTORY.json` maps replaced inputs to
byte-exact originals; `SOURCE_PROVENANCE.md` records actual origins, generator
pins, output identities, and unqualified boundaries.

## Correct ordinary keyword spellings

The original preferred source uses `/seled/i` for `kSealed` and `/safecal/i`
for `kSafecall`. The active preferred source changes only those two patterns
to `/sealed/i` and `/safecall/i`. The original static table produced native
ERROR identifiers for ordinary correctly spelled fixtures. Those strict
fixtures were not misspelled, skipped, or weakened.

Two independent preparations using the pinned CLI 0.24.4 and Node 24.11.1
generated ABI14 static sources. All six derived file pairs matched by size,
SHA-256, and actual bytes; no parser C or generated header was edited by hand.
Node types and parser header remain identical to published originals. A
separate generic C-lexical check accepted both generated tables within the
actual uint16 ABI width. No header widening or global budget change occurred.

The unchanged six generic native fixtures passed after generation, compared
with four passes and two spelling failures before it. Targeted native Clippy
with warnings denied passed. This is selected physical syntax/source evidence,
not a corpus percentage, full grammar pass, installed MCP proof, performance
claim, or compiler/preprocessor/binding equivalence claim. Acceptance of old
misspellings is not retained as a supported syntax contract.

## Local static build and Rust binding

The unpublished local manifest selects already declared `tree-sitter-language`
0.1.7 and `cc` 1.2.67, edition 2024, and MSRV 1.90. Its build compiles supplied
static parser bytes as C11, tracks both parser and header inputs, and uses
compiler-supported flags. There is no active scanner or build-time download,
generation, JavaScript/package lifecycle, or arbitrary upstream hook.
The documented immutable factory binding retains the original public interface.
Original build/lib files and both authored and normalized manifests remain
separate; normalized Cargo bytes use `Cargo.toml.published.orig`, preserving
the preexisting authored `Cargo.toml.orig` without collision.

## Explicit remaining limitations

The original preprocessor helper supports a single conditional group, not
arbitrary nested ifdefs, and does not choose active branches. Qualified,
generic, overloaded, parameterless, and member uses do not imply resolved
bindings. Shared-name declarations require individual direct written name
spans; forward types remain legitimate declarations without invented bodies.
The upstream highlight queries include heuristics and broad references and are
retained as inert data, not authoritative product binding policies.

The published VCS commit's exact authored archive returned HTTP 404; no master,
tag, or alternative source was substituted. Published checksum authentication
does not prove authored-tree equivalence or signed provenance. Legal/security
completeness, whole-language semantics, complete build/packaging source closure,
empty caches, independently denied networking, and source-to-binary
reproducibility remain unqualified.
