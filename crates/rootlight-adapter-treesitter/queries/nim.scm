; Native fields prove complete written owners; compiler expansion stays a gap.
(source_file) @root @module @scope
[(proc_declaration) (func_declaration) (type_declaration)
 (symbol_declaration)] @declaration @definition @signature
(call) @reference
[(macro_declaration) (template_declaration) (method_declaration)
 (iterator_declaration) (converter_declaration) (conditional_declaration)
 (when) (proc_expression) (func_expression) (iterator_expression)
 (tuple_deconstruct_declaration) (include_statement) (mixin_statement)
 (bind_statement) (pragma_statement) (block) (do_block) (ERROR)] @expression
[(comment) (documentation_comment) (block_comment)
 (block_documentation_comment)] @comment
[(interpreted_string_literal) (raw_string_literal) (long_string_literal)] @string
