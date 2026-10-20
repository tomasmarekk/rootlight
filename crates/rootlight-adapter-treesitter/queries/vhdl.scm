; VHDL physical declarations use reviewed grammar fields, never highlighting roles.
(design_file) @root @module
[(entity_declaration) (architecture_definition) (package_declaration)
 (component_declaration) (subprogram_declaration) (subprogram_definition)
 (type_declaration) (subtype_declaration)] @declaration @definition @signature
[(identifier) (library_constant) (library_constant_boolean)
 (library_constant_character) (library_constant_debug) (library_constant_env)
 (library_constant_standard) (library_constant_std_logic)
 (library_function) (library_type)] @declaration @definition @signature
[(identifier) (library_constant) (library_constant_boolean)
 (library_constant_character) (library_constant_debug) (library_constant_env)
 (library_constant_standard) (library_constant_std_logic)
 (library_function) (library_type)] @reference
[(line_comment) (block_comment)] @comment
[(string_literal) (string_literal_std_logic)] @string
[(package_definition) (configuration_declaration) (context_declaration)
 (mode_view_declaration) (process_statement) (block_statement)
 (for_generate_statement) (if_generate_statement) (case_generate_statement)] @signature
