; Ring ownership needs the closed native class/method census, not CST nesting.
; Runtime remapping, DSL and general expression semantics remain explicit gaps.
(source_file) @root @module @scope
(ERROR) @root @module @scope
[(class_definition) (function_definition)] @scope @declaration @definition
[(class_definition) (function_definition)] @signature
(param_list (identifier) @declaration @definition)
(identifier) @reference @signature
; Optional unresolved Reads must not disappear with the identity-only pattern.
(identifier) @reference
[(class_definition) (package_definition) (scanner_command)
 (anonymous_function) (interpolation) (load_statement) (import_statement)
 (ERROR)] @expression
(comment) @comment
(string) @string
