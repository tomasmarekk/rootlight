; Written V owners retain physical names before compile-time and type evaluation.
(source_file) @root @module
[(module_clause) (struct_declaration) (interface_declaration) (enum_declaration)
 (type_declaration) (function_declaration) (static_method_declaration)
 (interface_method_definition) (struct_field_declaration) (enum_field_definition)
 (const_definition) (global_var_definition) (parameter_declaration) (receiver)
 (generic_parameter) (var_definition) (identifier) (import_spec)] @declaration @definition @signature
(import_spec) @import
; Optional occurrences must not enter the mandatory declaration query partition.
[(reference_expression) (type_reference_expression)] @reference
(call_expression) @call
[(line_comment) (block_comment)] @comment
[(interpreted_string_literal) (raw_string_literal) (c_string_literal)] @string
[(import_declaration) (compile_time_if_expression) (compile_time_for_statement)
 (embedded_definition) (anon_struct_type) (function_literal)] @signature
