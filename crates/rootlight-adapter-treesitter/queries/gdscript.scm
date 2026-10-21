; Only a unique clean file class_name certifies direct method ownership.
; Ordinary names stay optional and never acquire targets by spelling.
(source) @root @module @scope
(ERROR) @root @module @scope
[(class_name_statement) (constructor_definition) (function_definition)] @scope @declaration @definition
[(class_name_statement) (constructor_definition) (function_definition)] @signature
(identifier) @reference
[(extends_statement) (variable_statement) (const_statement) (signal_statement)
 (enum_definition) (class_definition) (annotation) (lambda) (ERROR)] @expression
(comment) @comment
(string) @string
