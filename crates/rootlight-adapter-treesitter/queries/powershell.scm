; PowerShell written declarations and call-site evidence, without script execution.
; Runtime module loading and dynamic command dispatch are not resolved imports.

(program) @root @module @scope
(function_statement (function_name) @definition) @declaration @scope @signature
(class_statement . (simple_name) @definition) @declaration @scope @signature
(class_method_definition (simple_name) @definition) @declaration @scope @signature
(class_property_definition (variable) @definition) @declaration
(enum_statement . (simple_name) @definition) @declaration @scope @signature
(enum_member (simple_name) @definition) @declaration
[(script_parameter) (class_method_parameter)] @declaration
(script_parameter (variable) @definition)
(class_method_parameter (variable) @definition)
(left_assignment_expression) @declaration @definition
(foreach_statement (variable) @declaration @definition)
(script_block_expression) @scope
[(hash_literal_expression) (data_statement)] @scope
[(command) (invokation_expression)] @call
(command command_name: (command_name) @call_name @reference)
(invokation_expression (member_name (simple_name) @call_name))
[(variable) (type_name)] @reference
(class_statement (simple_name) @reference)
(member_name (simple_name) @reference)
(comment) @comment @documentation
(string_literal) @string
