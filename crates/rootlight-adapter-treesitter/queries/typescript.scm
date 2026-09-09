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
  (type_parameter)
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
  (type_parameter name: (type_identifier) @definition)
]

(import_statement) @import
(import_statement source: (string) @signature)
(import_clause (identifier) @signature)
(namespace_import (identifier) @signature)
(import_specifier) @signature
(import_specifier name: [(identifier) (string)] @signature)
(export_statement declaration: (_) @signature)
(export_statement "default" value: (_) @signature)
(export_statement source: (string) @signature) @signature
(namespace_export [(identifier) (string)] @signature)
; Optional references need a separate pattern from the identity-only signature pass.
(namespace_export [(identifier) (string)] @reference)
(export_specifier name: (string) @reference)
(export_specifier alias: (string) @reference)
(export_specifier) @signature
(export_specifier name: [(identifier) (string)] @signature)
(export_specifier alias: [(identifier) (string)] @signature)
(import_alias) @import
(import_require_clause (identifier) @declaration @definition)
(import_alias . (identifier) @declaration @definition)
(import_clause (identifier) @declaration @definition)
(namespace_import (identifier) @declaration @definition)
(import_specifier name: (identifier) @declaration @definition)
(import_specifier alias: (identifier) @declaration @definition)
(formal_parameters) @signature
(variable_declaration (variable_declarator) @signature)
(for_in_statement kind: "var") @signature
(statement_block) @scope
[(class_declaration type_parameters: (type_parameters))
 (abstract_class_declaration type_parameters: (type_parameters))
 (interface_declaration type_parameters: (type_parameters))
 (type_alias_declaration type_parameters: (type_parameters))] @scope
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
[(for_statement) (for_in_statement) (catch_clause) (switch_body) (class_static_block)] @scope

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
[(member_expression) (nested_identifier) (nested_type_identifier)] @reference
[(shorthand_property_identifier) (shorthand_property_identifier_pattern)] @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string) (template_string)] @string
