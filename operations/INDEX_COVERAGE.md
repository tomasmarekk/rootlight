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
- Effective configuration identity and the separate raw configuration input hash.
- Language-scoped indexed counts and every retained file record.
- Exact generation-bound source retrieval for every retained file, using bounded
  chunks with no added context, compared to live file bytes and hashes. Valid
  UTF-8 uses character-aligned chunks; other retained encodings use exact raw-byte
  reads without transcoding.
- Per-file occurrence counts by syntax and resolution, plus explicit skipped
  regions from the normalized index.

Inspection round-trips a source-free snapshot through the sharing API with its
existing 128 MiB caller-selectable ceiling. The report records the actual bundle
size and selected bound. This does not alter the 32 MiB default transfer policy,
the service's indexing budgets, or the process hard ceilings. A larger snapshot
fails the diagnostic explicitly rather than producing a partial report.

A successful run establishes these checks only. Independently compare the output
against a complete repository inventory to account for excluded files. Missing
declarations or relationships need source-grounded language-specific checks;
neither zero recorded gaps nor high occurrence counts prove semantic completeness.
This diagnostic does not qualify an installed package or the MCP transport.

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
