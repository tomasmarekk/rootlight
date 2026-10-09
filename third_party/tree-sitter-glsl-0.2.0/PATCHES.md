# Retained GLSL source and local changes

This is a local unpublished static binding for tree-sitter-glsl 0.2.0. The
published source archive, the complete authored GLSL revision
`a6a297b59bd615589d55f4529313dc2c87e815b8`, and the complete authored C parent
revision `2a265d69a4caf57108a73ad2ed1e6922dd2f998c` remain separately retained.
The C parent declares 0.23.5 in authored metadata; no registry publication or
registry archive for that version is asserted. Both original MIT notices and
the exact authored copyright spellings remain unchanged.

## Preferred grammar repair

`patches/precision-declaration.patch` adds only a standalone `precision`
declaration alternative with written precision and type fields and no variable
declarator. Existing declarations, qualifiers, conflicts, extras, scanners and
`discard` representation remain unchanged. The precision alternatives are
`highp`, `mediump` and `lowp`; primitive `int` and `float` are preserved as
primitive types, and raw opaque-type spellings use the inherited type identifier
shape. This is structural representation, not semantic type-eligibility or
precision-propagation validation.

The language basis is [GLSL 4.60 default precision qualifiers](https://registry.khronos.org/OpenGL/specs/gl/GLSLangSpec.4.60.html#default-precision-qualifiers).
Variable precision prefixes are a separate, unchanged grammar case.

## Generated and compiled inputs

The six retained outputs were generated twice from the exact repaired GLSL
grammar and exact C parent with Tree-sitter CLI 0.26.8, Node 24.11.1 and ABI14.
Both producers and both native lexical guards exited zero, with all six output
pairs literally equal. Generation used a detached four-input projection:
`grammar.js`, `LICENSE`, `node_modules/tree-sitter-c/grammar.js`, and its
`LICENSE`. These are copies of retained inputs, not package installation.
The full generation parent remains inert under `upstream/c-authored`.

Compilation uses only `src/parser.c` and the retained `src/tree_sitter/parser.h`
plus ordinary declared compiler standard headers. There is no external scanner,
build-time source acquisition, generation, npm lifecycle, submodule action or
upstream build hook. The local binding uses the exact existing
tree-sitter-language 0.1.7 and cc 1.2.67 inputs. `patches/local-binding.patch`
records changes against the authored Rust binding; the separately derived
`patches/qualified-binding-layout.patch` records the relocated build and factory
contract against the qualified isolated wrapper.

## Evidence boundary

The exact generated table passed six unchanged native fixture groups and eight
full-source observations on Rust 1.97.1 and 1.90.0, and scoped all-target/all-feature
Clippy with warnings denied. The original failed precision receipt remains
preserved. This is evidence for the isolated table/wrapper, not execution of the
relocated package binding. The relocated consumer and SDK/MCP integration must
be independently verified. `discard` remains an identifier-shaped expression
boundary; linked interfaces, preprocessing selection, semantic precision and
shader-profile validity are not established by these syntax tests. No
whole-language percentage or installed-product acceptance follows from them.
