; Reviewed Tier C/D structural evidence for the pinned Python grammar.
; Captures are deliberately syntax-only and never imply resolved semantics.

(module) @root @module

[
  (function_definition)
  (class_definition)
] @declaration

[
  (function_definition name: (identifier) @definition)
  (class_definition name: (identifier) @definition)
]

[(import_statement) (import_from_statement)] @import
(parameters) @signature
(block) @scope
(call
  function: [
    (identifier)
    (attribute
      attribute: (identifier))
  ] @call)
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
