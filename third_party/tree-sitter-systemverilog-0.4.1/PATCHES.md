# Retained SystemVerilog source modifications

The full published 0.4.1 archive and exact authored revision
`3390da8c76a976b6f47d19667accc43e81a1483b` remain under `upstream/`, with
all 47 files from each archive, Git metadata and archive-derived mode intent.
Original attribute and ignore files use `.upstream` suffixes so they cannot
alter source retention policy. Archive modes are recorded separately from
host filesystem permissions.

The published `src/tree_sitter/array.h` adds C++ pointer casts compared with
the authored tree. Both exact versions remain retained. Preferred inputs use
the Git-authenticated authored tree; parser.c and parser.h are identical in
both archives. This parser includes only parser.h and has no external scanner.
No preferred grammar or generated table is edited. Upstream optimization
pragmas remain intact; original generator identity is not inferred.

The unpublished local binding pins already retained tree-sitter-language
0.1.7 and cc 1.2.67, uses Rust 1.90 and compiles only the local C11 parser.
Original npm, Cargo, generation and maintenance programs stay inert.

The separate upstream test Git link is not covered by the grammar MIT grant.
Its pinned repository has no root license and contains multiple content
origins, including Apache and Solderpad notices and unlicensed-in-the-copy
fragments. Its content is not versioned in this package or required by native
compilation. Reading separately acquired fixtures in local diagnostics does
not assert redistribution clearance or complete versioned test closure.

Native ABI, syntax, edit and corpus results remain separate from production
language extraction, elaboration, installed MCP and offline package acceptance.
