# ABAP structure grammar provenance

The native grammar is retained from `https://github.com/zooloo303/tree-sitter-abap`
at commit `14db66a2f6c45b1992d5e252148a7d5a05724d86`, Git tree
`9bb3ac02a4d619825b52851a69dc63d29f317f25`.
`upstream/source.tar` is a locally exported Git archive, not an upstream release.
It retains all 20 original regular files, 432,568 source bytes, with their
non-executable file intent. This includes the upstream manifests, notices,
examples, tests and ignore metadata. Every archived file was compared with its
acquired Git blob. The archive is 460,800 bytes, SHA-256
`4a08ba7a4c3762a9bda5a30e914ab6a0cda7d5c03a4b37e0249a39efbcd1a542`.

The grammar, generated parser/JSON, scanner and three paired headers are copied
unchanged. The ABI15 parser was generated upstream by Tree-sitter 0.25.10 and has
not been regenerated locally. Its parser SHA-256 is
`9a0a63e49ad1d51f22ff77aab77736fb715f9fb844e032f5b32ad0245a78219b`;
the scanner SHA-256 is
`521500b65097deab41f8a29e387296e245f9ffa1167ad593105b8a75b454a087`.
Reproducible generation, adversarial work bounds and scanner cancellation are
not established by this intake.

The complete upstream MIT license is copied unchanged as `LICENSE`, SHA-256
`75834a75ef078ab7599a27be9d0bb0aff49386b7a97ea4f91f2698d2897d3959`.
It names Neil Ward, 2026. The local unpublished Rust manifest and static factory
binding do not claim authorship of the upstream grammar or a published Rust
package identity. Version 0.1.0 follows the upstream package metadata.

The local wrapper compiles both retained C owners using the actual
`tree_sitter_abap` factory, local paired headers and pinned vendored Rust inputs.
Standard C headers and declared toolchains remain environment prerequisites.
No build downloads sources, runs upstream lifecycle scripts or invokes a grammar
generator. The standalone test target reuses the existing native-grammar suite's
test file for focused feedback without compiling unrelated grammars.

This is an incremental structure parser. Upstream deliberately accepts unmodeled
statements as generic token sequences; a clean tree does not prove their syntax,
expression semantics, references or bindings. Method parameter clauses and many
declaration details remain untyped. Native owner/name tests do not qualify ABAP
semantic indexing, service integration or the finite corpus coverage goal.
