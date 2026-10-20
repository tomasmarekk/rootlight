; Ring language - Code folding for Tree-sitter
; Compatible with: Neovim, Helix, Zed

; ====================
; Definitions (foldable)
; ====================

[
  (function_definition)
  (class_definition)
  (package_definition)
  (anonymous_function)
] @fold

; ====================
; Control Structures (foldable)
; ====================

[
  (if_statement)
  (while_statement)
  (for_statement)
  (switch_statement)
  (try_statement)
  (do_again_statement)
] @fold

; ====================
; Clauses (foldable)
; ====================

[
  (elseif_clause)
  (else_clause)
  (case_clause)
  (other_clause)
] @fold

; ====================
; Collections (foldable)
; ====================

[
  (list_literal)
  (brace_expression)
  (brace_block)
] @fold

; ====================
; Comments (multi-line foldable)
; ====================

(comment) @fold
