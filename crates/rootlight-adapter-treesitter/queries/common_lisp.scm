; Captures prove written owners, never package, reader or evaluated bindings.
; Outer list_lit owns a specialized inner defun exactly once.
(source) @root @module @scope
(list_lit) @declaration @definition @signature
; A separate step keeps scope within the runtime's three-capture step limit.
(list_lit) @scope
(list_lit) @expression
[(sym_lit) (fancy_literal) (package_lit)] @declaration @definition
[(sym_lit) (fancy_literal) (package_lit)] @reference
[(include_reader_macro) (read_cond_lit) (splicing_read_cond_lit)
 (dis_expr) (meta_lit) (ERROR)] @expression
[(comment) (block_comment)] @comment
(str_lit) @string
