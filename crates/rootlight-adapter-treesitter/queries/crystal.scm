; Only direct native declaration names and admitted lexical owners become identities.
; Independent expression captures retain unsupported declaration and evaluation gaps.
(expressions) @root @module @scope
[(module_def) (class_def) (struct_def) (enum_def)
 (method_def) (abstract_method_def)] @declaration @definition @signature
[(module_def) (class_def) (struct_def) (enum_def)
 (method_def) (abstract_method_def)] @scope
[(module_def) (class_def) (struct_def) (enum_def)
 (method_def) (abstract_method_def) (macro_def) (macro_expression)
 (macro_statement) (lib_def) (type_def) (alias) (ERROR)] @expression
(comment) @comment
(string) @string
