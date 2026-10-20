; Written identities retain only clean native declaration and owner shapes.
(module) @root @module @scope
(ERROR) @root @module @scope
[(class_definition) (function_definition)] @scope @declaration @definition
[(class_definition) (function_definition)] @signature
(assignment) @declaration @definition @signature
(parameters [(identifier) (self_parameter) (typed_parameter)
 (default_parameter) (typed_default_parameter)] @declaration @definition @signature)
(identifier) @reference
(identifier) @signature
(attribute) @signature
[(decorator) (import_statement) (import_from_statement)
 (global_statement) (nonlocal_statement) (ERROR)] @expression
(comment) @comment
(string) @string
