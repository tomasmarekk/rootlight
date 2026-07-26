; Reviewed Tier D structural evidence for the pinned C++ grammar.
; Templates, overloads, and preprocessor branches remain unresolved syntax.

(translation_unit) @root
(namespace_definition) @module

[
  (function_definition)
  (declaration)
  (class_specifier)
  (struct_specifier)
  (union_specifier)
  (enum_specifier)
  (type_definition)
  (template_declaration)
] @declaration

[
  (function_declarator declarator: [(identifier) (field_identifier)] @definition)
  (class_specifier name: (type_identifier) @definition)
  (struct_specifier name: (type_identifier) @definition)
  (union_specifier name: (type_identifier) @definition)
  (enum_specifier name: (type_identifier) @definition)
  (type_definition declarator: (type_identifier) @definition)
  (namespace_definition name: (namespace_identifier) @definition)
]

(preproc_include) @import
[(parameter_list) (template_parameter_list)] @signature
[(compound_statement) (declaration_list)] @scope
(call_expression) @call
[
  (identifier)
  (field_identifier)
  (type_identifier)
  (namespace_identifier)
] @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string_literal) (raw_string_literal) (concatenated_string)] @string
