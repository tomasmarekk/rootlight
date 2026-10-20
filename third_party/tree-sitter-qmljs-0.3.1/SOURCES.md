# Pinned QML grammar sources

The complete MIT yuja/tree-sitter-qmljs 0.3.1 source archive is retained at commit
de96ed62abded51fcdfcbeaaa120e0dd0d20c697. Every original regular file is retained
byte for byte; upstream ignore and attribute files are inert .upstream data.
The original Cargo manifest, generated ABI14 parser, external scanner and Rust
LanguageFn wrapper remain unchanged.

The bundled TypeScript 0.23.2 scanner header is a separate MIT-authored source
with its own preserved Max Brunsfeld notice. Its ten external tokens match this
QML parser. The separately maintained TypeScript scanner is not substituted.
Compilation uses complete local C11 inputs and existing local cc and
tree-sitter-language sources without acquisition or generation. Dedicated native
tests use the existing exact Tree-sitter 0.26.11 test workspace; the excluded
grammar's upstream dev-only dependency is not an additional build input.

Optional npm, Python, Swift and grammar-generation workflows are retained inert
upstream data. Their separate toolchain dependency closure is not claimed.
Native syntax parsing does not qualify QML definitions, source encoding maps,
semantic binding, whole-language coverage or installed behavior.
