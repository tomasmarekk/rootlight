; Reviewed Tier C/D structural evidence for the pinned Java grammar.
; Captures remain generic Rootlight roles and expose no native query internals.

(program) @root
[(package_declaration) (module_declaration)] @module

(package_declaration
  [(identifier) (scoped_identifier)] @definition
  .)

(module_declaration
  name: [(identifier) (scoped_identifier)] @definition)

[
  (class_declaration)
  (interface_declaration)
  (annotation_type_declaration)
  (annotation_type_element_declaration)
  (enum_declaration)
  (record_declaration)
  (method_declaration)
  (constructor_declaration)
  (field_declaration)
  (local_variable_declaration)
] @declaration

[
  (class_declaration name: (identifier) @definition)
  (interface_declaration name: (identifier) @definition)
  (annotation_type_declaration name: (identifier) @definition)
  (annotation_type_element_declaration name: (identifier) @definition)
  (enum_declaration name: (identifier) @definition)
  (record_declaration name: (identifier) @definition)
  (method_declaration name: (identifier) @definition)
  (constructor_declaration name: (identifier) @definition)
  (variable_declarator name: (identifier) @definition)
]

; Named callable formals exclude record components and lambda binders.
(method_declaration
  parameters: (formal_parameters
    (formal_parameter name: (identifier) @definition) @declaration))

(constructor_declaration
  parameters: (formal_parameters
    (formal_parameter name: (identifier) @definition) @declaration))

(import_declaration) @import
(formal_parameters) @signature

(annotation_type_element_declaration
  "(" @signature
  ")")

(block) @scope
(method_invocation) @call
(method_invocation
  name: (identifier) @call_name)
((marker_annotation
  name: (identifier) @test_attribute)
  (#eq? @test_attribute "Test"))
(identifier) @reference
[(line_comment) (block_comment)] @comment

((block_comment) @documentation
  (#match? @documentation "^/\\*\\*"))

(string_literal) @string

; The required context records the limits of native setter-formal binding.
(method_declaration) @signature
