; Reviewed Tier C/D structural evidence for the pinned JavaScript grammar.
; Captures describe source structure only; no name resolution is inferred.

(program) @root @module

[
  (function_declaration)
  (class_declaration)
  (method_definition)
  (variable_declarator)
] @declaration

[
  (function_declaration name: (identifier) @definition)
  (class_declaration name: (type_identifier) @definition)
  (method_definition name: [(property_identifier) (identifier)] @definition)
  (variable_declarator name: (identifier) @definition)
]

(import_statement) @import
(formal_parameters) @signature
(statement_block) @scope
(generator_function_declaration name: (identifier) @definition) @declaration
(function_expression name: (identifier) @definition) @declaration
(generator_function name: (identifier) @definition) @declaration
; Pattern edges identify bindings without treating default values or computed keys as declarations.
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
[(identifier) (property_identifier)] @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string) (template_string)] @string
