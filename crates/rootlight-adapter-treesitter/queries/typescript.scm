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
(import_alias) @import
(import_require_clause (identifier) @declaration @definition)
(import_alias . (identifier) @declaration @definition)
(import_clause (identifier) @declaration @definition)
(namespace_import (identifier) @declaration @definition)
(import_specifier name: (identifier) @declaration @definition)
(import_specifier alias: (identifier) @declaration @definition)
(formal_parameters) @signature
(statement_block) @scope
(generator_function_declaration name: (identifier) @definition) @declaration
(function_expression name: (identifier) @definition) @declaration
(generator_function name: (identifier) @definition) @declaration
; Binding ancestry distinguishes declarations from assignments and excludes keys/default values.
(required_parameter pattern: (identifier) @declaration @definition)
(required_parameter name: (identifier) @declaration @definition)
(optional_parameter pattern: (identifier) @declaration @definition)
(optional_parameter name: (identifier) @declaration @definition)
(arrow_function parameter: (identifier) @declaration @definition)
(array_pattern (identifier) @declaration @definition)
(object_pattern (shorthand_property_identifier_pattern) @declaration @definition)
(pair_pattern value: (identifier) @declaration @definition)
(assignment_pattern left: (identifier) @declaration @definition)
(object_assignment_pattern left: (shorthand_property_identifier_pattern) @declaration @definition)
(rest_pattern (identifier) @declaration @definition)
(catch_clause parameter: (identifier) @declaration @definition)
(for_in_statement left: (identifier) @declaration @definition)
[(for_statement) (for_in_statement) (catch_clause)] @scope

[(function_declaration) (function_expression) (generator_function_declaration)
 (generator_function) (arrow_function) (method_definition) (function_signature)
 (method_signature) (abstract_method_signature) (function_type) (constructor_type)
 (call_signature) (construct_signature)] @scope

(call_expression
  function: [
    (identifier)
    (member_expression
      property: (property_identifier))
  ] @call)

[(identifier) (type_identifier) (property_identifier)] @reference
[(shorthand_property_identifier) (shorthand_property_identifier_pattern)] @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string) (template_string)] @string
