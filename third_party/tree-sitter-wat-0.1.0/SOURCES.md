# Local webassembly grammar inputs

Complete pinned upstream sources are retained under `upstream/`, including original licenses and build metadata. Upstream scripts are inert data. The operational Rust wrapper uses only exact local `tree-sitter-language` and `cc` inputs. The C11 build compiles `upstream/src/parser.c` and `src/scanner.c` without acquisition or regeneration.

See `SOURCE-PROVENANCE.json` and `UPSTREAM-INVENTORY.json` for exact source lineage and explicit inert filename mappings. The active stateless scanner corrects the missing create return, unwritten serialization byte and empty-EOF token; the original scanner remains unchanged.

Native syntax availability does not establish complete language semantics or installed-package qualification.
