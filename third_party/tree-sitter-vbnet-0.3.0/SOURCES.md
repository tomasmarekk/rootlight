# VB.NET grammar provenance

Retained native sources come from `https://github.com/repowise-dev/tree-sitter-vb-dotnet`
at commit `4e002ba8c6438842e18fa067b6d592885c13e8f9`, Git tree `e1624e2998b26a8568492f33cb30a479e9dd45c1`. This fork retains
CodeAnt AI attribution and adds the complete upstream MIT notice. The exact
47 regular files (24,256,546 source bytes) are retained in the locally
exported `upstream/source.tar`; it is not a published Rust crate. Every archive
member was compared with its acquired Git blob and has non-executable file intent.
The archive is 24,309,760 bytes, SHA-256 `1c452d2aeead2ce09bf39dab1bf5d0aa281d3da0c1860bf09937c3b2cd7a7543`.

`GRAMMAR.patch` records preferred-source changes relative to that exact archive:
external `Declare` signatures, shared keyword terminals, standalone `End` statements,
and targetless attributes on ordinary class/structure declarations. Global Assembly
and Module targets remain file-level attributes. Return attributes are confined to
Function, Declare Function, Delegate Function and Property signatures; Sub variants
and ordinary type uses do not admit them. The Property `as_clause` node shape is
preserved. Ordinary declaration-prefix and return attribute lists may consume
physical newlines, never a colon terminator. Explicit file-level Assembly/Module
attribute statements retain their required newline or colon separators, with a final
global attribute block also admitted at end of file. File/namespace blank lines use
physical newlines, so a colon cannot bypass ordinary prefix placement. These syntax
rules do not infer runtime termination, attribute, DLL, ABI or marshaling bindings. Original
upstream C/JSON and paired headers generated with Tree-sitter 0.25.8 remain unchanged
in the archive.

The preferred ABI15 C/JSON and paired headers were generated separately using the
declared Tree-sitter CLI 0.26.8 prerequisite and
`generate --js-runtime native --abi 15`. Executable SHA-256 is
`a61249a07c4a6d01d0bbbb5b7c058e29e12c119ef7fb5bede0b7fd916a7c56f0`.
The preceding standalone-End/Structure revision, parser SHA-256
`5778ec353255c5fcc4035b10c3f02cb5456805b9c5aad0edc5493b0a98668c83`,
had byte-identical outputs from two invocations. The current attribute revision has
one completed generation; repeated-generation equivalence has not been measured.
Current parser SHA-256 is
`b5ef8f3545cd756754f30d1fa4b075baf89622c4d398eb8a2f525fbb4e09dea4`.
No generated C table was hand-edited. `LICENSE.TREE-SITTER` retains the generator
and paired-header project's complete MIT notice. Upstream grammar MIT notice
SHA-256 remains `b94d0fbc2f7ff9e8e747c8aa01253453eddc0be7ab23f4b403e012aec234783d`.
No external scanner is used.

The local unpublished Rust wrapper compiles only the retained C11 parser with
pinned versioned Rust inputs. Standard headers and declared compilers remain
environment prerequisites. Compilation runs no downloads, npm lifecycle or grammar
generation. The local package name does not claim upstream Rust publication.

Native syntax tests retain complete licensed TaskbarX, MSAA, TaskbarStyle and
MainWindow originals with exact physical bytes. Controls cover native declarations,
ordinary class/structure attributes, signature-specific return attributes, Property
node compatibility, legal global colon statements, rejected ordinary/return colons,
standalone statements, compound terminators and invalid placement.
Complete syntax trees, including manual original-source diagnostics, do not establish
complete declaration lowering, attribute binding or body semantics. Whole VB.NET
semantics, complete corpus support, independent generation reproducibility and
adversarial parser work bounds remain unqualified.
