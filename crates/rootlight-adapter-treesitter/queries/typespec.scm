; Written TypeSpec owners use their own native grammar and exact name fields.
; Context markers keep unsupported declarations and evaluation semantics explicit.
(source_file) @root @module
(source_file) @expression
[(namespace_statement) (model_statement) (interface_statement)
 (operation_statement) (interface_member) (model_property)] @declaration
[(namespace_statement name: (_) @definition)
 (model_statement name: (_) @definition)
 (interface_statement name: (_) @definition)
 (operation_statement name: (_) @definition)
 (interface_member name: (_) @definition)
 (model_property name: (_) @definition)]
[(import_statement) (using_statement) (annotation_list)
 (union_statement) (enum_statement) (scalar_statement) (alias_statement)
 (const_statement) (decorator_declaration_statement) (function_declaration_statement)
 (model_statement) (interface_statement) (operation_statement)
 (interface_member) (model_property) (namespace_statement)] @expression
[(single_line_comment) (multi_line_comment)] @comment
