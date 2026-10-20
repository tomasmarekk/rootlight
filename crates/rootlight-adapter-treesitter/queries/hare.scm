; Each declaration must pass the complete native census before admission.
(module) @root @module @scope
(ERROR) @root @module @scope
(function_declaration) @scope @declaration @definition
(function_declaration) @signature
(type_declaration (identifier) @scope @declaration @definition)
(type_declaration (identifier) @signature)
(field) @declaration @definition @signature
(parameter) @declaration @definition @signature
(call_expression callee: (identifier) @reference @signature)
[(type_declaration) (global_declaration) (constant_declaration)
 (use_statement) (call_expression) (ERROR)] @expression
(comment) @comment
(string) @string
