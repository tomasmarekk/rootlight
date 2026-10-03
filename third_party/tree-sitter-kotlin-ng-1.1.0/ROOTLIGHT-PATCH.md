# Kotlin final class-member terminator patch

This is the Rust build payload and preferred `grammar.js` from the MIT-licensed
`tree-sitter-kotlin-ng` 1.1.0 crates.io archive, SHA-256
`e800ebbda938acfbf224f4d2c34947a31994b1295ee6e819b65226c7b51b4450`.
Its published VCS metadata names upstream commit
`77dd60ea0a9003ce062c9728a513ffe1aaff8c82`. The exact original archive is retained
at `upstream/tree-sitter-kotlin-ng-1.1.0.crate`; all 16 regular members were
matched to authenticated local registry source bytes. Registry checksum and
package lock bookkeeping are omitted from the compilation tree. The original
scanner is retained as `src/scanner.c.orig`. `SOURCE-PROVENANCE.json` records all
original member hashes and the bounded local override.

The archive omits its MIT license. `LICENSE` is an unchanged copy of Rootlight's
retained exact-tag license notice, whose existing evidence remains in
`adapters/grammars.lock`. This subset is not the full upstream repository or its
fixture collection, and no signed Git-object authentication is claimed here.

The upstream grammar requires an internal `_class_member_semi` after every
class member, but its scanner rejects a same-line closing brace. Kotlin's
[class-member grammar](https://kotlinlang.org/spec/syntax-and-grammar.html)
makes written `semis` optional after a member. The local scanner supplies the
required zero-width internal terminator before `}` only when
`CLASS_MEMBER_SEMI` is valid and ordinary `SEMI` is not valid. The pinned parser
uses both flags together only in its all-symbol recovery state, so that state
and ordinary statement terminators are not widened. The original brace is not
consumed; the token end remains before skipped trivia.

No other scanner branch, allocation, serialized state, grammar, generated
parser table, resource budget or binding/build script changes. Compilation
uses retained `src/parser.c`, `src/scanner.c` and native headers with the
versioned `cc` and `tree-sitter-language` sources; it downloads and generates
nothing. Authored native terminator/source-span/recovery/reset regressions do
not establish compiler acceptance, exhaustive Kotlin conformance, preferred
producer closure for the complete product, or an OS-denied build. Remove this
override when a pinned released scanner passes these exact boundary controls.
