# Local Notation3 derivation

This package retains the complete MIT source tree at upstream commit
`62816526edba86e8c899919f75fbf1101311b6a2`. The original grammar, manifests,
bindings, tests, notices and archive remain inert under `upstream/`.

The original package has no generated C parser. Its Node manifest declares
version `0.1.0`; its retained Rust manifest declares `0.0.1`, edition `2025` and
Tree-sitter `0.21`. The local static Rust binding uses the Node grammar version,
stable edition 2024, and the repository's retained Tree-sitter runtime and C
compiler dependencies. No upstream lifecycle script is executed.

## Preferred grammar

The active `grammar.js` adds the legacy `@forAll` and `@forSome` directives to
the existing N3 directive alternatives. Each admits the legacy comma-separated
IRI or prefixed-name symbol list. Empty lists remain valid, and the shared
statement production admits the directives inside formulas. These are syntax
rules; they do not evaluate quantifiers or perform logical inference.

The syntax follows the universal, existential and symbol-list productions in
the [W3C legacy grammar report](https://www.w3.org/2000/10/swap/grammar/n3-report.html).
Native tests keep malformed comma lists, literal targets and incorrect directive
casing as recovery cases. Three untouched original EYE files independently
exercise universal, existential, comma-list and nested-formula syntax.

## Generation and compilation

Generation is a separate maintainer preparation step using the already retained
Tree-sitter CLI `0.26.8` with its bundled native QuickJS runtime:

```text
tree-sitter generate --js-runtime native --abi 15 grammar.js
```

`SOURCE-PROVENANCE.json` records the exact tool, preferred grammar and generated
output hashes. The build script compiles only the retained local C parser and
headers; it performs no acquisition, generation, package-manager invocation or
network access. Parser loading and syntax tests do not qualify production
discovery, structural IR, semantic bindings or corpus-wide N3 coverage.
