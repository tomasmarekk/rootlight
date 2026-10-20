; highlights.scm — Q# syntax highlighting
;
; Capture names follow the nvim-treesitter standard:
; https://github.com/nvim-treesitter/nvim-treesitter/blob/main/CONTRIBUTING.md
;
; Editors map these captures to colors via themes:
;   Neovim  — uses @capture names directly
;   Zed     — maps to its own theme scopes
;   Helix   — maps with fallback (e.g. @keyword.import -> @keyword)

; =====================================================================
; Comments
; =====================================================================

(comment) @comment
(doc_comment) @comment.documentation

; =====================================================================
; Literals
; =====================================================================

(integer_literal) @number
(bigint_literal) @number
(float_literal) @number.float
(imaginary_literal) @number.float

(string_literal) @string
(interpolation_content) @string
(string_interpolation
  "$\"" @string
  "\"" @string)
(interpolation_expression
  "{" @punctuation.special
  "}" @punctuation.special)

(bool_literal) @boolean
(result_literal) @constant.builtin
(pauli_literal) @constant.builtin

; =====================================================================
; Keywords — categorized per nvim-treesitter standard
; =====================================================================

; Type-definition keywords
["struct" "newtype"] @keyword.type

; Function-definition keywords
["operation" "function"] @keyword.function

; Import/export keywords
["import" "export" "open"] @keyword.import

; Storage / binding keywords
["let" "mutable" "set" "use" "borrow"] @keyword

; Visibility modifier
"internal" @keyword.modifier

; Control flow — conditionals
["if" "elif" "else"] @keyword.conditional

; Control flow — loops
["for" "in" "while" "repeat" "until" "fixup"] @keyword.repeat

; Control flow — return / fail
"return" @keyword.return
"fail" @keyword.exception

; Conjugation (quantum-specific control flow)
["within" "apply"] @keyword

; Other keywords
["namespace" "as" "new" "is"] @keyword

; Specialization keywords
["body" "adjoint" "controlled"] @keyword
["auto" "distribute" "intrinsic" "invert" "self"] @keyword

; =====================================================================
; Operators
; =====================================================================

; Arithmetic
["+" "-" "*" "/" "%" "^"] @operator

; Comparison
["==" "!=" "<" "<=" ">" ">="] @operator

; Logical (keyword-style operators)
["and" "or" "not"] @keyword.operator

; Bitwise
["|||" "&&&" "^^^" "~~~" "<<<" ">>>"] @operator

; Assignment
["=" "+=" "-=" "*=" "/=" "%=" "^="] @operator
["&&&=" "|||=" "^^^=" "<<<=" ">>>=" "and=" "or="] @operator

; Copy-and-update
["w/" "w/=" "<-"] @operator

; Arrow / lambda
["->" "=>"] @operator

; Range
[".." "..."] @operator

; Ternary
["?" "|"] @operator

; Unwrap
"!" @operator

; =====================================================================
; Punctuation
; =====================================================================

["{" "}" "(" ")" "[" "]"] @punctuation.bracket
[";" "," ":" "::" "."] @punctuation.delimiter
"@" @punctuation.special

; =====================================================================
; Types
; =====================================================================

(builtin_type) @type.builtin

(type_param
  (apos_ident) @type)

(functor_expr
  ["Adj" "Ctl"] @type.builtin)

; Type in callable type arrows
(callable_type ["->" "=>"] @punctuation.delimiter)

; Qubit in initializer
(qubit_init "Qubit" @type.builtin)

; =====================================================================
; Declarations
; =====================================================================

; Namespace name
(namespace
  name: (path_or_identifier) @module)

; Callable name
(callable_decl
  name: (identifier) @function)

; Struct / newtype name
(struct_decl
  name: (identifier) @type)
(newtype_decl
  name: (identifier) @type)

; Parameters
(typed_param
  name: (identifier) @variable.parameter)

; Struct field definitions
(field_def
  name: (identifier) @variable.member)

; Attributes
(attribute
  (path_or_identifier) @attribute)

; =====================================================================
; Expressions
; =====================================================================

; Function / operation calls
(call_expression
  (identifier) @function.call)
(call_expression
  (path
    (identifier) @function.call .))

; Field access
(field_expression
  field: (identifier) @variable.member)

; new Struct { ... }
(new_expression
  (path_or_identifier) @type)

; Field assignment in struct expression
(field_assign
  name: (identifier) @variable.member)

; Functor application
(unary_expression
  ["Adjoint" "Controlled"] @function.builtin)

; =====================================================================
; Variables
; =====================================================================

(let_statement
  (pattern (identifier) @variable))

(mutable_statement
  (pattern (identifier) @variable))

(for_expression
  (pattern (identifier) @variable))

(discard) @variable.builtin

; =====================================================================
; Imports
; =====================================================================

(import_path
  (identifier) @module)

(import_path
  "*" @character.special)
