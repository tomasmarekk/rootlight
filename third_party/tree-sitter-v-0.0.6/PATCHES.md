# Local static binding

The original grammar, generated parser, node types and headers from
`vlang/v-analyzer` commit `925d4570d1668746762a2cdf0ecb9a25be704a67`
remain unchanged in the complete authored tree under `upstream/`. The preferred
grammar has the local overlay described below. Its generated outputs are not
hand-edited. The upstream package version `0.0.6` is marked private; this is
an unpublished local Rust binding, not evidence of a published release.

The complete analyzer archive and authored tree are retained under `upstream/`.
The original `tree_sitter_v/bindings/core` Git link is separately acquired at
`cd4b6e2ef996d4baca12caadb78dffc8b55bc869`, with its complete archive, files,
licenses and executable intent. Nested Git ignore and attribute files have
inert `.upstream` names; the exact bytes and original paths are inventoried.

The local build compiles only `src/parser.c` and its local parser header using
the declared C compiler and authenticated `cc` and `tree-sitter-language`
dependencies. The existing Rootlight Tree-sitter runtime supplies parsing.
Neither the original V binding/core, Node entry point, generator, nor Wasm
configuration is executed. The inactive upstream Node entry point is absent
from its authored tree; optional Wasm headers and a generated symbols include
are not supplied. No all-target upstream build closure is claimed.

Upstream generation declares CLI 0.26.3 and a V binding generator. The local
overlay was prepared with the separately declared Tree-sitter CLI 0.26.8,
native JavaScript runtime and ABI 15. Two fresh generation roots produced
identical outputs. `patches/regeneration.json` pins the preparation tool,
arguments, grammar, patch and generated files. Native compilation consumes
these local tables without invoking a generator. Source downloads and
generation are prohibited during build and packaging.

## Grammar overlay

`patches/grammar.patch` contains four source-level corrections:

- Global declaration groups permit an inline closing parenthesis and adjacent
  declarations without forcing a trailing line terminator.
- Top-level declarations and script statements consume explicit semicolons.
- A line comment is one lexical token. Its body cannot be reused as a
  separate syntactic fragment during an incremental edit.
- A defer statement accepts the written `(fn)` mode as well as a scoped
  block; unknown modes and missing delimiters or bodies remain errors.

The global, semicolon and defer forms are supported by the V compiler parser
at revision `e65ec29e8738bfc1875d722161b9babae372eb86`, specifically
[`vlib/v/parser/parser.v`](https://github.com/vlang/v/blob/e65ec29e8738bfc1875d722161b9babae372eb86/vlib/v/parser/parser.v).
This is syntax support, not execution of deferred code or compile-time logic.
Original authored corpus expectations remain unchanged. A concrete initializer
classification gap and a malformed-constant recovery-tree mismatch remain
applicable; the overlay does not claim complete native language coverage.

Original MIT, Unicode and embedded portable-endian notices remain unchanged.
The local binding does not replace original authorship or make a legal
acceptance claim. Native syntax tests do not establish structural extraction,
semantic support, an installed package, or independent offline acceptance.
