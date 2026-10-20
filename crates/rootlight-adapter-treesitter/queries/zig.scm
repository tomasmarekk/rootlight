; Only native binder declarations are candidates; uses and assignments need other proof.
(source_file) @root @module @scope
(function_declaration) @declaration @definition @signature
(function_declaration) @scope
(variable_declaration) @declaration @definition
[(function_declaration) (variable_declaration) (comptime_declaration)
 (using_namespace_declaration) (struct_declaration) (enum_declaration)
 (union_declaration) (opaque_declaration) (ERROR)] @expression
(comment) @comment
[(string) (multiline_string)] @string
