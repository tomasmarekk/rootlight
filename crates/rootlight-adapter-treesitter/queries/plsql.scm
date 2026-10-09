; Direct PL/SQL owners and name fields describe written source, not a catalog.
; Identity patterns have at most three captures; optional uncertainty is separate.
(source_file) @root @module @scope
[(create_package) (create_package_body) (create_procedure) (create_function)
 (procedure_definition) (procedure_declaration) (function_definition) (function_declaration)
 (type_definition_record) (create_trigger)] @declaration @definition @signature
[(create_package) (create_package_body) (create_procedure) (create_function)
 (procedure_definition) (procedure_declaration) (function_definition) (function_declaration)
 (type_definition_record) (create_trigger) (plsql_block)] @scope
[(parameter_declaration_element) (item_declaration) (field_definition)] @declaration @definition
[(create_package) (create_package_body) (create_procedure) (create_function)
 (procedure_definition) (procedure_declaration) (function_definition) (function_declaration)
 (type_definition_record) (create_trigger) (parameter_declaration_element)
 (item_declaration) (field_definition) (create_type) (create_type_body)] @expression
(ref_call) @reference
(ref_call) @expression
[(conditional_compilation_statement) (conditional_compilation_declaration)
 (conditional_compilation_declaration_section)] @expression
(identifier) @reference
[(comment_sl) (comment_ml)] @comment
(literal_string) @string
