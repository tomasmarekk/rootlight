# Local Meson parser integration

All 33 regular Git blobs from staysail/tree-sitter-meson commit
1a497eecfb1b840ab12caf28f0ef45d4a5e26d28 are retained
under upstream with exact bytes (see SOURCE-PROVENANCE.json for the authoritative
commit and UPSTREAM-INVENTORY.json for every blob). Upstream parser tables and
grammar are unchanged. Root attributes apply to the retained sources; upstream
Git attribute metadata is renamed and inert.

The operational Rust wrapper uses the existing LanguageFn and cc dependencies.
The retained old Rust binding and its Tree-sitter 0.20 requirement are inert;
they are not workspace members or build inputs. Compilation reads the local
generated parser only. No scanner, generator or network access is required.

The grammar recovers valid Meson syntax outside its older supported subset.
Recovery must remain observable; native parsing does not prove project execution,
dynamic name binding, dependency discovery or complete Meson language support.
