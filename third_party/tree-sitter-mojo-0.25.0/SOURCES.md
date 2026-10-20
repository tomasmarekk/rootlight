# Pinned Mojo grammar inputs

The complete 82-file MIT upstream source tree remains unchanged at the revision
in SOURCE-PROVENANCE.json. UPSTREAM-INVENTORY.json binds every original path,
Git blob, mode and byte digest. Upstream ignore and attribute names are inert;
their mapping preserves their original bytes and provenance.

The active local grammar adds default mutable unified capture syntax and the
legacy exact @parameter directive before a for loop. It preserves ordinary
parameter identifiers and existing decorated functions. LOCAL_CHANGES.patch
records this DSL delta and the separate scanner state bounds correction.

The operational Rust wrapper compiles the retained src/parser.c and src/scanner.c
using src/tree_sitter headers. Node metadata is src/node-types.json. The local
parser, metadata and support headers were generated in a separate preparation
step using the pinned Tree-sitter CLI 0.26.8 native JavaScript runtime and ABI15.
Their exact byte digests and generator identity are in SOURCE-PROVENANCE.json.
All original upstream parser tables, headers, grammar, tests, wrappers and locks
remain retained as provenance under upstream/.

Compilation invokes the declared C toolchain through the pinned local cc crate.
It does not invoke a generator, upstream workflow, installation or acquisition.
The current LanguageFn wrapper selects the reviewed local dependency versions.

The local tree-sitter.json defines the active grammar and local syntax corpus.
Original upstream editor queries and their complete tests remain upstream inputs;
they are not silently rewritten into the local syntax regressions. Syntax tests
do not establish Mojo compiler acceptance, lexical targets, types or execution.
Written declarations, ownership, Read and Calls require separate current-source
proof. A clean tree alone does not establish complete language support.
