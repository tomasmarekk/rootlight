; VBA written declarations retain exact names, headers and owning source scopes.
; Parenthesized applications are not proven calls without binding evidence.
(source_file) @root @module @scope
(source_file) @definition
(source_file) @reference
(source_file) @signature
(implements_statement name: (_) @reference)
(preprocessor_if) @scope
(preprocessor_block) @scope
(type_preprocessor_if) @scope
(type_preprocessor_block) @scope
; Complete branch headers are identity evidence, not optional runtime references.
(preprocessor_if condition: (_) @signature)
(preprocessor_elseif condition: (_) @signature)
(preprocessor_else) @signature
(type_preprocessor_if condition: (_) @signature)
(type_preprocessor_elseif condition: (_) @signature)
(type_preprocessor_else) @signature
(sub_declaration name: (_) @definition) @declaration @scope @signature
(function_declaration name: (_) @definition) @declaration @scope @signature
(conditional_sub_declaration name: (_) @definition) @declaration @scope @signature
(conditional_function_declaration name: (_) @definition) @declaration @scope @signature
(conditional_property_declaration name: (_) @definition) @declaration @scope @signature
(property_get_declaration name: (_) @definition) @declaration @scope @signature
(property_let_declaration name: (_) @definition) @declaration @scope @signature
(property_set_declaration name: (_) @definition) @declaration @scope @signature
(declare_sub_statement name: (_) @definition) @declaration @scope @signature
(declare_function_statement name: (_) @definition) @declaration @scope @signature
(event_declaration name: (_) @definition) @declaration @scope @signature
(type_declaration name: (_) @definition) @declaration @scope @signature
(enum_declaration name: (_) @definition) @declaration @scope @signature
(type_member name: (_) @definition) @declaration
(enum_member name: (_) @definition) @declaration
(parameter name: (_) @definition) @declaration
(variable_declarator name: (_) @definition) @declaration
(const_declarator name: (_) @definition) @declaration
(preprocessor_const name: (_) @definition) @declaration
(label_statement name: (_) @definition) @declaration
(line_number_prefix (line_number_literal) @definition @declaration)
(line_number_statement number: (line_number_literal) @definition @declaration)
(identifier) @reference
(line_number_literal) @reference
(bang_identifier) @reference
(qualified_member_expression) @reference
(implicit_member_expression) @reference
(call_expression) @reference
(new_expression) @reference
(call_statement) @reference
(comment) @comment
(string_literal) @string

; Access metadata is optional; missing evidence must never imply public visibility.
[
  (sub_declaration)
  (function_declaration)
  (conditional_sub_declaration)
  (conditional_function_declaration)
  (conditional_property_declaration)
  (property_get_declaration)
  (property_let_declaration)
  (property_set_declaration)
  (declare_sub_statement)
  (declare_function_statement)
  (event_declaration)
  (type_declaration)
  (enum_declaration)
  (variable_declarator)
  (const_declarator)
] @expression
