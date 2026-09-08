# R scanner qualification

The baseline is the published `tree-sitter-r` 1.3.0 archive, SHA-256
`ffc9954ec870dcad6cffdd302b405306c68cf031ed79a78cd9746f6740d9fe20`.
Its VCS metadata identifies `f2289db204dda2bf7c7ea720de5c2a2c3eff2530` in
<https://github.com/r-lib/tree-sitter-r>. The grammar, generated parser, scanner
baseline and MIT license match that revision. Tree-sitter CLI 0.24.7 with ABI
14 reproduces the published parser, grammar JSON and node types byte for byte.
Generated files and the upstream queries are unchanged.

The scanner retains the upstream process-local snapshot format: three raw
delimiter bytes, a native unsigned scope count and one byte per open scope.
Deserialization rejects oversized frames, invalid delimiter tuples, invalid
scope values and trailing bytes before copying the scope array. Rejected
frames reset all prior context. Serialization remains observational and fits
the existing 1024-byte Tree-sitter ABI buffer without dropping nesting state.
The upstream allocation and scope/hyphen limits are unchanged.

Raw-string scanning compares complete Unicode code points before narrowing
validated ASCII delimiters. Wide-character classification guards the platform's
representable range, so supplementary characters cannot alias whitespace on
Windows. A mismatched closing token does not pop a live scope. Raw closing
tokens revalidate the current source instead of trusting an earlier parse's
delimiter, and never advance beyond end-of-input.

The regression suites in `tests/native-grammars` exercise snapshots, direct lexer
callbacks, source spans, contextual newlines, raw strings and incremental/fresh
tree equivalence. R source is never executed. The unchanged upstream corpus,
highlight assertions and tag assertions also pass with the patched scanner.
This isolated native package is not itself a structural adapter, a guarantee
of R parser conformance, or evidence of complete R semantic coverage.
