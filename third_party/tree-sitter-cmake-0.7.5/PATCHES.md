# Retained CMake source modifications

The exact `tree-sitter-cmake` 0.7.5 publication and authored revision
`e997bd0b275ca525ce9befecedf5299031183661` remain unchanged under `upstream/`.
Both archives, all 21 published files and all 67 authored files retain their
authenticated bytes and archive-derived modes. Original ignore and attribute
files are named `.gitignore.upstream` and `.gitattributes.upstream` so they cannot
hide retained inputs or alter the repository's byte-preservation policy. These
archives and their build metadata remain inert provenance.

`patches/scanner-safety.patch` changes only preferred `src/scanner.c`. Creation
and deserialization initialize/reset every state field. The fixed six-byte,
versioned encoding excludes C padding and enum-layout assumptions; malformed
lengths, versions, token ordinals and impossible delimiter widths reset state.
Delimiter counters check their representable width before incrementing. The
maximum width leaves room for both bracket pairs within a 32-bit source length.

Content scans preserve leading layout, initial newlines, non-ASCII whitespace
and failed closing-delimiter attempts. Only a complete matching delimiter ends
the content token. New-token layout uses explicit ASCII bytes; locale-dependent
classification no longer consumes source. Failed bracket openers cannot fall
through into a later unrelated comment. Comment token selection respects the
parser's valid-symbol set. Allocation failure returns no scanner token; it does
not dereference a null state.

`patches/written-syntax.patch` changes the preferred grammar to admit empty
`if()` and `elseif()` conditions and to retain configure placeholders as named
syntax nodes. Empty `if()` is false and empty `elseif()` is accepted with a
developer warning by CMake 4.3.3; `while()` still requires an argument. The
placeholder nodes preserve template text such as `@PACKAGE_INIT@` and
`${z_@PORT@_root}` without losing later written commands to parser recovery.
They do not perform substitution or make the template executable CMake. Adapter
queries retain explicit substitution/evaluation gaps and do not infer computed
declaration names or bindings from these nodes.

`patches/grammar-regeneration.json` records the grammar input and generated output
hashes, the declared local Tree-sitter 0.26.8 generator, its native JavaScript
runtime and the explicit ABI14 command. Two fresh-directory regenerations
produced the checked-in parser, node schema, grammar JSON and support headers
byte for byte. No generated table is hand-edited and no generator or acquisition
runs during compilation. The original grammar, parser, schema, support headers
and scanner remain unchanged in both retained upstream inventories.

`patches/local-binding.patch` describes the unpublished local Rust manifest and
binding. It pins the already versioned `tree-sitter-language` 0.1.7 and `cc`
1.2.67 inputs, declares Rust 1.90 and compiles only the retained parser/scanner.
Archived upstream dependency locks and lifecycle hooks are never executed.
The static ABI14 handle remains subject to runtime ABI compatibility checks.

Native tests exercise initialization, complete reset, malformed serialization,
exact bracket content, delimiter widths, mixed command case, distinct arguments
and incremental recovery. Those finite witnesses do not establish whole CMake
syntax coverage, command evaluation, macro expansion, variable or target binding,
installed MCP correctness, source publication or offline package acceptance.
The scanner consumes admitted source through the native lexer; it does not own
an independent cancellation channel or a semantic execution engine.
