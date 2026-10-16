# Exact literal source matching

The MCP tool `source.find` searches one known indexed file in an immutable
generation. It matches a nonempty literal exactly and case-sensitively as Unicode
scalars, including overlapping matches. It returns original-byte `SourceRef`
locations, shared file identity and explicit scan accounting. Use `source.read`
with those references to retrieve the matched text or surrounding context.

```json
{
  "repository": {"repository_id": "<repository ID>"},
  "generation": "<generation ID>",
  "file_id": "<file ID>",
  "literal": "written spelling",
  "maximum_scan_bytes": 1048576,
  "maximum_matches": 128
}
```

Omit `generation` on the initial request to select the active generation. Every
successful response pins the resolved generation. A continuation must use that
explicit generation and preserve the repository selector, file, literal, scan
allowance, match allowance and effective response budget. The optional
`response_profile` selector accepts only `compact`, the tool's single supported
representation. Supply
the returned `next_cursor` as `cursor`. Cursors authenticate these controls and
the physical file identity; altered or stale continuations fail explicitly.

The literal may contain up to 64 KiB of UTF-8 query bytes. Each page allows at
most 64 MiB of forward physical-byte scanning and 1,024 matches; the default
match allowance is 128. Response and execution budgets can stop a page earlier.
The response distinguishes forward scanning from separately charged prefix
restoration. It never copies match bodies or presents an unobserved total count.

For source-only requests, omitted `budget.timeout_ms` does not impose an implicit
deadline on complete immutable-source verification. Any supplied value, including
the ordinary default value, is an explicit limit bounded by the existing server
ceiling; requesting a larger timeout does not raise that ceiling. Request
cancellation, response memory and byte limits, and per-page scan limits still
apply. A small requested range or scan page can require authenticating the full
stored file; verification does not allocate a full-file snapshot. Continuations
authenticate both the effective limits and whether duration was explicitly set.
The same omission rule applies to `source.read`; its lossless-text response shape
remains contract 1.3.

`data.stop` explains the actual boundary: `end_of_source`, `scan_limit`,
`match_limit`, `matcher_memory_limit`, `response_memory_limit`, or `result_limit`.
Only `end_of_source` finishes the scan. Earlier stops set `truncated` and return
an authenticated continuation; an empty match page can still be incomplete.
Complete scanning of a file does not establish language-analysis completeness.

UTF-8 and BOM-qualified UTF-16 little-endian and big-endian sources retain their
physical encoding, length and complete content hash. UTF-16 matching starts
after the encoding BOM; all returned offsets refer to the original physical
bytes. A UTF-8 BOM remains part of the source spelling. Extracted sources retain
their immediate physical container provenance. Repository-controlled paths,
language labels and references carry `untrusted_repository_data` classification.

This tool provides exact retrieval evidence for one identified file. File
discovery remains separate, and definitions, symbols, resolution and relations
must be qualified through their own language-specific obligations. Existing
`code.locate` text matching keeps its indexed lexical semantics. `source.find`
uses MCP tool contract 1.1 and requires daemon protocol 1.28 for the omitted
verification-deadline policy. Legacy peers retain their existing finite request
timeouts and cannot silently accept that policy.
