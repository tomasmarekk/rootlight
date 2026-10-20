# Crystal native sources

The complete crystal-lang-tools/tree-sitter-crystal source is retained in
`upstream/` and `upstream.tar.gz`, pinned to
`50ca9e6fcfb16a2cbcad59203cfd8ad650e25c49`. No upstream tags or releases were
published at preparation; the original package version is `0.0.1`. Its MIT
license and upstream authorship remain intact. Original executable intent is
recorded in SOURCE-PROVENANCE.json, independently of Windows file modes.

The local static LanguageFn binding pins the project's existing runtime/build
inputs. Its C11 build compiles only retained parser/scanner/header sources.
No dependency acquisition, upstream lifecycle scripts or generation runs at build
or package time. Upstream instructions and package scripts remain inert data.

The selected grammar adds a short-block setter with a mandatory right-hand-side
expression and was genuinely regenerated for ABI15 using the retained local
Tree-sitter CLI. Original parser errors and failed earlier candidates remain in
preparation evidence. The selected scanner validates a complete serialized extent
before restoring it; malformed/truncated states reset to the ordinary empty state.
It preserves valid ordinary quoted-literal stacks beyond the percent-only limit.
Every literal/heredoc growth operation is admitted against the exact 1024-byte
runtime serialization capacity before changing the stored stack. This preserves
valid 17-level and deeper ordinary interpolation while rejecting excess growth
with a syntax error instead of writing beyond the runtime state buffer.
The original generated inputs and scanner are separately retained under upstream.

Syntax grounding: Crystal 1.22 blocks-and-procs reference, short one-parameter
syntax, and crystal-lang/crystal tag 1.22.0 `src/compiler/crystal/syntax/parser.cr`
`parse_call_block_arg` / `parse_call_block_arg_after_dot`. The documented operator
calls and indexed calls remain admitted. Infix text following a short method
call is not substituted for the documented operator-method form.

Native observations cover the retained 75 original `.cr` files, setter/operator
controls, incremental/fresh equality and bounded scanner restoration controls.
They establish neither full Crystal semantics nor general scanner completeness.
The five `.ecr` template files are separate inputs; their semantics are not
claimed by this grammar binding. The finite source fixture retains its own MIT
notice and provenance under tests/fixtures.
