# Pinned HCL source inputs

Complete tree-sitter-grammars/tree-sitter-hcl v1.2.0 at
`fad991865fee927dd1de5e172fb3f08ac674d914` and its original archive are retained.
The Apache-2.0 license is unchanged. Original Git file identities and executable
intent are recorded in `SOURCE-PROVENANCE.json`; Windows mode equivalence is
not asserted. Upstream Git attributes/ignore files are retained with inert names.

The selected ABI15 parser and headers are unchanged. The scanner derivative
validates serialized state extents and context kinds before copying, preserving
its wire format and normal scan paths. Original and derivative native parses
of all 49 selected original HCL sources agree, including incremental/fresh trees.
This is component evidence, not complete HCL/Terraform semantics or product
qualification. Existing scanner serialization limits are not a full heap proof.

The static Rust wrapper compiles only the selected local C parser/scanner and
local headers with existing pinned dependencies. No build downloads code, runs
upstream scripts or generates parser tables. The separately retained original
MIT source fixture keeps its original CRLF bytes and attribution.
