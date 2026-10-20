# Retained Pony syntax inputs

The upstream source is `tree-sitter-grammars/tree-sitter-pony`, tag `v1.0.0`,
revision `7abc589f390d3d5f5de406ded2ce68d926a532cc`. The complete archive and
all 45 authored regular files are retained under `upstream/`. Provenance joins
their bytes and executable modes to the upstream Git blobs. Ignore and
attributes files have inert `.upstream` suffixes with unchanged bytes.

Both original Cargo and npm manifests declare `0.0.3`, despite the `v1.0.0`
tag. The local unpublished binding uses the manifest version and records the
tag separately; it does not present this archive as a published 1.0.0 crate.
The MIT license and original authorship notices are preserved unchanged.

The generated ABI14 parser, header, grammar and node metadata are unchanged.
`patches/empty-state-reset.patch` resets the scanner's quote counter before
deserialization. The original leaves a prior nonzero counter after an empty
restore. Native controls retain that failing callback case, ordinary and
integer-boundary roundtrips, and incremental versus fresh parser agreement.

The local LanguageFn binding compiles only retained C11 parser/scanner inputs
with existing pinned build dependencies. It never generates tables, acquires
sources or runs npm scripts. Upstream's optional example test script clones
Pony compiler examples; those checkouts are not native build inputs and that
script is not executed by the local build.

Written type and direct member identity do not establish receiver resolution,
reference-capability checking, dispatch, imported types or complete semantics.
