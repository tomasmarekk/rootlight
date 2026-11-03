(source_file) @root @module @signature
(interface_definition) @scope @declaration @signature
(interface_definition name: (identifier) @definition)
(class_definition) @scope @declaration @signature
(class_definition name: (identifier) @definition)
(methods_declaration) @signature
(method_entry) @declaration @signature
(method_entry name: (identifier) @definition)
(statement) @signature
(data_declaration) @signature
(inline_data_statement) @signature
(section_declaration) @signature
(class_implementation) @scope @signature
(class_implementation name: (identifier) @expression)
(method_implementation) @scope @signature
(method_implementation name: (identifier) @reference)
(class_forward_declaration) @signature
(interface_forward_declaration) @signature
(form_definition) @signature
(function_definition) @signature
(module_definition) @signature
(program_statement) @signature
(line_comment) @comment
(star_comment) @comment
