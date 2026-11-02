# Vento source provenance

The native grammar is retained from `https://github.com/ventojs/tree-sitter-vento`
at commit `4569bc1fb81f050ca4ef3ceefdd31cfeb35c4f0a`, Git tree
`f514bb07b0629025347d5bc1d77b180eeac3cb4d`.
`upstream/source.tar` is a locally exported Git archive, not an upstream release.
It retains all 57 original regular files and their non-executable file intent,
including the unchanged bindings, manifests, notices, tests and ignore metadata.
The archive is 1,085,440 bytes with SHA-256
`46a007db268b66f380e4069ca8f239010d144681f21cc944800a88fa928456a3`.
Every archived file was compared with the acquired Git blob before retention.

The root grammar, generated parser/JSON and three paired headers are exact copies.
The ABI15 parser has not been regenerated. The upstream generator dependency says
Tree-sitter 0.25.8, but reproducible generation is not established by this intake.
The local scanner differs only by the retained quoted-boundary patch. Quoted
pipeline and host delimiters stay within JavaScript strings; template interpolation
uses the scanner's existing balanced-object handling. This boundary improvement
does not implement JavaScript validation, comments, regular expressions or runtime
bindings. Scanner cancellation, adversarial work and stack bounds remain unqualified.

The complete upstream MIT license is copied unchanged as `LICENSE`, SHA-256
`bc1a60f93a11aa9a085c8c1bb08716a0feed91654b7886f2ff6b7bdafb4a3377`.
It names Matthew Taylor, 2024. The original Rust binding's inherited 2020
tree-sitter-embedded-template notice is retained in the archive as authored.
The local wrapper does not claim authorship of the upstream grammar or scanner.

The upstream Rust manifest has invalid trailing punctuation, its binding exports
the unrelated embedded-template factory, and its build omits the required scanner.
The local manifest and LanguageFn binding instead compile both retained native C
owners and use `tree_sitter_vento`. The version 0.21.0 follows upstream Cargo
metadata; upstream Tree-sitter metadata separately says 0.1.0. No published Rust
package identity is claimed. Optional Node/NAN bindings and lifecycle commands
remain inert archive contents and never run during the local build.

Builds use only the retained parser, scanner, paired local headers and pinned
vendored Rust dependencies. Standard C headers and declared toolchains remain
environment prerequisites. No build downloads or invokes a grammar generator.
The standalone test target reuses the existing native-grammar suite's Vento file
to permit focused compiler feedback without building unrelated grammars. Its
original 52-byte Deno fixture is source data, not rendered output or service
qualification. Discovery, structural lowering and corpus indexing are separate.
