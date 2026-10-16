# Retained Rocq grammar modifications

All 91 authored files at revision `300fe33fc299c30f736fd56d8ef8a28b08acd4e6`
remain byte-exact at the locations recorded in `upstream/retained-source-inventory.json`.
The complete archive, authored declarations and original executable intent remain
preserved. The preferred Rust binding is local and unpublished.

The named module overlay follows the [Rocq 9.2 module syntax](https://rocq-prover.org/doc/V9.2.0/refman/language/core/modules.html).
It retains module and module-type names, Import/Export modifiers, functor binders,
type constraints, aliases, module applications, explicit inline annotations and
Include combinations. Body expressions contain qualified module names and
parenthesized atoms; module types also retain parenthesized applications. Prefix
no-inline and suffix inline annotations are separate alternatives. These rules do
not accept arbitrary vernacular text. `with`
constraints and module elaboration remain separate applicable gaps; this syntax
does not prove functor application, visibility or type compatibility.

The Context overlay follows the [section command](https://staging.rocq-prover.org/doc/V9.1.0/refman/language/core/sections.html)
and [generalizing binder syntax](https://rocq-prover.org/doc/v9.0/refman/language/extensions/implicit-arguments.html).
It retains ordinary binders and named or anonymous typeclass constraints inside
backtick delimiters, including the explicit no-generalization mark and comma lists.
Named constraints have their own source name and type fields. Accepting their
syntax does not establish inferred parameters, typeclass search or section discharge.

The Instance overlay follows the [Rocq 9.2 typeclass commands](https://rocq-prover.org/doc/V9.2.0/refman/addendum/type-classes.html).
Named Instance headers retain parameters, an explicit type, optional numeric hint
priority and either a written body or a proof-form header. Declare Instance has a
distinct node for signature requirements; Existing Instance/Instances retain only
existing reference paths. Existing Class has a separate registration node and
does not introduce a new class declaration. These are syntax nodes, not evidence that an instance
has been elaborated, completed, registered or found by typeclass search. Hint
patterns, anonymous declarations and dynamically extended forms remain applicable
gaps.

Instance field shorthand retains exact field paths, optionally typed binders and
assigned terms. Proper `{| ... |}` record construction now replaces the original
generic DSL alternative for those delimiters; incomplete field assignments cannot
be hidden by that alternative. Field assignments refer to existing fields and do
not introduce global projections. The other original generic DSL forms remain
explicitly incomplete. The [Rocq 9.2 tactic-term syntax](https://rocq-prover.org/doc/V9.2.0/refman/language/extensions/evars.html)
has a named `ltac_term` with explicit delimiters and the retained tactic-expression
entry. That node does not execute tactics or check the resulting term or proof.

Record and Structure now have a named declaration with an optional written
constructor and ordered field nodes. Class uses either a braced declaration or
a distinct singleton projection node; the singleton projection is not a
constructor. Field types, binders and assigned values keep separate source
boundaries. Class fields can carry numeric priorities; Record has no priority
production. The original generic operator grammar can still parse an unknown
type notation such as `nat | 3`, which is not evidence of a Record priority.
Anonymous fields and omitted constructors do not supply authored projection or
constructor names. Field attributes, dynamic notation, sort elaboration,
generated projections and typeclass registration require separate analysis.

The [Rocq 9.2 projection grammar](https://rocq-prover.org/doc/V9.2.0/refman/language/core/records.html#accessing-fields-projections)
now has a named `term_projection` with separate object, projection name and
argument nodes. It retains qualified fields, ordinary or explicit arguments and
chained projections. Named arguments keep their written argument name separate
from the value reference. The dot and opening parenthesis form the single `.(`
token; a period followed by whitespace is a sentence boundary. Universe
annotations are not implemented by this slice and remain applicable syntax gaps.
Projection syntax does not establish a field target, elaborated type or a
semantic projection relation. Existing generic notation and tactic recovery can
still lose command boundaries; these nodes do not qualify complete recovery.

The [Rocq 9.2 tactic-notation command](https://rocq-prover.org/doc/V9.2.0/refman/user-extensions/syntax-extensions.html#tactic-notations)
retains ordered literal and parameterized nonterminal items, optional precedence
levels zero through five and a separate Ltac body. Parameter items retain their
entry, parameter and optional separator string; unsupported unbound or dynamically
extended entries remain applicable gaps. A notation literal is not a declaration
name. The [Ltac match syntax](https://rocq-prover.org/doc/V9.2.0/refman/proof-engine/ltac.html#pattern-matching)
has separate value, pattern and body fields for the bounded single-term and
`type of` forms. These nodes neither install dynamic syntax nor execute or check
tactics. Other Ltac values, binding forms and recovery boundaries remain partial;
the existing unsupported-sentence and semantic gaps still apply.

`patches/grammar.patch` contains the exact grammar changes. The ABI15 C tables,
node schema and headers are generated from only local `grammar.js` and
`tree-sitter.json` by the pinned Tree-sitter 0.26.8 maintenance CLI with its native
JavaScript runtime. Two independent fresh directories produced identical outputs,
recorded in `patches/grammar-regeneration.json`. No generated C or schema is edited
by hand. Node, source downloads and package lifecycle hooks are not invoked.
The CLI is a declared build-tool prerequisite; rebuilding its CLI/QuickJS source
closure locally is not established by this record. Compilation and packaging
consume retained C sources and never regenerate them.

The original grammar has broad generic tactic syntax and incomplete core and
plugin support. Those existing fallback nodes are not declaration proof. Unimplemented
record/class variants, instance variants, advanced terms, plugin
commands, dynamic notation and proof
checking remain applicable obligations. Finite native controls and all-file parser
observations do not qualify complete Coq structure or semantics, production queries,
installed-package behavior, empty caches or independently blocked networking.
