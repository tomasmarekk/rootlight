# Local Haskell grammar integration

The directory retains the complete 103-file upstream tree for
`tree-sitter/tree-sitter-haskell` at commit
`c30d812bc90827f1a54106a25bc9a6307f5cdcec` (tag `v0.23.1`, Git tree
`20213ad70e0b616fdc91065b00d93941cbde532c`). The MIT license and upstream
copyright notices are preserved. Authored grammar modules, including
`grammar/util.js`, are retained alongside the generated ABI-14 parser.

`SOURCE-PROVENANCE.json` records the original source inventories, archive
digests, Git blob identities and executable intent. Original archive permission
bits are evidence, not a claim that a Windows checkout reproduces POSIX modes.
Git tree and commit-object identities were checked against the selected pin;
the GitHub signature-verification metadata was inspected, but no independent
signing-key verification was performed. The upstream archive SHA-256 is a
recorded acquisition digest, not an independently published signature.

## Rust package manifest

`Cargo.toml.orig` is the exact upstream authored manifest. `Cargo.toml` starts
from the authenticated published crate's normalized manifest, with an additive
package include list for the retained license, provenance, authored grammar
modules, generator inputs and corpus. The unchanged Rust build script compiles
the retained `src/parser.c` and `src/scanner.c` with `cc`; it does not fetch
sources or run a parser generator. Upstream Node/CLI development and generation
files are retained for provenance, not invoked by an ordinary Cargo build.

## Scanner snapshot safety

`src/scanner.c.orig` is the exact upstream scanner. The local scanner patch:

- Checks serializer capacity before count multiplication and buffer writes.
- Reads process-local snapshot headers and context records with `memcpy`,
  without assuming the supplied byte buffer has C-struct alignment.
- Rejects truncated, oversized, trailing or count-inconsistent frames and
  invalid context/newline enum or Boolean representations before count-based
  allocation or publication of decoded state.
- Resets rejected frames using the existing zero-length `NResume` state,
  clearing prior contexts and lookahead rather than retaining a valid prefix.
- Applies the same length-before-allocation and unaligned-word-read checks to
  the optional `TREE_SITTER_DEBUG` parse-line payload.

The ordinary native wire layout, byte order, valid field domains and zero-state
behavior are unchanged. Snapshots remain process-local, not portable serialized
documents. The parser, grammar, tokens and language ABI are unmodified.

The isolated `tests/native-grammars/tests/haskell.rs` tests exercise the ordinary
scanner ABI and native syntax without evaluating Haskell. They do not establish
full scanner resource safety, debug-build coverage, compiler semantics, or
complete product dependency-source closure. In particular, upstream general
array allocation/OOM behavior and scan-time lookahead growth are outside this
bounded snapshot patch. Build and acceptance results are recorded separately.
