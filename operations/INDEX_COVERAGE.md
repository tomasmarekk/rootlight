# Public-service coverage diagnostic

The `rootlight-service` example `index_coverage` indexes a repository into a new,
isolated durable state directory. It uses the production service, not a second
parser implementation. It does not start a daemon, run repository code, change
repository configuration, or connect through MCP.

```sh
cargo build --locked --offline -p rootlight-service --example index_coverage
target/debug/examples/index_coverage /path/to/repository /path/to/new-state > coverage.json
```

An optional third argument selects a configuration file. To diagnose explicit
language assignments, use the format described in [SOURCE_LANGUAGES.md](SOURCE_LANGUAGES.md).
Run default detection and configured analysis separately with different new state
directories. A configured result is not evidence of automatic language detection.

The state directory must be absolute and must not already exist. Its parent must exist, and the platform
must support the service's private durable-state boundary. The example retains
the state for inspection; it does not overwrite an existing audit.

The JSON output records:

- Cold indexing and unchanged-repository no-op observations, with identical
  generation identity required for the no-op.
- Separate preparation and publication wall times. Completed cold preparation
  stages record the elapsed time since the preceding milestone, plus examined
  and written counters; `after_last_progress_micros` accounts for work after
  the last milestone. A retained no-op can finish before any completed-stage
  callback, so its stage list may be empty. Timings include the synchronous
  progress observer and are diagnostic wall time, not CPU profiles. The ordered
  `progress_events` include intermediate callbacks and intervals since the
  preceding event. Each interval brackets multiple operations and must not be
  attributed to one internal function without finer profiling.
- Effective configuration identity and the separate raw configuration input hash.
- Language-scoped indexed counts and every retained file record.
- Exact generation-bound source retrieval for every retained file, using bounded
  chunks with no added context and a complete returned-byte hash. Physical files
  are also compared to live file bytes and hashes. Valid physical UTF-8 uses
  character-aligned chunks; other retained encodings use exact raw-byte reads
  without transcoding.
- For extracted sources, the complete mapped physical container is checked
  against the live file once. The derived source has no editable filesystem path:
  its source read and origin are verified, while independent extraction-byte
  equivalence remains explicitly unproven by this diagnostic.
- Per-file occurrence counts by syntax and resolution, plus explicit skipped
  regions from the normalized index.

Inspection borrows the service's identity-verified immutable generation under a
local lease. It does not create a sharing bundle: large generations can be audited
without changing the independent 32 MiB default and 128 MiB hard transfer
ceilings. The report explicitly records that no sharing round-trip occurred.
This diagnostic includes normalized and file-only retained records, but does not
qualify the sharing API; validate that boundary separately where required.
`source_exact` is true only for a directly compared physical file. Extracted
rows report `source_read_exact`, `container_live_exact`, and
`extraction_independently_verified: false` separately; do not count the last
field as a successful extraction proof.

A successful run establishes these checks only. Independently compare the output
against a complete repository inventory to account for excluded files. Missing
declarations or relationships need source-grounded language-specific checks;
neither zero recorded gaps nor high occurrence counts prove semantic completeness.
This diagnostic does not qualify an installed package or the MCP transport.

For an independent, discovery-only path ledger, use the `source_inventory`
example. It uses the same default configuration and tracked-path capability
admission, with discovery audit enabled, but does not parse sources, create
service state, run repository code, or mutate the repository.

```sh
cargo build --locked --offline -p rootlight-service --example source_inventory
target/debug/examples/source_inventory /path/to/repository > inventory.json
```

Its optional second argument is a configuration file. The output retains the
canonical manifest and hash, included path/content/encoding identities, actual
exclusions with their decisive rules, diagnostics, and bounded traversal counts.
Its file IDs belong to a shared diagnostic namespace, not a registered service
repository or generation; compare exact paths and original byte identities,
not those IDs, against an indexing report. Excluded directories have an observed
directory record, not fabricated records for unvisited descendants. Independently
join a complete tracked inventory to this ledger to account for those descendants
and missing paths. A complete discovery traversal does not prove structural or
semantic language coverage. Inventory hashing also warms filesystem caches and
must not be described as an OS-cold indexing measurement.

BOM-backed, strictly valid UTF-16LE/BE text is retained with its original encoding,
hash and byte ranges. A bounded decoded prefix supplies discovery signals.
File-level lexical vocabulary scans the complete UTF-8 or UTF-16 text with the
same word-size and aggregate bounds. Exact omission counts use original encoded
bytes, including repeated oversized words; decoding does not replace source
snapshots. Scoped text queries can select witnesses beyond the stored prefix.
These lexical capabilities do not provide UTF-16 structural analysis: such files
retain explicit unsupported-encoding coverage gaps and do not count as
structurally indexed language support. Exact source reads use raw-byte mode;
the UTF-8 presentation mode does not silently transcode. Malformed UTF-16 and decoded NUL remain
excluded; this admission rule does not recognize arbitrary legacy code pages or
embedded container sources.

The timing observations are not a release benchmark: record build profile,
hardware, source/configuration hashes and filesystem/cache conditions before
comparing results. Incremental mutation measurements require a separate controlled
scratch repository. Existing-cache offline compilation does not prove a
source-contained build with empty dependency caches.
