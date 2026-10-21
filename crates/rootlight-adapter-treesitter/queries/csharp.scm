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
] @declaration

; Comma-separated field names keep separate declaration extents.
(field_declaration
  (variable_declaration
    (variable_declarator) @declaration))

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
  (field_declaration
    (variable_declaration
      (variable_declarator name: (identifier) @definition)))
]

(parameter) @declaration
(parameter name: (identifier) @definition)

; Full-method proof metadata is required but never a symbol signature.
(method_declaration) @signature
; Keep optional references separate from their required certificate carrier.
(identifier) @signature

(using_directive) @import
[(parameter_list) (type_parameter_list)] @signature
(block) @scope
(invocation_expression) @call
[
  (invocation_expression
    function: (identifier) @call_name)
  (invocation_expression
    function: (generic_name
      (identifier) @call_name))
  (invocation_expression
    function: (member_access_expression
      name: (identifier) @call_name))
  (invocation_expression
    function: (member_access_expression
      name: (generic_name
        (identifier) @call_name)))
]
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
