# COBOL grammar provenance and local changes

These sources come from
[`yutaro-sakamoto/tree-sitter-cobol`](https://github.com/yutaro-sakamoto/tree-sitter-cobol/tree/550020ddf42ef9b718ad897868a9308d2afcda35),
revision `550020ddf42ef9b718ad897868a9308d2afcda35`, metadata version `0.0.1`.
The upstream MIT notice is retained byte-for-byte in `LICENSE`.
The upstream README attributes the syntax rules to `opensource-cobol` and
mentions NIST COBOL85 validation. This does not establish the licensing of the
upstream fixture collection; none of that collection is retained or shipped here.

The retained upstream subset contains the authored standalone `grammar.js`,
generated grammar/node metadata, original ABI14 parser table/header, scanner,
license and original Rust build/binding/manifest inputs. It is not a complete
copy of the upstream repository. No Node binding, CI script, fixture, cache or
unneeded build system is invoked by this package. The preferred grammar has the
bounded syntax corrections described below; its generated C table and headers
are retained along with exact upstream originals. Compilation requires only
these local inputs and the versioned `cc` and
`tree-sitter-language` dependency sources with the declared C/Rust toolchain.
The original changed files have `.orig` copies. Acquisition association and
original source hashes are recorded in `SOURCE-PROVENANCE.json`.

The local unpublished Cargo binding uses `LanguageFn` rather than returning
the obsolete Rust runtime's `Language` representation from C. It compiles two
separate immutable factories from the same table. `FIXED` preserves the scanner's
fixed text-area rules; `FREE` disables prefix/suffix/indicator handling and fixed
continuation inference. Distinct language identities prevent cross-format cache
reuse. Neither profile pads, truncates, joins or normalizes the input buffer.

## Preferred grammar corrections and generation

The literal rule accepts either `x` or `X` immediately before a single- or
double-quoted payload, matching the case-insensitive COBOL prefix. The rule
preserves its prior lexical payload policy; it does not validate or decode
hexadecimal bytes. An ordinary storage operand named `x` remains a separate
word when separated from a following literal. File organization additionally
admits `LINE SEQUENTIAL`, including the existing optional `ORGANIZATION IS`
keywords and original physical-line boundaries. Existing organization choices
are unchanged. These forms are documented in the
[GnuCOBOL Programmer's Guide](https://gnucobol.sourceforge.io/HTML/gnucobpg.html).

Both `OCCURS` bounds admit an integer or a bare symbolic constant candidate,
including the level-78 substitution described by that guide. A dedicated
`occurs_bound` node preserves the existing `num`/`to` fields and exact written
integer or `WORD` leaf. A preparation-only finite-prefix expression excludes
structural clause keywords without changing the global word or integer rules;
keyword-prefix identifiers remain admissible. This contextual exclusion is not
a complete reserved-word table. Unknown and noninteger names remain syntax
candidates, not evaluated constants, cardinalities or proven storage reads.
The adapter retains symbolic bounds as unresolved source-bound references.

Tree-sitter CLI 0.24.4 generates ABI14 outputs from the complete local
`grammar.js` using an explicit local Node.js runtime during source preparation
only. The command, tool hashes and output hashes are pinned in
`SOURCE-PROVENANCE.json`; compilation never executes either tool. Repeated
generation from independent copies of the preferred grammar produces identical
outputs. The upstream generator version is unknown, and regenerating the
unchanged upstream grammar with this CLI is not byte-identical to its original
tables. That unchanged-generation baseline preserves the normalized grammar
rules and passes the existing authored native contracts; it is not a proof of
all-tree behavioral equivalence. Exact original grammar, generated metadata,
table and header bytes remain available as `.orig` files.

The authored `cobol_literals_and_files.rs` regressions distinguish clean and
recovered native trees, exact literal/query spans and file-clause ownership for
both profiles, LF/CRLF and EOF with or without a final newline. Malformed
prefixes and incomplete organization forms must retain native recovery, even
when a missing leaf is invisible through the public cursor. This is bounded
syntax evidence, not literal decoding or file/runtime semantics.

The authored `cobol_tables.rs` contracts cover both bound positions, numeric
parity, physical source coordinates, clause ownership, ordinary subscript and
reference-modification operands, malformed forms and incremental/fresh tree
equivalence. Constant substitution, integer typing, rank, storage layout and
table-element binding are not established by these native checks.

## Scanner corrections

The local scanner uses constant-size keyword tables, bounded keyword pointers,
actual EOF checks and ASCII-only case/separator classification. Short physical
lines cannot be joined while consuming the fixed prefix, Unicode lookahead is
not narrowed to a byte, recovery does not invent comment entries, and header
prefix lookalikes are rejected. Comment spans include their visible text.
Fixed continued strings require matching, in-area quote boundaries, support
doubled delimiters, retain their entire authored span, and distinguish LF/CRLF
from unsupported bare-CR continuation. Stateless serialization writes no bytes.

The authored `tests/native-grammars/tests/cobol_*.rs` contracts exercise scanner
progress/boundaries, native structure and byte ranges, edit/recovery equivalence,
and cancellation/reset. Native embedding checks qualify one complete contiguous
physical-line body over unchanged Markdown bytes for both profiles and LF/CRLF,
including original-source EOF, fixed comments/continued literals, neighboring
fences, and cancellation/reset. Markdown separately admits that body shape only
under an unindented opening fence and qualified document ownership; the sole
clean whole-snapshot EOF fence may retain definitions inside a native error root
without clearing the host recovery gap. Container-deindented or disjoint bodies
retain an explicit analysis gap. Public
COBOL included-range requests remain rejected. No general range-jump support is
claimed. Parser success is not full COBOL conformance. Compiler directives,
function units, copybook fragments and COPY REPLACING have known unsupported
syntax; COPY expansion,
storage qualification and CALL/PERFORM dispatch are not inferred from names.
