; Authored HDL declarations retain ownership before preprocessing and elaboration.
(source_file) @root @module
[(module_declaration) (interface_declaration) (program_declaration)
 (package_declaration) (class_declaration) (interface_class_declaration)
 (function_declaration) (task_declaration) (udp_declaration)
 (checker_declaration) (property_declaration) (sequence_declaration)
 (clocking_declaration) (generate_block) (modport_item)
 (function_prototype) (task_prototype) (let_declaration)
 (text_macro_definition)] @declaration @definition @signature
[(simple_identifier) (escaped_identifier)] @declaration @definition @signature
; Optional reads must remain separate from the mandatory identity query partition.
[(simple_identifier) (escaped_identifier)] @reference
[(tf_call) (method_call_body) (system_tf_identifier)] @call
[(one_line_comment) (block_comment)] @comment
(string_literal) @string
[(text_macro_name) (macro_text) (text_macro_usage) (conditional_compilation_directive)
 (loop_generate_construct) (conditional_generate_construct)
 (include_compiler_directive) (include_statement)
 (package_import_declaration) (package_export_declaration)
 (config_declaration) (library_declaration) (bind_directive)
 (covergroup_declaration) (constraint_declaration) (extern_constraint_declaration)
 (class_constructor_declaration) (nettype_declaration)] @signature
