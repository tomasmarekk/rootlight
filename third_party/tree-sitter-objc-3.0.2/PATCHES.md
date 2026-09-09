# Objective-C grammar provenance and changes

Base: crates.io `tree-sitter-objc` 3.0.2, SHA-256
`9ca8bb556423fc176f0535e79d525f783a6684d3c9da81bf9d905303c129e1d2`.
The package VCS revision and upstream tag `v3.0.2` both identify
`18802acf31d0b5c1c1d50bdbc9eb0e1636cab9ed` in
<https://github.com/tree-sitter-grammars/tree-sitter-objc>.

The published package omitted its MIT notice and grammar DSL. Both are restored
from that exact revision. The inherited C DSL and its MIT notice come from npm
`tree-sitter-c` 0.23.4, with SHA-512 integrity
`hp3xYuWbuTBanHEwrAxOBhDjdwiD1k3u2XpVmpFk5GdJJj7N2jrcF45hYrZPcwuAjNXdL01YFG7TSLdmPi2lyg==`.
The inherited DSL is retained under `grammar/c` so generation needs no downloads.
The package include list retains these sources and notices.

## Message selectors

The upstream message rule requires a named first selector component and accepts
multiple adjacent argumentless identifiers. Objective-C permits empty selector
components, including the first component, but every argument needs a colon and
an expression. The rule now distinguishes one argumentless selector from a
sequence of optional labels followed by mandatory colon/argument pairs.
It retains existing `method` and `receiver` fields and does not evaluate dispatch.

Regression fixtures are in `tests/fixtures/objective-c` and qualification tests
in `tests/native-grammars/tests/objective_c_syntax.rs`. These tests do not prove
complete language analysis or Objective-C++ support.

## Regeneration

Run `tree-sitter generate --abi 14` from this directory with Tree-sitter CLI
0.26.8 and Node.js. The generated parser, node types, grammar JSON and C headers
are outputs, not hand-edited sources. No external scanner is used. The generator
upgrade adds an empty `reserved` map to the normalized grammar JSON; the original
unmodified DSL otherwise reproduces the published grammar rules.
