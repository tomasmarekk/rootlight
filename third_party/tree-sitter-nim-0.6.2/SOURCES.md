# Nim grammar source

This package retains tree-sitter-nim 0.6.2 from
`https://github.com/alaviss/tree-sitter-nim`, commit
`897e5d346f0b59ed62b517cfb0f1a845ad8f0ab7`. All 84 original files and
22 directories are preserved under `upstream/`; upstream Git ignore/attribute
files have `.upstream` suffixes so they cannot change this repository's rules.
The exact 3,854,660-byte archive is `upstream.tar.gz`, SHA-256
`5ee12363f12b72fdf7048cdff3aa3124c9d70eabf01c5e843ede2b1ce7527e12`.
The inventory records original Git and archive modes independently of the host.

The ABI14 generated parser, preferred grammar, node metadata and paired headers
are original bytes. The local C11 binding compiles only `src/parser.c` and
`src/scanner.c`; no source acquisition, generation, npm lifecycle, original Rust
wrapper or prebuilt artifact participates. Existing pinned LanguageFn and cc
dependencies provide the binding. Original licenses and SPDX sidecars remain
under `upstream/`, including MPL-2.0, MIT and CC0 notices; `LICENSE.txt` is the
unchanged primary MPL-2.0 notice. Vendoring does not imply original authorship.

`patches/scanner-state-bounds.patch` bounds the scanner's actual copy and rejects
foreign serialized layout states before allocation. A native control reproduced
the original copy beyond the 1,024-byte destination after malformed 1,025-byte
ABI deserialization. Scan-created states instead use strictly increasing byte
indents from 0 through 254, which round-trip unchanged; parser-generated overflow
reachability has not been established. Empty deserialization resets without
forming an offset from a null pointer.

The grammar intentionally accepts a superset of Nim. Compile-time selection,
templates, macros, overload/type checking and module loading require separate
analysis authority. Tree acceptance and a bounded static call edge do not prove
complete Nim or NimScript support.
