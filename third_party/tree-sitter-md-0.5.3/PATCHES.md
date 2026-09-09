# Markdown source grammar qualification

The baseline is `tree-sitter-md` 0.5.3, upstream commit
`f969cd3ae3f9fbd4e43205431d0ae286014c05b5`. Its published archive SHA-256 is
`2efd398be546456c814598ee56c0f51769a77241511b4a58077815d120afa882`.
The archive omits LICENSE and the required `common/common.js` module; their
unmodified contents are restored from that commit, and the package include path
is corrected to retain the actual shared grammar module.
Both generated parsers use ABI 15. Grammar definitions, generated parser tables,
node types, query files and generated headers are unchanged from the archive.

Block and inline scanners retain full delimiter lengths instead of wrapping at
one byte. The block scanner also preserves tab-expanded indentation and stack
match counts. Source counters use 64-bit storage; decoded values must fit the
existing 32-bit Tree-sitter source-coordinate domain (four times that for tabs).
This does not change Rootlight's source, depth, node, memory or output budgets.

Both scanner snapshots use version 1 with explicit little-endian fields. The
block header is 23 bytes followed by one byte per complete block, up to 1001
entries within Tree-sitter's unchanged 1024-byte serialization buffer. The
inline snapshot is 26 bytes. No host enums, pointers or alignment-dependent
integers are copied to the wire. The block stack is fixed storage: scanning and
restoration do not allocate, truncate entries or overwrite the snapshot buffer.

Decoders validate the full frame, version, flags, counts, enum values and source
counters before publishing any restored state. Truncated, oversized, unknown or
inconsistent frames enter a persistent `ff` failure state. Stack exhaustion also
enters this state and emits the grammar's error token. A zero-length reset clears
it. Allocation failure and null scanner payloads cannot be dereferenced.

Regression tests in `tests/native-grammars` cover exact block and inline source
ranges, container-marker exclusions, UTF-8/CRLF positions, long delimiters and
indentation, full-capacity snapshots, malformed frames, deep nesting, parser reuse
and clean/incremental equivalence. The block grammar is registered for explicit
source-backed analysis; inline parsing and automatic indexing capability are
separate integration contracts. These tests do not claim full CommonMark/GFM semantics, Markdown
rendering equivalence, MDX/Astro support or installed MCP acceptance.
