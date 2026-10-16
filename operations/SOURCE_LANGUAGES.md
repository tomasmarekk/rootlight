# Explicit source-language assignments

Use the `source-languages` configuration extension when a source filename is
ambiguous or nonstandard. The daemon reads it from the same user configuration
as other Rootlight settings; restart the daemon after changing that configuration,
then request indexing. Library callers can supply it through `ConfigLayer`.

```toml
version = "1.3"

[extensions.source-languages]
version = "1.0"
critical = true

[extensions.source-languages.data.extensions]
bas = "vba"
cls = "vba"

[extensions.source-languages.data.paths]
"legacy/module.bas" = "vb6"
"scripts/driver" = "python"
```

Exact repository-relative paths are case-sensitive and override extension rules.
Extension keys use lowercase letters, digits, hyphens or underscores, begin with
a letter and omit the dot. They match the final extension case-insensitively at
any depth. Paths use `/`; absolute paths, traversal, backslashes and wildcard
patterns are rejected. Rules are not executable and never read additional files.
The extension uses the existing configuration byte, nesting and node limits.

Use canonical language labels, such as `vba`, `cpp` and `python`. An unsupported
label retains source retrieval with unsupported-language coverage; it does not
fall back to a different parser. Assignments do not install an analyzer or prove
semantic completeness. Binary, linked, unreadable, excluded or oversized sources
remain subject to the normal discovery safety policy. Assigning a language does
not add support for extracting binary containers such as MATLAB live scripts.

CMake uses the `cmake` label. `CMakeLists.txt`, `.cmake` sources and their
`.in` templates route to the same written-source profile at any depth, with
case-insensitive filename recognition. Template substitutions and CMake execution
remain unevaluated; physical definitions and command sites do not establish
effective variables, targets, macro expansion or command dispatch.

F# uses `fsharp` for implementation sources and `fsharp-signature` for signature
contracts. Unambiguous `.fsx` and `.fsi` filenames select those distinct profiles.
The `.fs` suffix is also used by Forth: automatic F# routing requires bounded
module/namespace and declaration evidence, with conflicting content kept as
unsupported source. This hint is not universal language recognition. Use narrow
path assignments for headerless, nonstandard or otherwise unproven `.fs` inputs,
and assign signature contracts explicitly when their filenames use `.fs`.

The highest-authority layer supplying this extension replaces the entire rule
set; maps from different layers are not merged. Supply empty `data = {}` to
clear prior rules. The payload contributes to the configuration hash, so changed
assignments invalidate generation reuse even when source bytes are unchanged.
Discovery records `configuration` evidence separately from filename/content
evidence. The critical flag is required so older binaries reject the settings
instead of silently interpreting the same source with a different language.

User configuration applies to repositories indexed by that daemon. Choose narrow
path rules where repositories need different interpretations of a shared suffix.
Do not treat configured indexing results as successful automatic language detection.

V uses the `v` label. `.vv` compiler-input fixtures select V by extension;
validity is reported separately by analysis. `.v` is shared with Verilog and
Coq, so automatic V selection requires bounded written-code or literal V
interpreter evidence. `.vsh` retains its GLSL extension default unless V code
or a literal V shebang identifies a script. Comments and quoted data cannot
supply declaration evidence. Exact path assignments can resolve ambiguous
sources without assigning every file with the shared suffix to one language.

Coq/Rocq uses the canonical `coq` label and accepts the `rocq` alias. Complete
written command sentences can provide bounded content evidence for `.v` sources.
Nested comments and strings do not supply commands, and contradictory written
language signals remain explicit. A suffix alone does not select Coq.
The native Coq adapter extracts written declarations and names for reviewed
constructs. Parse recovery, unmodeled commands, module or section ownership,
and incomplete reference analysis retain explicit source-bound gaps. The adapter
does not elaborate terms, evaluate tactics, resolve proof environments, or check
proofs. Exact source availability remains separate from these analysis limits.
Use exact path assignments for sources whose bounded prefix supplies no evidence.
