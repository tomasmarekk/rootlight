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

Inline escaped mustache starts are literal tokens, including escaped block
headers and closers. A single adjacent backslash escapes the start; two or more
backslashes leave the following mustache active, matching the pinned official
lexer reference in `SOURCE-PROVENANCE.json`. The same boundary applies inside
quoted attribute text. No template evaluation runs during indexing.

Contiguous `@data` paths, including parent-depth forms, have a distinct token
without identifier children. Query captures retain unavailable runtime reads;
their data names do not participate in lexical block-parameter binding. This
does not add ordinary relative paths or evaluate runtime data values.

The grammar does not verify matching block names or template runtime behavior.
The adapter separately validates block names and scopes before admitting block
parameters and lexical reads. Helper dispatch, property identity, inverse
branches and host-language semantics remain unavailable.
