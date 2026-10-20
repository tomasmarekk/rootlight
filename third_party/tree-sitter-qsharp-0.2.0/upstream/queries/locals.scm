; locals.scm — Scope and variable resolution for Q#
; Used by editors for improved highlighting, go-to-definition, and rename.

; Scopes
(block) @local.scope
(callable_decl) @local.scope
(namespace) @local.scope
(for_expression) @local.scope
(while_expression) @local.scope
(repeat_expression) @local.scope
(if_expression) @local.scope
(conjugation_expression) @local.scope

; Definitions
(let_statement
  (pattern (identifier) @local.definition))

(mutable_statement
  (pattern (identifier) @local.definition))

(for_expression
  (pattern (identifier) @local.definition))

(typed_param
  name: (identifier) @local.definition)

(use_statement
  (pattern (identifier) @local.definition))

(borrow_statement
  (pattern (identifier) @local.definition))

; References
(identifier) @local.reference
