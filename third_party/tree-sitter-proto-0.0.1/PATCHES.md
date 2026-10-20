# Local protobuf grammar repair

The local `extend` rule accepts one optional leading dot before its existing
`full_ident` child. Proto2 extend statements consume message types, which permit
absolute names. The global `full_ident` rule stays unchanged, so packages and
ordinary names do not gain an invalid leading-dot form. Existing relative and
nested extension syntax retains the same named CST shape.

The original archive, complete upstream mirror and MIT license remain unchanged.
`patches/absolute-extendee.patch` records the sole handwritten grammar delta.
The parser, grammar JSON, node types and all three support headers were generated
by Tree-sitter CLI 0.26.8 with explicit ABI14 and Node 26.2.0. Generated C was not
edited manually. The Rust build script still compiles only retained local inputs;
no parser generation or dependency acquisition runs during product compilation.

Reproduce generation in this package with a declared Tree-sitter CLI 0.26.8 and
Node runtime:

```text
tree-sitter generate --abi 14 --js-runtime node grammar.js
tree-sitter build --output ../proto.dll .
tree-sitter test --lib-path ../proto.dll --lang-name proto --file-name absolute_extendee.txt
```

Six authored corpus cases cover absolute enum-value, field/message and
service/method/enum option extendees, ordinary relative qualified/unqualified
names and a nested absolute extension. Three files in `test/rejected` must still
produce nonzero parse exits: an absolute package name, a missing extendee and a
doubled leading dot. They are negative grammar controls, not accepted programs.

Fifteen inherited cases remain byte-exact under `upstream/corpus`. To run them
with the current CLI layout, copy those eight text files temporarily beneath
`test/corpus/retained-upstream`, run each case using `--file-name` and `--include`
in a fresh CLI process, then remove only those temporary copies. The recorded
qualification uses this isolated-case form because a combined baseline run gave
inconsistent failure accounting. All 21 isolated cases passed with the repaired
parser; four absolute-extension cases failed with the original parser. The
original three development corpus files likewise changed from parser ERROR
nodes to error-free complete native parses without source preprocessing.

These are native grammar checks. Product lowering, extension binding, descriptor
resolution, option meaning, schema/wire validation and complete language support
are not qualified by this repair. Existing explicit semantic gaps must remain.
