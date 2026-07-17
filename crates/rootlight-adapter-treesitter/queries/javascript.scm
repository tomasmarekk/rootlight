; Reviewed Tier C/D structural evidence for the pinned JavaScript grammar.
; Captures describe source structure only; no call or name resolution is inferred.

(program) @root @module

[
  (function_declaration)
  (class_declaration)
  (method_definition)
  (variable_declarator)
] @declaration

[
  (function_declaration name: (identifier) @definition)
  (class_declaration name: (identifier) @definition)
  (method_definition name: [(property_identifier) (identifier)] @definition)
  (variable_declarator name: (identifier) @definition)
]

(import_statement) @import
(formal_parameters) @signature
(statement_block) @scope
[(identifier) (property_identifier)] @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string) (template_string)] @string
