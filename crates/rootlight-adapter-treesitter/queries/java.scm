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

(import_declaration) @import
(formal_parameters) @signature

(annotation_type_element_declaration
  "(" @signature
  ")")

(block) @scope
(method_invocation
  name: (identifier) @call)
(identifier) @reference
[(line_comment) (block_comment)] @comment

((block_comment) @documentation
  (#match? @documentation "^/\\*\\*"))

(string_literal) @string
