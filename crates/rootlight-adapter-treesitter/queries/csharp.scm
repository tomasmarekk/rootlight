; Reviewed Tier D structural evidence for the pinned C# grammar.
; Partial types, delegates, async calls, and LINQ stay syntactic at this tier.

(compilation_unit) @root
[(namespace_declaration) (file_scoped_namespace_declaration)] @module

[
  (class_declaration)
  (interface_declaration)
  (struct_declaration)
  (record_declaration)
  (enum_declaration)
  (delegate_declaration)
  (method_declaration)
  (constructor_declaration)
  (property_declaration)
  (field_declaration)
] @declaration

[
  (class_declaration name: (identifier) @definition)
  (interface_declaration name: (identifier) @definition)
  (struct_declaration name: (identifier) @definition)
  (record_declaration name: (identifier) @definition)
  (enum_declaration name: (identifier) @definition)
  (delegate_declaration name: (identifier) @definition)
  (method_declaration name: (identifier) @definition)
  (constructor_declaration name: (identifier) @definition)
  (property_declaration name: (identifier) @definition)
]

(using_directive) @import
[(parameter_list) (type_parameter_list)] @signature
(block) @scope
(invocation_expression) @call
(identifier) @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^///|^/\\*\\*"))

[
  (string_literal)
  (verbatim_string_literal)
  (raw_string_literal)
  (interpolated_string_expression)
] @string
