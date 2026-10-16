# Retained Eiffel source modifications

The complete authored revision `ec098b67b98b7f5da10f8d66bdb6c52fc91341fc`
from `imustafin/tree-sitter-eiffel` remains unchanged under `upstream/`, with its
original archive and all 69 files authenticated against the exact Git tree and
blob metadata. This revision contains unreleased changes after the upstream
`v1.0.0` tag; its manifest version remains 1.0.0. The local binding is unpublished
and does not claim to be a crates.io, npm or released source package.

Original ignore and attribute files use `.upstream` suffixes so they cannot
override this repository's source retention policy. Archive modes and executable
intent remain recorded independently of Windows filesystem permissions. No
upstream installation, lifecycle hook or Eiffel code runs.

The preferred grammar contains a reviewed lexical overlay for binary, octal and
hexadecimal integers and ordinary, escaped and numeric character constants.
`patches/grammar.patch` records all differences from the exact authored grammar.
Real components remain decimal. Character payload and closing quotes use immediate
tokens so grammar whitespace cannot swallow a space character or permit extra
characters inside a literal. The lexical forms follow the
[Eiffel language syntax](https://www.eiffel.org/doc/eiffel/Eiffel_programming_language_syntax).

Declaration separators apply after fields and constants as well as routines.
The shared suffix retains individual written heads and includes every separator
in its native declaration range; it does not admit a missing constant value.

Reserved and contextual words accept ASCII letter case according to the
[ECMA-367 Case Insensitivity principle, clause 8.2.19](https://ecma-international.org/wp-content/uploads/ECMA-367_2nd_edition_june_2006.pdf).
Named internal tokens keep global reserved-word admission; anonymous aliases
retain the existing public token kinds. Written identifier spelling, strings and
comments retain their exact bytes. Case-insensitive name resolution remains a
separate product obligation.

Multiple aliases in feature declarations and inherited renamings follow the
[EiffelStudio 19.12 compiler extension](https://www.eiffel.org/doc/eiffelstudio/Release_notes_for_EiffelStudio_19.12).
The standard ECMA-367 (2006) syntax shows a single alias; this overlay explicitly
retains the later implementation form. A `frozen` prefix may precede `deferred`,
`expanded` or `once`, following the class-header syntax implemented by Gobo.
These combinations preserve one native header-mark node and exact feature names.
They do not establish alias dispatch or inheritance resolution. The native
contracts retain failures for repeated or reversed marks and incomplete aliases.

Note entries retain optional separators according to ECMA-367 clauses 8.2.21
and 8.4.3. Written metadata values remain inert. A note after an attribute can
belong to the class or introduce a routine body; an explicit declaration conflict
keeps both parses available until the following body or class end resolves it.
Dynamic declaration precedence retains the preferred complete declaration without
prematurely rejecting a trailing class note. Native contracts check both owners,
incremental edits and malformed missing note values.

Type decorations retain attachment before separation for class, tuple and anchored
types. Expanded class types and unique attributes follow the pinned Gobo parser
productions. Address expressions include `Result` under ECMA-367 clauses 8.19.8
and 8.28.1; `Current` follows the retained Gobo implementation form. The latter
is distinguished from the standard Variable-only address rule. The public native
nodes retain their written spans, and unique attributes retain explicit-value
classification. These syntax forms do not establish runtime addresses, values,
SCOOP processor binding or resolved calls.

List separators stay attached to their grammatical positions. ECMA-367 second
edition clause 8.2.21 permits omitted separators and a trailing semicolon in
semicolon-separated repetitions. Leading and repeated semicolons follow the
retained Gobo parser's recursive `Semicolon` production and its list-specific
optional prefixes. This implementation syntax is preserved for inheritance,
exports, notes, feature declarations, formal arguments, locals, labeled tuples
and assertions. Empty argument and tuple lists also preserve written syntax.
Semicolons are never global extras: they remain invalid inside identifiers,
actual argument lists and comma-separated generic arguments. These forms do not
establish contract truth, inherited bindings or tuple-field semantics.

Inline separate instructions accept the current documented `do` body delimiter
and retain the original upstream `then` form for syntax compatibility. The
current form follows Eiffel Software's Exclusive Access documentation and the
retained Gobo `Inline_separate_instruction` production. Written aliases are
read-only local names; accepting this syntax does not prove controlled-region
identity, exclusive access, reference binding or runtime concurrency semantics.

Real literals follow ECMA-367 second edition clause 8.32.31: decimal parts
surround a required dot, at least one part is present, and an optional `e` or `E`
exponent contains an optional sign and decimal digits. Components cannot contain
intervening whitespace. The pinned Gobo scanner agrees with these lexical forms.
Signed exponent trees retain their original named sign and integer components;
unsigned exponents are a single named `real_exponent` token so an identifier such
as `e6` cannot consume their spelling. Numeric value evaluation is not inferred.

Formal generic parameters accept the `expanded` and `reference` representation
marks described by the pinned Gobo `Formal_parameter` production, with optional
constraints. These are implementation forms, distinct from the `frozen` form in
ECMA-367 second edition clause 8.12.2. `reference` remains contextual so existing
unmarked parameters or features with that name retain their original spelling.
Written parameter identities do not prove representation, constraint validity,
generic substitution or conformance; the adapter records that binding gap.

The ABI15 parser, node schema and support headers are generated from local inputs
with the declared Tree-sitter 0.26.8 maintenance tool and its native JavaScript
runtime. Two fresh-directory replays produced identical outputs, recorded in
`patches/grammar-regeneration.json`. No generated C or schema is hand edited.
Compilation and packaging consume these checked-in files without generation.
A stateless external scanner retains free-operator spans without consuming comment
prefixes or the delimiter pairs `->`, `<<` and `>>`. One external token covers
modern printable symbolic operators; original `#`, `|` and `&` ASCII-tail forms
remain syntax compatibility, including whole `|<<`, `|>>` and `#abc->` spellings.
Those legacy tails stop before `--` comments; arrow and shift-pair splitting
applies only to modern symbolic operators. Reserved builtins keep their existing native nodes.
The scanner performs no allocation, serialization or Eiffel evaluation. The exact
original parser,
headers and grammar remain under `upstream/authored-files` and in the archive.

The scanner's Unicode ranges derive from complete retained regex-syntax 0.8.11
Unicode 16.0.0 category data, with both original MIT and Apache-2.0 notices in
`patches/unicode`. Mark, Number, Punctuation and Symbol categories provide
printable non-letter candidates; ASCII digits and Eiffel punctuation are excluded.
`patches/generate_operator_symbols.py` and its regeneration record preserve exact
inputs and two identical fresh-directory outputs. This maintenance command uses
Python and is never run during compilation or packaging. Gobo's pinned scanner
trailing-context rules motivate keeping both comment/delimiter characters outside
the operator span. Written operators do not establish alias dispatch or targets.

`patches/local-binding.patch` describes the local Rust manifest and static C11
binding, using already versioned `tree-sitter-language` 0.1.7 and `cc` 1.2.67 with
the declared Rust 1.90 and C toolchain.

Native syntax contracts are finite evidence. Tagged and brace verbatim strings and
other unimplemented syntax remain applicable grammar gaps. Native parsing does not
establish product reference resolution, whole-corpus coverage, installed MCP
behavior or empty-cache independently network-blocked package acceptance.
