# Local static Scheme binding

This unpublished local package compiles the default reader from the complete
retained authored revision of `6cdh/tree-sitter-scheme`. All 104 authored files
remain unchanged under `upstream/`, including all six inert dialect selectors,
their notices, build metadata, instructions and test inputs. They are dependency
data, not instructions or activated build hooks for this project.

The revision is `1b112d9571e4f62fb3d095d52a51f1da7756fb94`, with Git tree
`b90f665139a09d1ce6ebd07c433049907b3a352c`. Its nominal manifest version is
`0.24.7-1`; this is not a claim that a crates.io distribution with that version
contains this revision. The local package is named
`tree-sitter-scheme-native-local` and is not published.

## Binding boundary

The local `Cargo.toml` and `build.rs` are the exact binding files used by the
isolated current and minimum Rust qualification consumer. The current `lib.rs`
is a comment-only derivative that describes product use and the static table
safety contract without implying per-compile source authentication. Its Rust
tokens remain unchanged; the original qualification pins and current derivative
are recorded separately in `SOURCE-PROVENANCE.json`. These are first-party
integration files, not replacements presented as upstream bytes. The authored
manifest and Rust binding remain intact under `upstream/`.

The local manifest pins the already retained `tree-sitter-language` 0.1.7 and
`cc` 1.2.67 sources. The C11 build compiles only `upstream/src/parser.c` and
includes its adjacent retained header. No JavaScript evaluation, grammar
generation, acquisition, npm lifecycle or dialect selection occurs during the
build. The original optimization-disabling parser pragmas remain unchanged.

`LANGUAGE_SCHEME` exposes the default-root ABI-15 table. Its parser has no
external scanner. The retained CHICKEN scanner belongs to a distinct inert
dialect and is not linked. Reader parsing does not prove standards validity,
case-folded or escape-decoded identity, macro expansion, library resolution,
runtime call targets or complete language semantics.

## Provenance and notices

`upstream/archives/` retains the original authored archive. The two original
Git metadata responses under `upstream/metadata/` associate all authored file
bytes with the recorded commit and tree; this is not signed provenance.
`SOURCE-INVENTORY.json` records the complete local payload and original archive
members. `SOURCE-PROVENANCE.json` records origin and qualification boundaries.

The exact primary MIT notice remains `upstream/LICENSE`, attributed to 6cdh.
`notices/tree-sitter-runtime-LICENSE` retains the MIT notice associated with
the referenced Tree-sitter runtime and generated support headers. Retaining
sources and notices does not establish first-party authorship or remove
supply-chain risks.

The local `upstream/archives/.gitattributes` storage control disables text
filtering for retained archives. It overrides the authored upstream text policy
without modifying that policy or any of the 104 authored files. The control is
first-party storage metadata, not an upstream authored source or build hook.

The isolated native evidence covers nine unchanged written-reader groups and
29 source-conservation cases on Rust 1.97.1 and 1.90.0, plus scoped Clippy.
Publication at this new path, product integration, corpus coverage, installed
MCP, performance, empty dependency caches and independently denied networking
remain separate qualifications.
