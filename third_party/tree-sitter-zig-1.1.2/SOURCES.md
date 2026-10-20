# Retained Zig parser sources

The complete 51-file tree-sitter-grammars/tree-sitter-zig v1.1.2 source tree
at b670c8df85a1568f498aa5c8cae42f51a90473c0 and its exact archive are retained
under `upstream/` and `upstream.tar.gz`. The original MIT notice is preserved.
The upstream Git attributes file is retained as `.gitattributes.upstream`
so published upstream policy cannot change the containing source frame.

The selected `grammar.js` changes only optional container membership in four
rules. `patches/empty-container-members.patch` records that source correction.
Actual Tree-sitter CLI0.26.8 generation explicitly requested ABI14 from the
grammar root; its parser, node metadata and local headers are retained in `src/`.
There is no external scanner. The modern LanguageFn wrapper compiles only
local C11 parser/header inputs and uses existing pinned build dependencies.
No build invokes source downloads, lifecycle scripts or code generation.

The native preparation parsed all 762 original Zig source files with exact
incremental/fresh equality. Six files still have 14 retained syntax gaps;
their exact original fixtures and source pins remain under `tests/fixtures/`.
Those gaps are applicable limitations, not clean parsing or whole-language
qualification. The provider proposal covers bare written names and lexical
containment. Imports, comptime evaluation, inferred types and call resolution
require separate semantic proof.
