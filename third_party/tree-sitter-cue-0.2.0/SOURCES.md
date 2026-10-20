# CUE native sources

The complete cue-lang/tree-sitter-cue v0.2.0 release is retained in `upstream/`
and `upstream.tar.gz`, pinned to ba4356732fcf8b203f7f98cfb6e70390d5dc1da8.
The generated C parser, scanner and headers are unchanged copies. Their MIT
license and authorship remain intact. Original executable intent is recorded in
SOURCE-PROVENANCE.json rather than inferred from Windows file modes.

The local Rust LanguageFn binding and C11 build wrapper replace the old Rust API;
they perform no generation, dependency acquisition or lifecycle-script execution.
The retained upstream package scripts and instruction documents are inert data.
The source-backed CUE fixture has its own retained MIT notice and provenance.
Native parsing alone does not establish full CUE semantic support.
