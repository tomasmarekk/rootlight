; Reviewed Tier C/D structural evidence for the pinned TypeScript grammar.
; Captures remain syntax-only and do not promote structural names to deep facts.

(program) @root @module

[
  (function_declaration)
  (function_signature)
  (class_declaration)
  (abstract_class_declaration)
  (interface_declaration)
  (type_alias_declaration)
  (enum_declaration)
  (method_definition)
  (method_signature)
  (abstract_method_signature)
  (variable_declarator)
] @declaration

[
  (function_declaration name: (identifier) @definition)
  (function_signature name: (identifier) @definition)
  (class_declaration name: (type_identifier) @definition)
  (abstract_class_declaration name: (type_identifier) @definition)
  (interface_declaration name: (type_identifier) @definition)
  (type_alias_declaration name: (type_identifier) @definition)
  (enum_declaration name: (identifier) @definition)
  (method_definition name: [(property_identifier) (identifier)] @definition)
  (method_signature name: [(property_identifier) (identifier)] @definition)
  (abstract_method_signature name: [(property_identifier) (identifier)] @definition)
  (variable_declarator name: (identifier) @definition)
]

(import_statement) @import
(formal_parameters) @signature
(statement_block) @scope

(call_expression
  function: [
    (identifier)
    (member_expression
      property: (property_identifier))
  ] @call)

[(identifier) (type_identifier) (property_identifier)] @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string) (template_string)] @string
