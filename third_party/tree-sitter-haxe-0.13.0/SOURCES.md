# Haxe grammar source inputs

Upstream: https://github.com/vantreeseba/tree-sitter-haxe, commit
a25faf5efa842502a326ec803cfbe5a442a11e7c, Git tree
df0cbe98980c3e6f64576dcc6a7c2db47c03661e. The complete 88 authenticated
Git blobs, original MIT notice and source archive are retained under upstream/.
Source provenance maps original paths and executable intent to retained files.
Ignore/attribute files keep exact bytes with inert .upstream names; the local
.gitattributes preserves physical bytes of all vendor inputs.

Local version 0.13.0 records the upstream npm lineage. Upstream Cargo declares
0.0.1, and the selected commit is later than tag v0.13.0. This is not represented
as a published upstream Cargo 0.13.0 release or an exact tag snapshot.

The active root grammar and generated src/ contain the reviewed macro
expression/reification, compiler-condition and try-expression corrections in
patches/grammar.patch. patches/generation.json pins the complete active input
set and generator prerequisites. Generated C was not hand edited. The scanner
and all original licenses remain unchanged. LOCAL_CHANGES.md lists local work.

Cargo compiles active src/parser.c and src/scanner.c with active headers using
the declared C11 toolchain and existing locally retained cc/language bindings.
Compilation never invokes generation, Node, npm, upstream scripts or network
acquisition. The complete sources are inputs, not dependency-cache evidence.

This source preparation does not establish full Haxe semantics, macro
evaluation, empty-cache/network-blocked product acceptance or an installed
package qualification. Source-defined owners and bounded native local Reads
require their separate adapter/public tests and exact release qualification.
