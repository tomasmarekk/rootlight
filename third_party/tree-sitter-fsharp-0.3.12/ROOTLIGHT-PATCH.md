# F# scanner state, signatures, type syntax, enums, measures and loops

This directory retains the MIT-licensed preferred grammar sources, Rust binding
and complete native compilation payload from `ionide/tree-sitter-fsharp` tag
`0.3.12`, commit `aefd0c8741bdf3aeb827a228aa4a996a6536697e`. The unchanged HTTPS
archive is retained in `upstream/`; its SHA-256 is
`5c40b126e9c4641ac8775d2cd9d390503fd3ec1307531412143969f263b0f0ad`.
`SOURCE-PROVENANCE.json` records all 129 original regular members, the selected
payload, preserved license, and ten bounded local overrides. HTTPS acquisition
and a commit-addressed URL do not independently authenticate a signed Git object
or publisher digest. This is not the complete upstream repository: example
submodule contents are absent and unrelated fixtures/bindings are not compiled.

The shared external scanner originally narrowed indentation columns in snapshots,
accepted truncated stack prefixes, and could lose empty or nondefault base state.
The patch preserves complete paired indentation state and independent
preprocessor state using full-width columns and a versioned, bytewise codec.
It validates complete frames before publishing decoded state. Snapshot capacity,
column overflow and allocation failure decline scanning with a canonical sticky
unrepresentable-state snapshot instead of emitting a token with lost state.
Fresh reset clears that state. Allocation failure is handled in the scanner but
the tests do not inject every allocator failure or prove that every parser
alternative produces a particular product-level resource error.

Nested block comments are scanned iteratively without consuming the outer closing
parenthesis or moving the token end inside nested delimiters. The scanner uses
the lexer EOF callback, so embedded NUL is content rather than synthetic EOF.
Existing inside-string and operator-reference controls remain intact. Inactive
preprocessor branches may still be treated as trivia; a clean parse is not proof
that declarations in every conditional branch were structurally analyzed.

Hash-prefix lookahead preserves the pending layout mark before `#`. Only complete
directive names may change directive state; directive-like identifier prefixes
must remain intact for type syntax. Named external conditional-directive tokens
prevent the internal lexer from adopting a shorter directive prefix. Structured
tokens end at the directive keyword, not its condition or the first branch token.
The internal `#line` and `#light` rules share the initial hash with type syntax
instead of greedily consuming directive-like type-name prefixes. Their existing
anonymous directive leaves cover the hash; the parent directive, argument nodes
and public node vocabulary retain their written ownership.
The signature profile uses the inherited directive extras consistently with the
implementation profile. Its redundant ordinary compiler-directive alternative
is removed from the signature-element supertype; the named directive node and
its arguments remain available as extras. Tests pin directive spans and extra
status together with surrounding declarations and structured branches.
An inline stray-directive peek may coalesce following whitespace and comments
into inactive trivia, but it does not consume the first program character.
This preserves the written bytes, not separate documentation facts or evaluated
conditional-compilation semantics.

Same-column siblings may close a deeper declaration body while preserving the
enclosing conditional's indentation boundary. Authored controls pin declaration
siblings, attributed members and nested directive ownership under LF/CRLF and
EOF variants. They do not qualify every owner family, conditional expression
continuations, nested alternative layout, leading block-comment layout, or
complete conditional-compilation syntax.

