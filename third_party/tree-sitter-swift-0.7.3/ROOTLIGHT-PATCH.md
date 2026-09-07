# Swift scanner state patch

This is the Rust build payload of the MIT-licensed `tree-sitter-swift` 0.7.3
crates.io archive, checksum
`fe36052155b9dd69ca82b3b8f1b4ccfb2d867125ac1a4db1dd7331829242668c`.
The upstream release commit is `b8b22bffbb3441780e6471665bacfb263741c86a`.
Cargo's generated manifest, Rust bindings/build script, queries, generated
parser, node types and headers are retained. Incidental `node_modules` license
files and registry bookkeeping are not build inputs and are omitted.

The only native change clears scanner state before deserialization and converts
each serialized byte through `uint8_t` before widening. Otherwise empty state
retains a previous raw-string delimiter count, and signed-char targets corrupt
counts whose serialized low bytes have the high bit set. No grammar, parser,
resource budget or compiler-wide character mode is changed.

The state contract is described in the upstream
[external scanner documentation](https://tree-sitter.github.io/tree-sitter/creating-parsers/4-external-scanners.html).
The affected implementation is in the
[release scanner](https://github.com/alex-pinkus/tree-sitter-swift/blob/b8b22bffbb3441780e6471665bacfb263741c86a/src/scanner.c).
Remove the override when a released grammar preserves all four serialized byte
positions and resets empty state, with Rootlight's regression tests passing.

Upstream publishes generated ABI-15 parser files using an unpinned generator and
`cargo publish --allow-dirty`. These generated files are pinned to the published
archive; exact regeneration from the source tag is not claimed.
