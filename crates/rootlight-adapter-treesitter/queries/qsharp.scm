(source_file) @root @module @scope
(ERROR) @root @module @scope
[(namespace) (callable_decl)] @scope @declaration @definition
[(namespace) (callable_decl)] @signature
(typed_param) @declaration @definition @signature
(callable_body) @signature
(identifier) @reference
(identifier) @signature
[(open_decl) (import_decl) (export_decl) (newtype_decl) (struct_decl)
 (let_statement) (mutable_statement) (set_statement) (use_statement)
 (borrow_statement) (return_statement) (fail_statement)] @expression
[(comment) (doc_comment)] @comment
(string_literal) @string
