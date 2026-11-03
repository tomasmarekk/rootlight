# VB.NET grammar provenance

Retained native sources come from `https://github.com/repowise-dev/tree-sitter-vb-dotnet`
at commit `4e002ba8c6438842e18fa067b6d592885c13e8f9`, Git tree `e1624e2998b26a8568492f33cb30a479e9dd45c1`. This fork retains
CodeAnt AI attribution and adds the complete upstream MIT notice. The exact
47 regular files (24,256,546 source bytes) are retained in the locally
exported `upstream/source.tar`; it is not a published Rust crate. Every archive
member was compared with its acquired Git blob and has non-executable file intent.
The archive is 24,309,760 bytes, SHA-256 `1c452d2aeead2ce09bf39dab1bf5d0aa281d3da0c1860bf09937c3b2cd7a7543`.

The grammar, generated C/JSON, three paired headers, package metadata and MIT
license are copied unchanged. The ABI15 parser was generated upstream using
Tree-sitter 0.25.8; no generator runs during this build. Parser SHA-256 is
`500a1a0f01d0a4f7ee0bcab589c8af0f8ea0e74db22763e30667b7cad9d690c3`; MIT notice SHA-256 is
`b94d0fbc2f7ff9e8e747c8aa01253453eddc0be7ab23f4b403e012aec234783d`. No external scanner is used.

The local unpublished Rust wrapper compiles only the retained C11 parser with
pinned versioned Rust inputs. Standard headers and declared compilers remain
environment prerequisites. Compilation runs no downloads, npm lifecycle or grammar
generation. The local package name does not claim upstream Rust publication.

Native syntax tests use an exact BOM/CRLF original TaskbarX source. They establish
only the tested native structure and source spans. Whole VB.NET semantics, complete
corpus support, reproducible generation and adversarial parser work bounds remain
unqualified.
