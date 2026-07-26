; Reviewed Tier D structural evidence for the pinned Kotlin grammar.
; Overloads, extension dispatch, frameworks, and generated origins stay unresolved.

(source_file) @root
(package_header) @module

[
  (class_declaration)
  (object_declaration)
  (function_declaration)
  (property_declaration)
] @declaration

[
  (class_declaration name: (identifier) @definition)
  (object_declaration (identifier) @definition)
  (function_declaration name: (identifier) @definition)
  (property_declaration (variable_declaration (identifier) @definition))
]

(import) @import
[(function_value_parameters) (type_parameters) (class_parameters)] @signature
[(block) (class_body)] @scope
(call_expression) @call
[(identifier) (qualified_identifier)] @reference
[(line_comment) (block_comment)] @comment

((block_comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string_literal) (multiline_string_literal)] @string
