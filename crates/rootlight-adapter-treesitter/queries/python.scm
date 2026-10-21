; Reviewed Tier C/D structural evidence for the pinned Python grammar.
; Captures are deliberately syntax-only and never imply resolved semantics.

(module) @root @module @signature

[
  (function_definition)
  (class_definition)
] @declaration

[
  (function_definition name: (identifier) @definition)
  (class_definition name: (identifier) @definition)
]

; Only direct named-function formal binders declare parameters. Native name
; fields keep annotation/default expressions and lambda binders out of identity.
(function_definition
  parameters: (parameters
    (identifier) @declaration @definition))

(function_definition
  parameters: (parameters
    (typed_parameter
      . (identifier) @definition) @declaration))

(function_definition
  parameters: (parameters
    (default_parameter name: (identifier) @definition) @declaration))

(function_definition
  parameters: (parameters
    (typed_default_parameter name: (identifier) @definition) @declaration))

(function_definition
  parameters: (parameters
    (list_splat_pattern (identifier) @definition) @declaration))

(function_definition
  parameters: (parameters
    (dictionary_splat_pattern (identifier) @definition) @declaration))

(function_definition
  parameters: (parameters
    (typed_parameter
      (list_splat_pattern (identifier) @definition)) @declaration))

(function_definition
  parameters: (parameters
    (typed_parameter
      (dictionary_splat_pattern (identifier) @definition)) @declaration))

; Assignment capture is only a candidate; native whole-file proof admits
; a unique simple module binder and records all other binding gaps explicitly.
(module (expression_statement (assignment) @declaration))
(module (expression_statement (assignment left: (identifier) @definition)))
(assignment) @signature
(augmented_assignment) @signature

[(import_statement) (import_from_statement)] @import
; The required context witness records the limits of this lexical subset.
(function_definition) @signature
(parameters) @signature
(block) @scope
(call
  function: [
    (identifier)
    (attribute
      attribute: (identifier))
  ] @call)
; Pairing keeps certified references in identity extraction. The separate
; reference pattern preserves every unproved optional source use.
(identifier) @scope @reference
(identifier) @reference
(comment) @comment

(module
  . (expression_statement (string) @documentation))

(function_definition
  body: (block
    . (expression_statement (string) @documentation)))

(class_definition
  body: (block
    . (expression_statement (string) @documentation)))

(string) @string
