; textobjects.scm — Semantic text objects for Q#
; Used by Helix and nvim-treesitter-textobjects for selecting/navigating code.

; Functions / operations
(callable_decl
  body: (callable_body) @function.inside) @function.around

; Types (struct / newtype)
(struct_decl) @class.around
(newtype_decl) @class.around

; Parameters
(typed_param) @parameter.inside

; Comments
(comment) @comment.inside
(comment)+ @comment.around
(doc_comment) @comment.inside
(doc_comment)+ @comment.around

; Conditionals
(if_expression
  body: (block) @conditional.inside) @conditional.around

; Loops
(for_expression
  (block) @loop.inside) @loop.around
(while_expression
  (block) @loop.inside) @loop.around
(repeat_expression) @loop.around
