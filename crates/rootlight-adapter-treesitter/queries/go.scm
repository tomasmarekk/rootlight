; Reviewed Tier C/D structural evidence for the pinned Go grammar.
; Captures describe bounded source structure and never imply resolved semantics.

(source_file) @root

(package_clause
  (package_identifier) @definition) @module

[
  (function_declaration)
  (method_declaration)
  (type_spec)
  (var_spec)
  (const_spec)
] @declaration

[
  (function_declaration name: (identifier) @definition)
  (method_declaration name: (field_identifier) @definition)
  (type_spec name: (type_identifier) @definition)
  (var_spec name: (identifier) @definition)
  (const_spec name: (identifier) @definition)
]

(import_declaration) @import

[
  (function_declaration parameters: (parameter_list) @signature)
  (method_declaration parameters: (parameter_list) @signature)
]

(block) @scope

(call_expression
  function: [
    (identifier)
    (selector_expression
      field: (field_identifier))
  ] @call)

[(identifier) (field_identifier) (type_identifier) (package_identifier)] @reference
(comment) @comment

(
  (comment) @documentation
  .
  [
    (package_clause)
    (function_declaration)
    (method_declaration)
    (type_declaration)
    (var_declaration)
    (const_declaration)
  ]
)

[(interpreted_string_literal) (raw_string_literal)] @string
