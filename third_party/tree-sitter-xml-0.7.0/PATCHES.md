# Retained XML and DTD source modifications

The upstream `tree-sitter-xml` 0.7.0 publication and the authored repository
revision `4b64dd3a03ec002258d6268d712fd93716d6ab57` remain unchanged under
`upstream/`. Both original archives, every original file, raw tar-member modes,
Git tree/blob identities and the separately authenticated same-revision MIT
notice are retained as inert provenance. No upstream lifecycle hook is invoked.

The original authored `.gitignore` and `.gitattributes` bytes are retained as
`upstream/authored-files/.gitignore.upstream` and
`upstream/authored-files/.gitattributes.upstream`. They cannot hide retained
inputs or override this repository's byte-preservation attributes. Original
archive members, Git identities and modes remain unchanged in the acquisition
inventory; the source inventory and provenance record the two retained-path
mappings. No native, grammar, scanner or Rust binding input is changed.

`patches/preferred-syntax.patch` reproduces exactly four reviewed overlays:

- `xml/grammar.js` retains physical end-tag shape without runtime tag-stack
  identity and accepts repeated CDATA scanner chunks in one source section.
- `common/common.mjs` expresses the exact XML 1.0 fifth-edition written Name
  and Nmtoken ranges and the reviewed PI/comment/CDATA grammar boundaries.
- `xml/src/scanner.c` uses stateless lexer scanning. It does not allocate or
  serialize a runtime tag stack, normalize names or validate opening/closing
  name equivalence. Its inherited erroneous-end-name ordinal remains dormant.
- `common/scanner.h` uses full lexer code points and exact terminators without
  locale classification or narrow-byte name identity. The unchanged DTD scanner
  also consumes this shared preferred header.

`dtd/grammar.js` and `dtd/src/scanner.c` remain literal upstream inputs. XML and
DTD parser tables, node schemas and parser support headers are the exact closed
preferred ABI14 canonical-generation outputs, not hand-edited generated code.
The declared Tree-sitter CLI 0.26.8 and Node 24.11.1 identities and the closed
generation receipt are recorded separately. The original generator dependency
metadata is retained; it is not evidence of the preferred generator's build
origin and is never used to download or generate code during compilation.

`patches/local-binding.patch` creates the three first-party package binding
files. The unpublished manifest pins `tree-sitter-language` 0.1.7 and `cc` 1.2.67
and declares Rust 1.90. The build script compiles only the two retained tables
and paired scanners. Distinct `LANGUAGE_XML` and `LANGUAGE_DTD` constants retain
distinct process-lifetime factory handles; neither establishes XML validity,
DTD/entity resolution, namespace equivalence, adapter qualification or MCP data.
The exact MIT notice is installed as `LICENSE` without changing its terms.

The closed native v3 evidence proves bounded written-syntax witnesses and
source-bound scanner execution. It does not prove product coverage, a relocated
Rust binding, offline empty-cache network denial, or complete clean-tree
snapshot emission. The independent audit records 617 non-trivia leaf-gap bytes
in parent ranges and two incremental comparisons whose clean snapshots were
not emitted. Adapter integration must retain those complete physical source
ranges and must compare written opening/closing XML names itself.

This package source-preparation metadata keeps materialization, publication,
relocated binding, release notice integration and installed MCP flags false.
Later operations must produce separate evidence, not rewrite historical claims.
