; indents.scm — Auto-indentation rules for Q#

; Indent after opening brace
[
  (block)
  (namespace)
  (specialization_list)
  (struct_decl)
  (array_expression)
  (new_expression)
] @indent

; Dedent on closing brace
"}" @outdent

; Dedent on closing bracket/paren
"]" @outdent
")" @outdent
