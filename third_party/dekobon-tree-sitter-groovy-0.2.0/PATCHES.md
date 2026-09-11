# Groovy syntax and scanner corrections

The source is the published `dekobon-tree-sitter-groovy` 0.2.0 crate from
https://github.com/dekobon/tree-sitter-groovy, revision
`0562eda573c28154f7bf5dd9b6a8ece4c7227b42`.
Archive SHA-256:
`891a2367f48836c854e84bac87f17513707a65a20dce7cab5707ddde6ca23fe0`.
The original `LICENSE-APACHE` and `LICENSE-MIT` notices are unchanged;
the declared license is `Apache-2.0 OR MIT`.

Changes from the published archive are limited to `grammar.js`,
`src/grammar.json`, `src/node-types.json`, `src/parser.c`, `src/scanner.c`,
`src/tree_sitter/array.h`, and the added `tree-sitter.json` generation metadata.
All other published files, including Rust bindings and build configuration,
are preserved. Generated sources use Tree-sitter CLI 0.26.8 with
`generate --abi 15 --js-runtime native`; the generated parser uses ABI 15.

The grammar and scanner changes preserve source boundaries and distinguish:

- Statement separators, compound statements, newline continuations and comments.
- Typed declarations, command calls, command chains and competing expressions.
- Slashy strings versus division, escaped slash endings and lazy interpolation.
- Unicode identifier categories and interpolation paths.
- CR, LF and CRLF comment endings without consuming adjacent source.

The scanner owns its continuation state. Serialization is empty or exactly five
bytes: a little-endian remaining distance and a validated decision byte.
Malformed input resets state. Continuation lookahead caches positive and negative
decisions, counts lexer advances rather than UTF-8 bytes, and publishes state only
after successful token emission.

The parser and scanner share eight external token kinds. The string-content guard
is a non-emitting context marker: it prevents comments or whitespace from being
consumed inside literals, while retaining normal embedded-expression handling and
the competing completed-slashy branch. It never emits a zero-width token.
Do not mix this scanner with the upstream generated parser or reuse persisted
parser state across the changed grammar identity.

The isolated native test suite contains 135 authored syntax, exact-coordinate,
scanner-state, incremental-edit, cancellation, parser-reset and threaded-lifetime
regressions. Fixtures are parsed only, never executed. Separate AddressSanitizer
qualification instrumented Tree-sitter 0.26.11, this parser and scanner, and the
test harness, with a working detector negative control. Finite test coverage is
not a proof of universal memory safety, leak freedom or semantic completeness.
Native parser qualification alone does not establish Rootlight structural,
semantic or MCP support for Groovy.
