# PowerShell source syntax corrections

This is the Rust build payload of the MIT-licensed `tree-sitter-powershell`
0.26.4 crates.io archive, SHA-256
`3faf304d44b9ddd4a7d97804bb8de7daf564336dd5a526dc6de5b39238243022`.
The package records upstream commit `d398441825243b00e317e87e1829b9d6a3e54ce0`;
the release tag points to a different commit, as recorded in `adapters/grammars.lock`.
Registry bookkeeping and the upstream development lockfile are omitted.
Bindings, build script, scanner, headers, queries and license retain published bytes.

`grammar.js` permits an absent body in `script_block_expression`. Empty blocks
are valid expression values and command arguments, including whitespace-only
and comment-only bodies. Delimiters and the existing nonempty body grammar
remain required where applicable; incomplete blocks still report syntax errors.
This changes syntax acceptance, not runtime execution or semantic resolution.

Numeric tokens accept either case for the hexadecimal prefix, decimal and long
suffixes, exponent marker and byte-size multipliers. Their existing lexical
priority is retained so invalid suffix combinations remain barewords rather than
being split into a valid numeric prefix and another token. This does not implement
new numeric types or resolve the upstream ambiguity between compact numeric
range/member expressions and command names.

Method invocations accept one adjacent bare script-block argument after any
member name, for instance and static calls. The block retains the ordinary
`script_block_expression` shape and the member is directly owned by
`invokation_expression`, so source queries retain the written call name.
This replaces the special `invokation_foreach_expression` wrapper and rejects
whitespace before its bare block, matching the PowerShell parser. It does not
evaluate computed member names or resolve runtime method dispatch.

The published `src/parser.c`, `src/grammar.json`, `src/node-types.json` and parser
header reproduce byte-for-byte using Tree-sitter CLI 0.26.8 with ABI 15.
Regenerate the patched outputs from this directory with the same command:

```sh
tree-sitter generate --abi 15
```

Generated sources are never hand-edited. Native tests cover exact source ranges,
command and expression contexts, trivia, malformed input and incremental edits.
The production adapter consumes the same pinned grammar. This is not a claim of
complete PowerShell coverage. Remove the override when a released grammar passes
these regressions and its source provenance and ABI are qualified again.
