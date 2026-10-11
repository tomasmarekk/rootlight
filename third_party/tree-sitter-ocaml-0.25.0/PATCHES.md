# OCaml source retention and scanner state repair

This unpublished local package retains all 115 original regular files and all
52 expanded directories of the official `tree-sitter-ocaml` v0.25.0 release
archive. The original archive remains at
`upstream/tree-sitter-ocaml-v0.25.0.tar.gz`, SHA-256
`55cdd7443a7838e650ed070ad28ed4dd774867331da01df2cc5ad3272f5598bd`.
`SOURCE-INVENTORY.json` records original archive modes, exact retained paths and
the complete local payload. Original upstream metadata, ignore files, manifests,
README, frontend inputs and query assets are inert source data, not instructions
or authoritative parent dependency locks.
The exact original `.gitignore` and `.gitattributes` are retained as
`.gitignore.upstream` and `.gitattributes.upstream`. They do not hide the retained
archive or override this project's byte-preserving publication policy.

The retained MIT `LICENSE` preserves the supplied 2020 Max Brunsfeld and Pieter
Goetschalckx copyright. Local retention and repair do not imply original
authorship, exclusive ownership, complete legal-notice review or removal of
supply-chain risk. The GitHub release archive is not a crates.io distribution.
Its checksum must not be represented as a registry crate checksum.

## Original and local binding inputs

The original `Cargo.toml` is byte-exact at `Cargo.toml.upstream`. The local
manifest is explicitly unpublished, declares the existing Rust 1.90 minimum,
and pins the existing local-source dependency identities: `tree-sitter-language`
0.1.7, `cc` 1.2.67 and development-only Tree-sitter 0.26.11. It adds no new
dependency implementation. The original Cargo lock remains inert and unchanged;
parent locked local-source resolution is a separate integration obligation.

The upstream Rust library and build script are unchanged. They retain the
official `LANGUAGE_OCAML` and `LANGUAGE_OCAML_INTERFACE` factories and compile
only checked-in C parsers/scanners with local headers. The third type-fragment
grammar, factory and source metadata remain retained and compilable by this
unchanged upstream binding, but are not a Rootlight physical-file profile.
Upstream query exports are not adopted as Rootlight semantic assertions.
The first-party adapter must own physical routes, reviewed query contracts,
source identity, scoped gaps and coverage accounting outside this package.

## Local scanner codec repair

The original `common/scanner.h` remains byte-exact at `common/scanner.h.orig`.
The active header contains the qualified bounded UTF-8 state repair. All three
preferred grammars, generated grammar/node metadata, C parsers, scanner wrappers
and support headers are unchanged. No generation ran for this repair.

The scanner retains its seven-token ABI and unchanged 1,024-byte serialization
buffer. State consists of the exact boolean flag and the complete UTF-8 delimiter
payload. Encoding validates and measures the complete payload before writing;
invalid or oversized state refuses with zero bytes and no partial output or
live-state mutation. Decoding validates into bounded stack storage, checks a
temporary allocation result and commits the complete scalar sequence. Malformed
state resets logical delimiter length and flag instead of retaining a prefix.
The existing resize/push helpers and all other scanner decisions are unchanged.

On both Rust 1.97.1 and minimum 1.90.0, the isolated `ocaml_syntax` suite ran its
four unchanged Rust tests with the exact repaired header and all three original
native parsers: all four passed. Controls include the original Unicode state
round-trip, fresh-versus-restored parsing, implementation declarations and
abstract interface signatures. The same C witness additionally checked exact
ASCII/mixed buffer capacity, scalar widths, mutation-free refusal and malformed
state reset/reuse. The original header failed its Unicode state comparison on
both toolchains; that historical failure remains distinct from this repair.

This is bounded native qualification of the exact header/parser inputs, not
execution of this relocated package or its local manifest. No source-to-binary
reproduction, generated-table reproduction, complete OCaml conformance, product
analyzer, installed MCP, whole-language coverage or full-product source closure
is asserted. Refusal for a delimiter that cannot fit the finite serialization
interface does not prove incremental equivalence for arbitrarily long legal
delimiters. Empty dependency caches and independently blocked network acceptance
remain separate requirements.
