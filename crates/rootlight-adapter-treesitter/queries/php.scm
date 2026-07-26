; Reviewed Tier D structural evidence for the pinned mixed-mode PHP grammar.
; Dynamic calls, framework routes, and include targets remain unresolved syntax.

(program) @root
(namespace_definition) @module

[
  (class_declaration)
  (interface_declaration)
  (trait_declaration)
  (enum_declaration)
  (function_definition)
  (method_declaration)
  (property_declaration)
  (const_declaration)
] @declaration

[
  (class_declaration name: (name) @definition)
  (interface_declaration name: (name) @definition)
  (trait_declaration name: (name) @definition)
  (enum_declaration name: (name) @definition)
  (function_definition name: (name) @definition)
  (method_declaration name: (name) @definition)
]

[
  (namespace_use_declaration)
  (include_expression)
  (include_once_expression)
  (require_expression)
  (require_once_expression)
] @import

(formal_parameters) @signature
[(compound_statement) (declaration_list)] @scope
[
  (function_call_expression)
  (scoped_call_expression)
  (member_call_expression)
  (nullsafe_member_call_expression)
] @call
[(name) (qualified_name) (variable_name)] @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string) (encapsed_string) (nowdoc_string)] @string
