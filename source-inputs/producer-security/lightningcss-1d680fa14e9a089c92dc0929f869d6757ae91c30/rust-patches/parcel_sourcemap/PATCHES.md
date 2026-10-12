# Reviewed parcel_sourcemap archive derivative

This directory derives from the unmodified `parcel_sourcemap` 2.1.1 registry
publication. The original archive and expanded sources remain in
`source-inputs/crates/parcel_sourcemap-2.1.1.crate` and
`source-inputs/cargo/parcel_sourcemap-2.1.1/`. `DERIVATION.json` binds their original
SHA-256 values to this derivative's changed files. This is a local security
patch, not an upstream release or original authorship claim.

## Security boundary

The dependency changes from rkyv 0.7 to exactly 0.8.18. This removes the affected
0.7 line for RUSTSEC-2026-0235 and the earlier oversized archive allocation
advisory. Runtime deserialization uses rkyv's checked `from_bytes` API with the
`bytecheck` feature explicitly enabled. It does not form unchecked archived
references. An owned aligned copy accommodates arbitrary caller slice alignment.
Validation/serialization errors retain the existing `BufferError` category.

Primary references:

- [RustSec advisory RUSTSEC-2026-0235](https://rustsec.org/advisories/RUSTSEC-2026-0235.html)
- [rkyv 0.8.18 checked deserialization](https://docs.rs/rkyv/0.8.18/rkyv/api/high/fn.from_bytes.html)
- [rkyv alignment contract](https://rkyv.org/format/alignment.html)
- [rkyv format controls](https://rkyv.org/format.html)

## Compatibility

SourceMap construction, mapping operations, source/name/content access, VLQ,
JSON and data URL behavior are retained. `to_buffer` still clears and fills an
aligned byte buffer, and `from_buffer` still takes a project root and byte slice.
The nominal aligned buffer type now comes from `rkyv::util::AlignedVec` 0.8;
direct Rust callers importing the 0.7 type must update that dependency/import.
The selected Lightning CSS producer does not directly use the archive methods.

Internal binary cache bytes intentionally change. The exact 16-byte prefix is
`PSMAP-RKYV-0.8\0\0`, followed by the rkyv 0.8.18 archive using explicitly selected
little-endian, aligned, 32-bit-pointer format controls. The prefix preserves
16-byte payload alignment. Older unversioned buffers are rejected and must be
regenerated; there is no unsafe legacy decoding fallback. This is not a portable
or backward-compatible promise for future versions of the internal archive.

## Origin and license

The authenticated archive SHA-256 is
`485b74d7218068b2b7c0e3ff12fbc61ae11d57cb5d8224f525bd304c6be05bbb`.
Its supplied VCS metadata identifies
`a136bd6ba9f1d3cded501ae824341be0a54c5ce3`, path `parcel_sourcemap`, in
`https://github.com/parcel-bundler/source-map`.

The publication declares MIT and retains the original author metadata. It
contains no standalone license file; this derivative preserves the supplied
license declaration and every published source file. The original normalized
manifest is available in the immutable original source inventory;
`Cargo.toml.orig` and `.cargo_vcs_info.json` remain byte-exact origin metadata.
The original `.cargo-checksum.json` is not copied as a claim about changed bytes.

## Validation

`tests/archive_validation.rs` checks logical map/VLQ round trips, output reuse,
empty maps, unaligned input, all truncations of its populated fixture, malformed
format markers and an invalid archived boolean. JSON-enabled tests also compare
JSON and data URL output. The malformed-byte tests modify byte buffers and invoke
checked entry points; they never construct invalid typed references.

The producer security profile owns the effective dependency lock and complete
local source closure. Test runs use an isolated copy with its own target directory
and offline local sources. Test success is recorded separately from source
provenance; the retained source alone does not establish executed validation or
source-to-binary reproduction of published native packages.
