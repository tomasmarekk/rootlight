# Native Nix grammar provenance

This directory contains the MIT-licensed published `tree-sitter-nix` 0.3.0 crate
from `nix-community/tree-sitter-nix`, revision
`ea1d87f7996be1329ef6555dcacfa63a69bd55c6`. The original crate SHA-256 is
`4952a9733f3a98f6683a0ccd1035d84ab7a52f7e84eeed58548d86765ad92de3`.

## Empty inheritance

The `inherit` and `inherit_from` productions permit an omitted attribute list.
Nix accepts `inherit;` and `inherit (expression);`; the published grammar instead
inserts a missing identifier, which can become a fabricated binding downstream.
The named `inherited_attrs` node remains nonempty whenever present.

This matches the empty `attrs` production in the
[Nix parser](https://github.com/NixOS/nix/blob/master/src/libexpr/parser.y)
and addresses the behavior discussed in
[upstream pull request 162](https://github.com/nix-community/tree-sitter-nix/pull/162).
The isolated native tests compare these cases with RNix and retain malformed
expression checks. No Nix expression is evaluated during qualification.

Generated artifacts are rebuilt from `grammar.js` using Tree-sitter CLI 0.26.8
with `tree-sitter generate --abi 14`. The published parser used ABI 13, which
this generator no longer emits; ABI 14 is already supported by the pinned runtime.
The stateless external scanner is unchanged.
No runtime limits or scanner serialization contracts are relaxed.

## Import formatting

Four trailing spaces in the entirely commented `queries/locals.scm` are removed
for the repository whitespace gate. No query text or behavior changes.
