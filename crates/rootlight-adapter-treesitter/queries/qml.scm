; QML object ownership and calls are admitted by the native bounded census.
; Imported types, dynamic bindings and runtime dispatch remain explicit gaps.
(program) @root @module @scope
(ui_object_definition) @scope @declaration @definition
(ui_inline_component) @scope @declaration
(ui_property) @declaration @definition
(function_declaration) @scope @declaration @definition
(function_declaration) @signature
[(function_expression) (arrow_function) (generator_function)
 (generator_function_declaration) (method_definition)] @scope @declaration
[(variable_declarator) (class_declaration) (ui_signal)
 (enum_declaration)] @declaration
(call_expression) @call
[(ui_import) (ui_binding) (ui_signal) (enum_declaration)
 (ui_inline_component) (ERROR)] @expression
(comment) @comment
(string) @string
