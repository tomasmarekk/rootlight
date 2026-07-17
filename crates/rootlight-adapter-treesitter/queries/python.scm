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
[(function_definition) (class_definition)] @signature
(block) @scope
(identifier) @reference
(comment) @comment
(expression_statement (string) @documentation)
(string) @string
