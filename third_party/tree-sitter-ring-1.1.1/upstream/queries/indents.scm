; Ring language - Indentation rules

; Indent after these nodes
[
  (if_statement)
  (while_statement)
  (for_statement)
  (switch_statement)
  (try_statement)
  (do_again_statement)
  (function_definition)
  (class_definition)
  (package_definition)
  (elseif_clause)
  (else_clause)
  (case_clause)
  (other_clause)
  (brace_expression)
  (brace_block)
  (list_literal)
] @indent

; Closing brackets outdent
[
  "}"
  "]"
  ")"
] @outdent
