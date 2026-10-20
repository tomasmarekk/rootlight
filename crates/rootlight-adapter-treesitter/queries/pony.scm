; Native direct-child proof separates owner/member names from type references.
; Optional gaps stay separate from mandatory identity extraction patterns.
(source_file) @root @module @scope
[(class_definition) (actor_definition) (struct_definition)
 (primitive_definition) (trait_definition) (interface_definition)
 (constructor) (method) (behavior)] @declaration @definition @signature
[(class_definition) (actor_definition) (struct_definition)
 (primitive_definition) (trait_definition) (interface_definition)
 (constructor) (method) (behavior)] @scope
[(class_definition) (actor_definition) (struct_definition)
 (primitive_definition) (trait_definition) (interface_definition)
 (constructor) (method) (behavior) (type_alias) (field)
 (parameter) (object_literal) (ERROR)] @expression
[(line_comment) (block_comment)] @comment
(string) @string
