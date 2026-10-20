# Pinned Modelica grammar sources

The complete MIT modelicahub/tree-sitter-modelica tree and matching archive are retained.
Tag v0.1.0 and npm version0.1.0 contain Cargo package version0.0.1; the original
metadata and stale Cargo repository URL remain explicit in SOURCE-PROVENANCE.json.
Every original blob and published executable intent is preserved. Upstream ignore
and attribute files are inert .upstream data, not project ignore/checkout rules.

The original ABI14 C tables and static LanguageFn wrapper are unchanged; there is
no external scanner. Compilation uses existing local cc/tree-sitter-language
inputs and runs no acquisition or generation. Tests belong to the existing
native-grammar workspace, never a standalone excluded-package Cargo lock.

Leading UTF8 BOM handling is provided by the pinned Tree-sitter runtime. Native
syntax success does not qualify Modelica analysis, encoded source mapping or
installed behavior. Original license and authorship notices remain unchanged.
