# F# scanner state, signatures, type syntax and loops

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
were generated with the source-built `tree-sitter-generate` 0.27.0 API, ABI 15 and
Node.js 24.11.1 as a declared preparation tool. All six original outputs for each
profile were reproduced byte-for-byte, then all twelve patched outputs were
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
