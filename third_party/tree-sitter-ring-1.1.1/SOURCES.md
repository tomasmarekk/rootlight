# Pinned Ring grammar inputs

The complete31-blob MIT source tree is retained from the declared immutable Git
revision. Every archive byte and executable intent was joined to the Git tree;
upstream ignore/attribute filenames are inert and their mapping remains explicit.

The active generated C tables are unchanged. The handwritten external scanner
adds only initialization of its state before deserializing an optional buffer,
so a zero-length state restores the initial state instead of retaining history.
LOCAL_CHANGES.patch records that correction. Original author/license notices remain.

The local Rust wrapper compiles existing C files and local headers with the
declared toolchain. It does not invoke upstream install, Bun, a generator or any
source download. Upstream generator metadata is retained as inert provenance.

Native syntax success does not qualify Rootlight definitions, binding, source
mapping, discovery or installed behavior. Runtime syntax changes and DSL effects
remain unproved. The .rh and .rform suffixes remain discovery-ambiguous until
independently source-grounded; they are not automatic Ring routes here.
