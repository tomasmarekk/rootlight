; VBA written declarations retain exact names, headers and owning source scopes.
; Parenthesized applications are not proven calls without binding evidence.
(source_file) @root @module @scope
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
(identifier) @reference
(bang_identifier) @reference
(qualified_member_expression) @reference
(implicit_member_expression) @reference
(call_expression) @reference
(comment) @comment
(string_literal) @string
