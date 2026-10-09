# Local Fortran scanner safety patch

The retained Fortran frontend preserves its original authored inputs and upstream
license. `src/scanner.c.orig` is the byte-exact source from the retained archives;
only `src/scanner.c` has the local scanner-state and labeled-DO changes below.

## State frames and allocation boundaries

The native-endian scanner frame remains continuation:u8, depth:i32, depth labels:i32,
depth counts:i32, pending:i32, and pending-eos:u8. Its size remains exactly
`10 + 8 * depth`: depth 0 through 100 occupies 10 through 810 bytes within the
unchanged 1024-byte Tree-sitter serialization buffer.

Restore clears prior state, validates the complete frame into a temporary scanner,
and publishes only after validation. Nonempty null buffers, truncated or trailing
bytes, depth outside 0 through 100, noncanonical boolean bytes, nonpositive active
counts, and negative pending counts reset the scanner. Serialization validates the
same state domains and buffer capacity before the first output write. Invalid or
null state/output returns zero without publishing a partial frame.

The constructor returns its zero-initialized allocation without dereferencing a
failed allocation. Scan rejects null state, lexer, or token flags; restore and
serialization reject null state, and destruction accepts it as a no-op. These
guards do not prove allocator-failure injection or allocator-wide recovery.

## Labeled-DO bookkeeping

The unchanged 100-entry stack bounds distinct stored labels, not repeated adjacent
labels sharing the top entry. Positive count and pending values above 100 remain
supported. A repeated top-label increment at `INT32_MAX` and a new distinct label
at depth 100 fail before state mutation or token publication.

An internal three-way number-scanning result propagates a refused DO label to the
external scanner. Refusal cannot fall through and claim an integer or a later
operator token while silently omitting its label state. No production resource
limit, grammar rule, generated parser, or scanner wire representation changes.

## Regression boundary

`tests/native-grammars/tests/fortran_scanner.rs` uses the isolated native-grammar
workspace's existing unsafe boundary. Its fixtures cover every admitted depth,
exact byte round-trips, unaligned buffers and output canaries, malformed frame
domains, serializer preflight, null entrypoints, distinct-label capacity, repeated
labels above that capacity and at full depth, checked top-count increments, one
large pending decrement, ordinary syntax, and clean versus incremental parsing.

The null tests deliberately do not claim allocation-failure injection. Such an
injection requires a separate allocator shim without a shared mutable process-wide
hook that could interfere with parallel parser tests.

This patch addresses the identified scanner-state and bookkeeping defects. It
does not establish an external route for arbitrary state injection, comprehensive
security signoff, source self-containment acceptance, or whole-language semantic
coverage. Original archive/file inventories and licensing remain authoritative in
`SOURCE-PROVENANCE.json`; the local change is declared separately there.
