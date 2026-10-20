; Ring language - Text objects for Tree-sitter
; Compatible with: Neovim (nvim-treesitter-textobjects)

; ====================
; Functions
; ====================

; Function definitions (af = around function, if = inside function)
(function_definition) @function.outer

(function_definition
  name: (identifier)
  (_)* @function.inner)

; Anonymous functions
(anonymous_function) @function.outer

; ====================
; Classes
; ====================

; Class definitions
(class_definition) @class.outer

(class_definition
  name: (identifier)
  (_)* @class.inner)

; ====================
; Parameters/Arguments
; ====================

; Function parameters
(param_list) @parameter.outer

(param_list
  (identifier) @parameter.inner)

; Call arguments
(arguments) @parameter.outer

(arguments
  (_) @parameter.inner)

; ====================
; Conditionals
; ====================

(if_statement) @conditional.outer

(if_statement
  condition: (_) @conditional.inner)

(elseif_clause) @conditional.outer
(else_clause) @conditional.outer

; ====================
; Loops
; ====================

(while_statement) @loop.outer

(while_statement
  (_)* @loop.inner)

(for_statement) @loop.outer

(for_statement
  (_)* @loop.inner)

(do_again_statement) @loop.outer

; ====================
; Comments
; ====================

(comment) @comment.outer

; ====================
; Blocks
; ====================

; Brace blocks
(brace_expression) @block.outer

; ====================
; Calls
; ====================

(call_expression) @call.outer

(call_expression
  (arguments) @call.inner)

; ====================
; Statements
; ====================

[
  (expression_statement)
  (return_statement)
  (see_statement)
  (give_statement)
  (load_statement)
  (import_statement)
  (exit_statement)
  (loop_statement)
  (bye_statement)
] @statement.outer
