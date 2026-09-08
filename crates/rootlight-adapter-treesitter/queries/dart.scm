; Dart bindings come from reviewed declaration positions, never editor color categories.
; Callable wrappers retain bodies for source reads while signatures exclude them.
(source_file) @root @module
(class_declaration name: (identifier) @definition) @declaration @scope @signature
(mixin_application_class . (identifier) @definition) @declaration @scope @signature
(mixin_declaration name: (identifier) @definition) @declaration @scope @signature
(extension_declaration name: (identifier) @definition) @declaration @scope @signature
(extension_declaration !name) @scope @signature
(extension_type_declaration name: (extension_type_name . (identifier) @definition)) @declaration @scope @signature
(extension_type_declaration name: (identifier) @definition) @declaration @scope @signature
(extension_type_representation name: (identifier) @definition) @declaration
(enum_declaration name: (identifier) @definition) @declaration @scope @signature
(enum_constant name: (identifier) @definition) @declaration
(type_alias (type_identifier) @definition) @declaration
(type_parameter name: (type_identifier) @definition) @declaration
[(function_declaration signature: (_) @definition)
 (getter_declaration signature: (_) @definition)
 (setter_declaration signature: (_) @definition)
 (external_function_declaration signature: (_) @definition)
 (external_getter_declaration signature: (_) @definition)
 (external_setter_declaration signature: (_) @definition)
 (local_function_declaration (function_signature) @definition)
 (method_declaration signature: (_) @definition)] @declaration @scope @signature
(declaration [(function_signature) (getter_signature) (setter_signature)
 (operator_signature) (constructor_signature) (constant_constructor_signature)
 (factory_constructor_signature) (redirecting_factory_constructor_signature)] @definition) @declaration @scope @signature
(identifier) @declaration @definition
(function_expression) @scope
(block) @scope
(for_statement) @scope
(switch_statement_case) @scope
(switch_expression_case) @scope
(catch_clause) @scope
[(identifier) (type_identifier)] @reference
(import_or_export) @import
(call_expression) @call
(call_expression function: [(identifier) (member_expression) (null_aware_member_expression)] @call_name)
(comment) @comment
((comment) @documentation (#match? @documentation "^(/\\*\\*|///)"))
(string_literal) @string
