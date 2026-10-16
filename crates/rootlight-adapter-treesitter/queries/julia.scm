; Native ownership distinguishes method heads, bindings and literal contents.
; Lowering retains written declarations without inferring evaluated Julia state.
(source_file) @root @module
(for_binding) @declaration @definition @signature
[(local_statement (_) @declaration @definition @signature)
 (global_statement (_) @declaration @definition @signature)
 (let_statement (_) @declaration @definition @signature)]
[(module_definition) (abstract_definition) (primitive_definition)
 (struct_definition) (function_definition) (macro_definition) (assignment)
 (arrow_function_expression) (do_clause)] @declaration @definition @signature
(argument_list (_) @declaration @definition @signature)
[(arrow_function_expression (_) @declaration @definition @signature)
 (do_clause (_) @declaration @definition @signature)
 (signature (_) @declaration @definition @signature)]
(parenthesized_expression) @declaration @definition @signature
[(where_expression (_) @declaration @definition @signature)
 (curly_expression (_) @declaration @definition @signature)]
(where_expression) @signature
[(tuple_expression (_) @declaration @definition @signature)
 (open_tuple (_) @declaration @definition @signature)]
(call_expression (parenthesized_expression [(typed_expression) (unary_typed_expression)] @declaration @definition @signature))
(struct_definition (block (_) @declaration @definition @signature))
(call_expression) @call
(identifier) @reference
(prefixed_string_literal) @reference
[(import_statement) (using_statement)] @import
[(quote_statement) (quote_expression) (macrocall_expression)
 (for_statement) (let_statement) (global_statement) (compound_assignment_expression)] @signature
[(line_comment) (block_comment)] @comment
[(string_literal) (command_literal) (prefixed_string_literal) (prefixed_command_literal)] @string
