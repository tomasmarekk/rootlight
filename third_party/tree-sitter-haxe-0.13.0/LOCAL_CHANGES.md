# Local Haxe changes

The complete upstream mirror is retained with exact authored bytes. Only its
ignore/attribute filenames are made inert, as mapped by source provenance.

The active grammar adds compiler-flag conditions separately from macro
quotation, expression quotation/direct reification, and unbraced try/catch
expression support. patches/grammar.patch records the handwritten delta.
The pinned Tree-sitter generator produced the active ABI 15 C parser, JSON
node types, grammar JSON and headers; generated C was not hand edited.

The local edition 2024 Rust wrapper uses the existing LanguageFn ABI and cc
dependencies and compiles both active C files. It does not use the upstream
Cargo 0.0.1 dependency on an older Tree-sitter or the stale mirror parser.
Native CST preparation is separate from Rootlight structural/binding support.
