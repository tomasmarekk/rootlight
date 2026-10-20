; Native owner nodes provide definitions; terminators, aspects and uses do not.
(compilation) @root @module
[(package_declaration) (package_body) (subprogram_body)
 (subprogram_declaration) (expression_function_declaration)] @declaration @definition @signature
[(package_declaration) (package_body) (subprogram_body)
 (subprogram_declaration) (expression_function_declaration)] @scope
(parameter_specification (identifier) @declaration @definition)
(procedure_call_statement name: (identifier) @reference)
[(with_clause) (use_clause) (full_type_declaration) (subtype_declaration)
 (object_declaration) (function_call) (selected_component) (generic_instantiation)
 (task_body) (protected_body) (ERROR)] @expression
(comment) @comment
(string_literal) @string
