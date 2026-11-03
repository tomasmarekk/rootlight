# VB.NET grammar provenance

Retained native sources come from `https://github.com/repowise-dev/tree-sitter-vb-dotnet`
at commit `4e002ba8c6438842e18fa067b6d592885c13e8f9`, Git tree `e1624e2998b26a8568492f33cb30a479e9dd45c1`. This fork retains
CodeAnt AI attribution and adds the complete upstream MIT notice. The exact
47 regular files (24,256,546 source bytes) are retained in the locally
exported `upstream/source.tar`; it is not a published Rust crate. Every archive
member was compared with its acquired Git blob and has non-executable file intent.
The archive is 24,309,760 bytes, SHA-256 `1c452d2aeead2ce09bf39dab1bf5d0aa281d3da0c1860bf09937c3b2cd7a7543`.

`GRAMMAR.patch` records preferred-source changes relative to that exact archive:
external `Declare` member signatures with library, entry-point, character-set and
parameter fields, plus shared keyword terminals for ordinary/external modifiers.
External declarations are admitted in class/module/structure member lists, without
inventing DLL, ABI or marshaling bindings. Original upstream C/JSON and paired
headers, generated with Tree-sitter 0.25.8, remain unchanged in the archive.

The preferred ABI15 C/JSON and paired headers were generated separately using the
declared Tree-sitter CLI 0.26.8 prerequisite and
`generate --js-runtime native --abi 15`. Executable SHA-256 is
`a61249a07c4a6d01d0bbbb5b7c058e29e12c119ef7fb5bede0b7fd916a7c56f0`.
Two successful invocations with identical preferred inputs produced byte-identical
outputs. Parser SHA-256 is
`4275d95f4ab7a55ce1064bae8648bdd203961b7de5de1ba02b7ae647ee58ad92`.
No generated C table was hand-edited. `LICENSE.TREE-SITTER` retains the generator
and paired-header project's complete MIT notice. Upstream grammar MIT notice
SHA-256 remains `b94d0fbc2f7ff9e8e747c8aa01253453eddc0be7ab23f4b403e012aec234783d`.
No external scanner is used.

The local unpublished Rust wrapper compiles only the retained C11 parser with
pinned versioned Rust inputs. Standard headers and declared compilers remain
environment prerequisites. Compilation runs no downloads, npm lifecycle or grammar
generation. The local package name does not claim upstream Rust publication.

Native syntax tests use complete licensed TaskbarX, MSAA and TaskbarStyle originals
with exact physical bytes. They establish only the tested structure and spans.
Other original recovery cases remain open. Whole VB.NET semantics, complete
corpus support, reproducible generation and adversarial parser work bounds remain
unqualified.
