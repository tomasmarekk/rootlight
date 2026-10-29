# Retained template grammar

The complete authored source archive and its 86 file identities are retained in
`upstream/` and `SOURCE-INVENTORY.json`. Upstream lifecycle scripts are inert.
The local Rust binding compiles only the retained native parser and scanner.

The preferred grammar adds opaque XML processing instructions and simple
DOCTYPE declarations for written templates. Internal DTD subsets remain
unsupported. It excludes `~` from identifiers so whitespace-strip delimiters
cannot become part of a lexical name. `patches/written-prologue.patch` records
these preferred-source changes. ABI15 outputs are generated with the pinned
Tree-sitter CLI recorded in `SOURCE-PROVENANCE.json`; builds never run it.

The grammar does not verify matching block names or template runtime behavior.
The adapter separately validates block names and scopes before admitting block
parameters and lexical reads. Helper dispatch, property identity, inverse
branches and host-language semantics remain unavailable.
