# Local Meson parser integration

All 33 regular Git blobs from staysail/tree-sitter-meson commit
1a497eecfb1b840ab12caf28f0ef45d4a5e26d28 remain byte-exact under `upstream/`.
`SOURCE-PROVENANCE.json` records the source pin; `UPSTREAM-INVENTORY.json`
records every original blob. Upstream Git attribute metadata is renamed and inert.

The active `grammar.js` derives from that retained input through
`patches/lists-and-iterables.patch`. It admits trailing commas in nonempty arrays,
dictionaries and function argument lists, and expression-valued foreach iterables.
A single trailing-comma boundary for mixed arguments does not admit empty or
repeated arguments. The hidden expression rule preserves direct bare-name iterable
children so lexical binding proofs can distinguish them from unevaluated values.

Quoted content, named escapes and closing delimiters use immediate tokens. Global
whitespace and comment extras cannot consume literal spaces or hashes inside
ordinary or formatted strings. Raw triple strings retain authored whitespace and
newlines, including embedded quote characters, without producing comment nodes.
Comment tokens stop before the newline so trailing comments cannot consume the
statement terminator; comment-only input also remains valid at end of file.

`GENERATION.json` binds the reviewed grammar and ABI14 parser, node metadata and
headers to the retained Tree-sitter CLI 0.26.8 maintainer tool. Generation occurs
only during source preparation. The active generated tables live under `src/`;
the original upstream tables remain unchanged for provenance and comparison.

The operational Rust wrapper uses the existing LanguageFn and cc dependencies.
The old upstream Rust binding and its Tree-sitter 0.20 requirement remain inert.
Compilation reads reviewed local C11 inputs only: no scanner, generator,
JavaScript runtime, upstream lifecycle scripts or network acquisition is required.

Recovery remains observable for malformed or unsupported syntax. Native parsing
does not prove project execution, dynamic name binding, dependency discovery or
complete structural and semantic Meson coverage.
