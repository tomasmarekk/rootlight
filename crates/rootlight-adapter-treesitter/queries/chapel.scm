; Written native headers, certified formal reads and finite typed-local calls.
(source_file) @root @module @scope
(ERROR) @root @module @scope
(module_declaration) @scope @declaration @definition
(module_declaration) @signature
(procedure_declaration) @scope @declaration @definition
(procedure_declaration) @signature
(formal_parameter) @declaration @definition @signature
(identifier) @reference
(identifier) @signature
(procedure_declaration body: (block_statement) @signature)
(call_expression) @signature
[(unsupported_keyword_statement) (for_statement) (assignment_statement)] @expression
(unsupported_keyword_statement) @signature
[(block_comment) (inline_comment)] @comment
(string_literal) @string
