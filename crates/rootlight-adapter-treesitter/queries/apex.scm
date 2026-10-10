; Apex declarations use direct native owners and original written source.
; Physical names and shared field headers do not prove Salesforce bindings.
(parser_output) @root @module @scope
[(class_declaration) (interface_declaration) (enum_declaration)
 (constructor_declaration) (method_declaration) (formal_parameter)
 (enum_constant) (variable_declarator)] @declaration @definition @signature
[(block) (constructor_body)] @scope
[(class_declaration) (interface_declaration) (enum_declaration)
 (constructor_declaration) (method_declaration) (formal_parameter)
 (field_declaration) (block) (constructor_body)
 (method_invocation) (field_access) (annotation)] @expression
(identifier) @reference
[(line_comment) (block_comment)] @comment
((block_comment) @documentation
  (#match? @documentation "^/\\*\\*"))
(string_literal) @string
