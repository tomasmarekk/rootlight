# Retained Fish syntax inputs

The upstream source is `ram02z/tree-sitter-fish`, tag `3.7.0`, revision
`fa2143f5d66a9eb6c007ba9173525ea7aaafe788`. The complete archive and its 56
authored regular files are retained under `upstream/`; provenance binds each
file to the upstream Git blob and executable mode. Ignore, attributes and
submodule configuration files have inert `.upstream` suffixes without byte changes.

The upstream `LICENSE` is the Unlicense, while the original Cargo manifest
declares MIT. Both conflicting originals are preserved. The local package uses
the actual retained license file and does not claim that the metadata agrees.

The original Gitlink `examples/fish` names a Fish-shell checkout used only by
the optional upstream example test script. It is recorded, not materialized;
neither the local native build nor the retained authored grammar tests use it.

The stateless scanner is unchanged. `patches/final-statement.patch` corrects
the grammar's top-level final-statement rule: Fish permits token exhaustion
without a final newline, while header and body separators remain required.
Both authoritative TypeScript and emitted JavaScript inputs are patched.
`GENERATION.json` binds ABI14 tables regenerated once during preparation with
the pinned Tree-sitter CLI and its native JavaScript runtime.

The local LanguageFn binding builds only retained C11 parser/scanner inputs
using existing pinned build dependencies. It never fetches, generates tables,
executes Fish, or invokes upstream package scripts. Upstream Rust bindings
remain unchanged under `upstream/` as provenance.

The original parser's missing final separator is retained as a failing baseline.
The patched parser accepts the same original bytes; no source terminator is
injected or hidden. Boundary controls retain malformed block/header/body/pipe
failures and compare incremental with fresh scanner trees. Native syntax
availability does not prove command lookup, runtime expansion or complete Fish
semantic analysis.
