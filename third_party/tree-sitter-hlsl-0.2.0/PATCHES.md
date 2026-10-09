# Local HLSL source modifications

This local unpublished binding retains tree-sitter-hlsl 0.2.0 at authored
revision `bab9111922d53d43668fabb61869bec51bbcb915`, its exact C++ parent
`e5cea0ec884c5c3d2d1e41a741a66ce13da4d945`, and that parent's exact C source
`2a265d69a4caf57108a73ad2ed1e6922dd2f998c`. The complete original archives,
members, directories, mode records and executable intent are preserved under
`upstream/`. HLSL's exact MIT notice is `LICENSE`; the inherited notices are
`notices/tree-sitter-cpp-LICENSE` and `notices/tree-sitter-c-LICENSE`.
Retention does not imply original authorship or exclusive copyright ownership.

## Preferred grammar and static table

The preferred grammar exposes the existing `cbuffer_specifier` as a physical
top-level item with an optional written trailing semicolon. It adds the small
written `register(...)` binding alternative with raw `register` and optional
`space` fields, while preserving the inherited C++ rules and resource alias
representation. The singleton top-level conflict records the ambiguity reported
by the actual generator; it does not override semantic precedence.

A written constant-buffer name is not a declared, referenceable, unique symbol
or declaration scope. Equal written names remain distinct physical containers.
The grammar retains inherited permissive local-buffer, base-class, virtual and
name-only branches; their acceptance is not evidence that HLSL permits them.
Legacy-profile binding spellings, component/packoffset syntax and comprehensive
namespace validity have not been qualified. Written struct suffixes remain
`bitfield_clause` syntax, and resource annotations retain their original physical
alias shape; neither is reclassified as resolved runtime information.

The complete inherited grammar closure is retained as authored inert source.
Two sequential actual local Tree-sitter CLI 0.26.8 / Node 24.11.1 generations
with ABI14 produced six literally identical outputs. Both actual lexical table
guards passed. The preferred generated table is not claimed to reproduce the
original published table.

## External scanner boundary

Only the locally hardened scanner is active. Its allocation callback returns
the allocator result directly; consumers handle a null payload, and deserialization
checks the existing 16-wchar capacity and byte alignment before copying. The
normal scan algorithm and producer-state representation remain unchanged.
The original unchecked scanner is retained only as inert source under `upstream/`.

A separate preferred-only C11 callback witness actually passed 16 accepted and
five rejected state lengths, null payloads and fixture-private allocation failure.
Its MSVC compilation retained two inherited lookahead-narrowing warnings; it is
not claimed warning-clean. No crafted state or forced allocation failure was
executed against the unsafe original scanner.

## Local static Rust binding and observed qualification

The local manifest pins `tree-sitter-language = 0.1.7`, `cc = 1.2.67` and Rust
1.90. The C11 build compiles only `src/parser.c` and `src/scanner.c`; no source
download, grammar generation or archived lifecycle hook runs during compilation.
The public API exposes the exact `LANGUAGE_HLSL` factory and `NODE_TYPES`. No
query constants are invented for absent upstream HLSL queries. Original published
and authored manifests, bindings, package locks and hooks remain intact but inert.
Exact source and qualified-to-relocated binding diffs are retained in `patches/`.

The exact generated table and hardened scanner passed seven native groups and
nine full physical-source observations on Rust 1.97.1 and 1.90.0, including normal
incremental edits and trusted producer-state restoration. Both test exits and
scoped all-target/all-feature Clippy with warnings denied were zero. Independent
physical replay conserved 1,901 source bytes as 1,729 leaf bytes plus 172 whitespace
gap bytes, with zero ERROR/MISSING nodes, non-whitespace gaps or leaf overlaps.
This evidence applies to the isolated qualified wrapper, not this relocated
binding. The relocated package has not yet been compiled or tested.

This source preparation does not prove product integration, installed MCP,
whole-language coverage, empty dependency caches, OS-denied networking or complete
product build/packaging source closure. The declared compilers and SDK remain
environment prerequisites. Raw annotations are physical source facts, never
evaluated register/profile assignments, namespace/type identity or runtime binding.