The signature grammar inherited implementation constructors and member bodies.
Its local rules now accept written constructor/member signatures, including
static, abstract, generic and accessor forms, without implementation bodies or
primary-constructor arguments. Literal `val` declarations retain their written
constant value. These changes follow the
[F# signature-file description](https://learn.microsoft.com/en-us/dotnet/fsharp/language-reference/signature-files)
and [namespace/module signature specification](https://fsharp.github.io/fslang-spec/namespace-and-module-signatures/).
They do not establish compiler acceptance, `.fsi`/`.fs` conformance, typechecking,
name resolution, dispatch, or exhaustive F# syntax support.

Current nullness syntax has a distinct `nullable_type` node instead of invented
union constructors. Application types may carry `| null`; function and tuple
operands require parentheses, and subsequent application suffixes cannot consume
an unparenthesized nullable operand. Generic constraints distinguish `not null`
from the existing `null` and `not struct` forms. Signature value constraints
follow the complete curried specification, and structured signature conditional
branches use signature declarations rather than implementation bodies. A local
dynamic preference retains complete typed function heads without resolving names.

Noninterpolated regular, verbatim and triple-quoted string type leaves retain a
distinct `static_constant_type` node in both profiles. A non-inline triple-string
helper preserves the complete written literal rather than aliasing only one
delimiter. The lower-priority leaf keeps the existing generic static-parameter
`const` ownership when both alternatives admit the literal. This bounded subset
follows the compiler's
[raw constant type branch](https://github.com/dotnet/fsharp/blob/7e2cbf10edb6ac52b09c88214a4a39a8c774eebd/src/Compiler/pars.fsy#L6703).
Controls retain the single-clause ownership of inline nullable function-type
annotations, and reject expression-only unit and interpolated strings as these
type leaves. They do not qualify all raw constants, static providers, typing,
evaluation, operator resolution or generic multi-arm expression layout.

Flexible `#` types consume an atomic operand. Array/postfix suffixes and nullness
remain outside that operand unless written parentheses place them inside it;
constrained-type right-hand sides still accept complete types. Discriminated-union
payloads retain their application-type frontier, including parenthesized nullable
forms, instead of swallowing a case separator into a false nullable field.
These boundaries follow the
[compiler parser's type syntax](https://github.com/dotnet/fsharp/blob/7e2cbf10edb6ac52b09c88214a4a39a8c774eebd/src/Compiler/pars.fsy#L6533).
Authored controls check exact node ownership and source spans, not successful
typechecking or runtime null safety.

The shared type grammar also accepted an unmarked single RHS identifier as a
union case, creating false constructor evidence for abbreviation-shaped syntax.
Its case sequences now require a leading bar, a following case separator, or an
explicit `of`/`:` payload. This matches the
[official parser's union syntax frontier](https://github.com/dotnet/fsharp/blob/7e2cbf10edb6ac52b09c88214a4a39a8c774eebd/src/Compiler/pars.fsy#L2712).
Bare RHS names enter the abbreviation-shaped syntax branch without case-folding,
capitalization rules, primitive-type whitelists or name-specific behavior.
However, [the language specification](https://fsharp.github.io/fslang-spec/type-definitions/#85-union-type-definitions)
allows a single-name RHS to be reinterpreted as a union by later name resolution.
The patch does not prove semantic alias-target or union resolution; lowering must
preserve that limitation rather than treating a native node kind as typechecking.

Repeated enum cases preserve the compiler's bounded offside frontier: a written
case bar may sit up to two columns left of the first scoped case. An external
separator, available only through the enum grammar, is emitted before ordinary
dedentation. It aliases the existing anonymous bar rather than adding a public
node, and the internal separator remains available for recovery. Operator bars,
error-mode scanning, excessive outdent and the enclosing module floor retain
their existing behavior. The scanner snapshot codec and layout kinds are
unchanged. Controls pin direct case names and constant spellings, comments,
Unicode byte positions, signature-only declarations, incremental edit/restore
equivalence and independent following declarations. This is written syntax and
source containment, not constant evaluation or semantic enum resolution.

Parenthesized enum values retain the complete existing `paren_expression` owner
instead of failing the enum's formerly constant-only RHS. The implementation
profile delegates this enum-private route to its existing expression parser.
The signature profile admits only constants, nested parentheses, and one literal
`<<<` shift within that route, with the existing `infix_expression` and `infix_op`
node vocabulary. Its ordinary signature expression, tuple, value-body, operator,
type, and scanner rules remain unchanged. Direct case identifiers and whole-case
source spans remain intact; private enum helpers do not become public node kinds.
Authored controls cover parentheses, shifts, comments, newlines, following
declarations, recovery, queries, and incremental edit/restore equivalence. These
are bounded parse and source-ownership guarantees, not constant evaluation,
operator resolution, typing, exhaustive enum syntax, or complete F# semantics.

Constructed type qualifications retain a distinct `qualified_type` node with
direct `base` and `name` fields and optional segment-owned `arguments`.
Right-associative precedence keeps ordinary suffix arguments attached to their qualified segment.
This bounded constructed, parenthesized and recursive subset follows the
[compiler parser's atomic type qualification branch](https://github.com/dotnet/fsharp/blob/7e2cbf10edb6ac52b09c88214a4a39a8c774eebd/src/Compiler/pars.fsy#L6769).
Constructed and parenthesized bases, recursive constructed segments and flexible
type wrappers preserve their written ownership rather than flattening generic
arguments into a dotted name. Abbreviation helpers keep the existing generic
indentation scope open through the qualified head and its nullness, function or
tuple continuation. A named, universally aliased head prevents recursive fields
from leaking through hidden-node inheritance without adding a public helper node.
Plain dotted identifiers and the existing canonical type routes remain intact.
Explicit `qualified_type` queries expose these nodes in both profiles; ordinary
implementation annotations retain canonical `_type` captures. The historical
direct-abbreviation route does not provide a canonical root capture, and the
signature profile does not declare that supertype. Controls check exact fields,
spans, following declarations, query captures and bounded edit/restore equality;
malformed suffix controls require recovery, not later-declaration preservation.
These are syntax and source-ownership guarantees, not name resolution, type
checking, constant evaluation or complete language coverage.

Numeric `for` loops admit the written wildcard binder as a `wildcard_pattern`
without treating tuple, record or arbitrary patterns as numeric-loop binders.
The bounded identifier/wildcard subset follows the
[compiler's numeric-loop parser branch](https://github.com/dotnet/fsharp/blob/7e2cbf10edb6ac52b09c88214a4a39a8c774eebd/src/Compiler/pars.fsy#L5749).
Both ascending and descending forms preserve their direct binder, bounds and
body ownership. The wildcard produces no invented module-level declaration.

Loop and plain `do` expression bodies use a dedicated external indentation mark.
An explicit written `done` closes the nearest marked body through ordinary
dedentation before emitting its existing anonymous tail. Delimited type,
parenthesized and braced scopes remain barriers. Declaration and expression
`do` routes share the body mark so lexical admission cannot eliminate the
declaration before its existing precedence decides ownership; the declaration
route does not acquire a `done` tail. Conditional-module controls retain distinct
declaration, module, import, loop and following-binding owners. Numeric, foreach,
while and nested expression controls check exact tails, source bytes and bounded
incremental edit/restore equivalence, not evaluation or compiler name resolution.

The two external additions follow the existing 41-token prefix. Snapshot framing
and capacity remain unchanged, while the validated kind domain adds marked
`do` frames and their existing midline/stranded combinations. Old frames remain
readable; an older reader rejects the added kind instead of decoding lost state.
Native tree handles remain grammar/provider-private: cross-grammar snapshot or
tree reuse is not an interoperability guarantee. Written keywords, recovery
mode, barriers and native-created round-trip states have direct scanner controls.

`done` is reserved in both profiles. The signature grammar uses a hidden,
fixed-spelling regex with lexical priority to retain the keyword token without
introducing implementation bodies or public helper nodes. Bare keyword names
require recovery; longer names, backtick escapes and the qualified Unicode
suffix controls remain identifiers. The scanner's conservative word boundary
does not claim a complete Unicode identifier classifier. Parse-only checks from
the pinned SDK compiler independently confirm the authored conditional, numeric
and keyword fixtures; they neither execute/typecheck the strings nor establish
compiler equivalence for all F# source.

The ten overrides are the scanner, both preferred grammars, both generated
parser/grammar-JSON pairs, both node-type inventories, and one Rust binding
line-wrap required by workspace Rustfmt. The binding's tokens and all its other
bytes remain unchanged. The `tree_sitter/{alloc,array,parser}.h` headers, build
script and license remain byte-exact. Original scanner and
both preferred grammar inputs are also retained as `.orig` files. Both profiles
were initially generated with the source-built `tree-sitter-generate` 0.27.0 API, ABI 15 and
Node.js 24.11.1 as a declared preparation tool. All six original outputs for each
profile were reproduced byte-for-byte, then all twelve outputs for that earlier patch state were
independently regenerated byte-for-byte. Preparation tool hashes and the limits
of its source-closure evidence are recorded in the provenance file.

Compilation uses checked-in C and headers with the versioned `cc` and
`tree-sitter-language` inputs. It downloads and generates nothing; Node.js and
the generator are not native compilation inputs. The authored native tests cover
exact spans, malformed input and recovery, signature body exclusion, incremental
edits, cancellation, wide indentation, complete scanner state, serialization
bounds, nested comments and embedded NUL. These tests are not installed MCP,
whole-corpus coverage, full language semantics, or empty-cache OS-network-denied
build acceptance. Remove each override when a pinned upstream release passes
the same authored regression controls without it.

## Historical flat sequencing, committed roots and qualified callees

The earlier bounded syntax repair flattens sequential expressions without
inventing nested owners, retains genuine application and offside frontiers, and
keeps written double-semicolon declaration boundaries attached to their committed
module-level value bodies. Whole qualified operator and active-pattern callees
remain existing public identifier-expression owners, including their written
qualified prefix; plain dotted fields, patterns and declaration names retain
their existing routes. No source name, repository identity or corpus ordinal
selects production behavior.

Five additions follow the retained 43-token external prefix without renumbering
it. At that patch state, the qualified-callee DOT is ordinal 47 of 48. It aliases the existing
anonymous dot, consumes exactly that one byte and inspects the parenthesized
tail without committing its source bytes. Older layout, recovery, inside-string
and type-application admissions retain their priority. Generated non-error
qualified-DOT states exclude type-application indentation; the all-valid error
row remains guarded. Failed lookahead may inspect input, but must not publish a
DOT or mutate serialized scanner state. Direct tests use UTF-8 byte spans and
skip-aware token-start/mark-end behavior rather than treating inspected bytes as
consumed source.

Snapshot codec 2 adds explicit committed-root state while accepting validated
historical codec-1 frames without inventing a root owner. The snapshot capacity
remains 1,024 bytes. The final stateless DOT itself adds no serialized field and
does not require another codec change. Native parser handles and scanner states
remain provider-private; no cross-language tree compatibility is claimed.

Both six-output canonical profiles were freshly reproduced byte-exact with the
same reviewed producer and declared Node tool. Only the implementation parser C
is subsequently compacted: consecutive leading ASCII 0x20 bytes are removed
from each line, preserving all other bytes and all 3,442,311 LF bytes. Two actual
transforms agreed on the 100,600,902-byte compilation payload, removing exactly
15,377,744 spaces from the 115,978,646-byte canonical C. The signature C and both
grammar/node inventories remain canonical bytes. This deterministic formatting
is not permission to change parse tables or numeric parser limits.

Actual isolated qualification completed the exact 136 native, 63 retained scanner
and 14 corrected qualified-DOT groups. The original DOT companion's 12/14 outcome
is preserved; its two failures occurred during a midline donor setup before the
DOT assertions. The corrected newline donor reached and passed those assertions
without changing product scanner code. The durable relocation keeps every test
body, name, assertion, watchdog and snapshot capacity; only include paths and
the native target registration change. Earlier native qualification is retained
as historical evidence rather than silently rewritten.

These results cover authored parse-only source, direct ownership, raw query
captures and bounded incremental edit/restore contracts. They do not establish
typechecking, name resolution, constant evaluation, complete F# semantics,
repository-wide 100 percent coverage, installed MCP behavior or indexing speed.
Durable target execution after production adoption, complete preferred-producer
source closure, and empty-cache independently OS-network-denied compilation and
packaging remain separate gates. Compilation still uses checked-in local C,
scanner and headers; it must not generate tables or acquire source code.

## Historical numeric literal continuations

The numeric repair retains complete written literal owners rather than allowing
ordinary application lookahead to claim an adjacent literal suffix, exponent or
digit continuation. It does not choose behavior by repository identity, source
name or corpus ordinal. Numeric continuation and ordinary trivia-separated
application remain distinct syntax routes; recovery, layout, string and older
external tokens retain their prior admission and priority.

Both profiles keep external ordinals 0 through 47 unchanged and append five inert
context guards: integer-literal suffix 48, based-literal suffix 49, float-literal
suffix 50, numeric exponent continuation 51 and numeric digit continuation 52.
The complete FFI valid-symbol domain is now 53 flags. These guards contribute
context only and are never published as scanner result symbols. Qualified-callee
DOT remains ordinal 47. The scanner's existing buffered word probe checks numeric
continuations, including uppercase `UL`, before publishing an ordinary
application marker; this repair adds no encoded state. Snapshot codec 2,
the 1,024-byte snapshot capacity, ABI 15 and production resource caps are unchanged.

The preferred implementation grammar also admits an empty fractional digit run
only when followed by a required exponent, as in the written literal `1.e2`.
This is a general F# syntax repair, not a waiver of malformed-source controls.
The signature preferred grammar, scanner shims, headers, Rust binding, license,
original inputs and retained upstream archive remain byte-exact.

Fresh generation completed both six-output profiles with the reviewed producer.
An independent source review authenticated the outputs and exact 53-ordinal
maps, preserved the old 48-token prefix, and inspected action-index domains and
guard/application co-validity. A separate actual fresh generation reproduced all
twelve canonical outputs for that numeric patch state byte-for-byte; another
actual compaction matched the implementation C bytes already qualified natively.
Earlier byte-reproduction
proofs remain historical and are not reused as current proof. Complete versioned
preferred-producer source closure remains unverified.

Two actual prefix-space-only compactions agreed on the 102,633,890-byte compiled
implementation parser from the 118,283,208-byte canonical parser. They removed
15,649,318 leading ASCII spaces while conserving all 3,504,302 LF bytes and
every other byte. Signature C and both grammar/node inventories remain exact
canonical generation outputs. This retains the existing 104,857,600-byte
generated-source cap, with 2,223,710 bytes remaining for the compact payload;
formatting alone makes no compiler or runtime behavior claim.

Actual isolated native qualification completed all thirteen exact target rosters:
319 of 319 groups passed, without failed or skipped groups. This includes the
retained 136 sequence groups, 67 direct scanner groups, 14 qualified-DOT groups,
nine new numeric continuation groups, and the unchanged seven enum atomic-RHS
groups. Both scanner FFI roots and their helper assertions use all 53 flags.
The corrected include fragment changes only two invalid inner doc comments to
ordinary comments; test bodies, names, masks, watchdogs and the 319-group roster
are preserved. The earlier 316/319 native outcome and earlier include-fragment
compilation failure remain historical evidence rather than being overwritten.

The previous successful 319-group native run is also preserved intact as
historical before-quality evidence. Its subsequent formatting and Clippy
diagnostics were terminal observations without persisted quality receipts;
they are not assigned invented proof hashes or represented as current
warnings-qualified evidence.

The quality-qualified numeric snapshot covered the exact nine test-source overlays.
Only the numeric test's tuple type aliases and formatting, and the equivalent
rule/pattern let-chain in the sequence controls, were needed for source quality.
Fixture values, behavioral assertion conditions, test names, watchdogs and the
319-group roster remain unchanged. Actual Cargo formatting with `--all --check`
and offline locked Clippy with `--all-targets -- -D warnings` both closed
successfully against the same prepared source snapshot as that complete numeric
native run. All 1,225 quality-guarded inputs were conserved. Formatting stderr
is empty; Clippy stderr contains only successful compilation announcements,
not MCP transport evidence. That numeric native process has its own authenticated
process and stream hashes even though the two successful native runs' summary
and result files are byte-identical.

Provenance binds the exact actual generation, independent source-review,
compaction and closed isolated-native receipts. Publication metadata is prepared
after those isolated executable-input qualifications. Durable native execution
after production adoption, public source/query/cache checks, installed MCP
behavior, indexing performance, complete language semantics and empty-cache
independently OS-network-denied build/packaging acceptance remain separate
uncompleted gates. Compilation still uses only reviewed local checked-in C,
scanner, headers and declared local dependencies; it must neither generate
tables nor acquire code.

## Reciprocal measures and offside declaration boundaries

Written reciprocal units of measure may begin with a slash without a written
numerator. The shared measure quotient admits that existing syntax directly;
authored controls require the denominator's exact operand and source ownership
and reject an invented numerator. Ordinary binary products, quotients and
explicit dimensionless numerators retain their distinct written operands.
Numeric suffixes, generic arguments, comments, parentheses, powers and type
variables have bounded controls in both implementation and signature profiles.
This is parse and source-ownership evidence, not dimensional typing, unit
normalization, numeric evaluation or complete units-of-measure support.

The signature profile admits measured numeric constant annotations through a
hidden numeric-only route, not the implementation expression grammar. Its public
node inventory adds the existing implementation carrier `typed_expression`;
the implementation node inventory is byte-exact with the preceding numeric
patch state. No hidden helper becomes a public node. Both profiles reserve
complete written `let` and `val` keywords, while backtick escapes and complete
identifier prefixes retain their source identity. Signature keyword admission
does not introduce implementation declaration bodies. Positive controls preserve
member properties, mutable fields and static fields with their written carrier,
name, type and following declaration. They establish syntax only, not compiler
initialization requirements, typechecking or dispatch.

The scanner admits an ordinary requested indentation mark before a same-line
slash while preserving its existing token frontier and scope flags. A line
comment still declines that admission rather than consuming its slash bytes as
a measure. Extra-only offside lookahead can close the appropriate ordinary owner
before a complete following `let` or `val` declaration, without consuming the
keyword as a denominator. Incomplete keyword prefixes, operator continuations,
recovery masks, strings and preprocessor barriers retain bounded negative
controls. Native-created snapshot replay, edit/restore and following-declaration
checks exercise both repairs. These bounded controls do not guarantee recovery
after every malformed source or define a complete Unicode identifier classifier.

No external token is added or renumbered by these repairs. ABI 15, all 53 valid
symbol flags, qualified-callee DOT ordinal 47, the five inert numeric guards,
snapshot codec 2 and the 1,024-byte snapshot capacity remain unchanged. Existing
production resource caps, authored watchdogs and all 319 original test names are
preserved. Eight scanner controls and twelve reciprocal/keyword cases complete
the 339-test roster. Repository names, paths, source identifiers and corpus
ordinals do not select production behavior.

Two fresh independent generation passes reproduced all twelve canonical outputs
byte-for-byte with the same authenticated preparation producer and declared Node
tool. Only implementation parser C is compacted afterward. Two actual
prefix-space-only transforms agreed on the 102,676,775-byte compiled payload
from the 118,333,579-byte canonical payload, removing 15,656,804 leading ASCII
spaces while conserving all 3,505,740 LF bytes and every other byte. Signature C
and both grammar/node inventories retain their exact canonical generated bytes.
The unchanged 104,857,600-byte generated-source cap leaves 2,180,825 bytes for
the compact payload. Formatting alone makes no compiler or runtime claim.

The authored reciprocal and scanner additions were copied and transformed by
the authenticated Rust formatter before this qualification. Raw sources,
historical rosters and their include-only derivations remain separately pinned.
The formatted reciprocal root contains the exact 28,415-byte formatted baseline
plus only its original 49-byte include suffix; it is not claimed to retain the
raw 24,124-byte prefix. The formatter derivation's string and function-head text
comparisons are not a Rust AST or universal semantic-equivalence proof.

Actual direct Rustfmt with edition 2024 and `--check` closed successfully over
all fifty staged Rust buffers, including fragments and both unchanged grammar
binding files. Offline locked Clippy closed successfully with `--no-deps`,
fourteen explicit test-target selectors and `-D warnings`. This is not a
`cargo fmt --all` or blanket `--all-targets` observation. The same prepared
snapshot then completed all fourteen exact native target rosters: 339 of 339
tests passed, with no failures, ignored tests, measured tests or filtered tests.
The scanner target retains 75 controls and the reciprocal target adds twelve;
all other retained target rosters remain exact. Processes and asynchronous byte
streams closed with owned-child cleanup and conserved authenticated inputs.
Formatting streams are empty; Clippy and native stderr contain successful
build/check/run announcements, not MCP transport evidence.

`SOURCE-PROVENANCE.json` preserves the preceding numeric generation, compaction,
quality and native records under historical keys and binds these new actual
observations by SHA-256 identities. This isolated authored-source qualification
does not prove durable native execution after publication, installed MCP
behavior, repository-wide language coverage, indexing speed, full F# semantics,
complete preferred-producer source closure or empty-cache independently
OS-network-denied build and packaging acceptance. Compilation continues to use
reviewed local checked-in C, scanner, headers and declared dependencies; it must
neither generate tables nor acquire source code.

## Conditional namespace headers

Top-level conditional branches now admit a namespace header through a hidden
branch-bounded rule alongside the existing conditional module rule. The rule
preserves the public `namespace` carrier, written name and branch-owned syntax;
it does not consume a root separator, select a compilation-symbol arm or assign
post-conditional declarations to an evaluated namespace. The signature profile
inherits this rule with its own declaration elements. Both parser profiles and
their grammar/node inventories were regenerated, not edited independently.

Two fresh paired generation passes reproduced all twelve canonical outputs
byte-for-byte with the authenticated Tree-sitter-generate 0.27.0 / ABI 15 helper
and Node.js 24.11.1. Prefix-space-only compaction yields a 102,841,201-byte
implementation parser from 118,517,175 canonical bytes, removing 15,675,974
leading ASCII spaces and conserving all 3,510,501 LF bytes and every other byte.
Signature C remains its 14,348,682-byte canonical output. The unchanged
104,857,600-byte generated-source cap, all 53 external-token ordinals, scanner
codec 2, snapshot capacity and scanner source remain unchanged.

Rootlight retains conditional source scopes as mandatory non-entity barriers.
Exact own-formal proofs can remain available after a clean parse, while
conditional namespace selection and branch-dependent lookup retain explicit
source-scoped semantic limitations. A clean parse is not a claim of active-arm
evaluation, compiler acceptance, complete structural support for every inactive
branch, full language coverage or complete producer source self-containment.
The preceding reciprocal native/quality observations are historical evidence,
not a qualification of these new tables. Current observed verification is
recorded separately in `SOURCE-PROVENANCE.json`.

The published tables completed the fourteen retained native target rosters:
339 tests passed with no failures or ignored tests. The adapter completed 298
unit tests and 72 F# source/analysis tests, including both authored namespace
headers, signature contracts, exact independent formal reads, scoped semantic
gaps, branch barriers and malformed recovery. All 32 F# service tests and two
recipe invalidation tests passed. Selected-package and fourteen-target native
Clippy denied warnings; selected workspace and native-package Cargo formatting
checks passed. These observations use locked local dependency inputs and a warm
build cache. They do not replace installed MCP, full eligible-source accounting,
comparable indexing performance or independently network-denied empty-cache
build and packaging acceptance.

## Adjacent abstract signature members

The signature profile reserves the complete `abstract` keyword so that a
following member declaration cannot become a postfix type argument of the
preceding member. Backtick-quoted names remain identifiers. The repair changes
neither type precedence nor method/property classification: authored argument
specifications and parenthesized function-valued properties keep their native
distinction and exact source boundaries.

Two fresh signature-only generation passes reproduce all six canonical outputs
byte-for-byte with the same authenticated Tree-sitter-generate 0.27.0 / ABI 15
helper and Node.js 24.11.1. Only the signature preferred grammar, generated grammar
JSON and parser C change. Public node types, headers, the shared scanner and all
implementation outputs remain byte-exact. Implementation C retains its preceding
compaction proof; it was not regenerated for this change. External token ordinals,
scanner codec, snapshot capacity and generated-source limits remain unchanged.

The new signature tables passed the fourteen retained native target rosters
(339 tests), focused query units (47), F# adapter tests (91) and F# service tests
(41), without failures or ignored tests. The adjacent-member controls cover
quoted keyword names, function-valued properties, both newline styles and exact
source reads across mutation and durable reopen. Selected adapter, service and
tooling packages passed all-target, all-feature Clippy with warnings denied and
Cargo formatting checks. Unchanged native Rust quality observations remain
historical evidence rather than new executions.

Compilation continues to use checked-in local C and headers without generation
or source downloads. These focused checks use a warm build cache; they do not
establish independently network-denied empty-cache build/package acceptance,
installed MCP qualification or full-language coverage.
