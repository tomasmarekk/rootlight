; The default reader has lists and symbols, not semantic declaration nodes.
; The private helper proves direct owners and executable positions before admission.
(program) @root @module @scope
(list) @declaration @definition @signature
(list) @scope
(list) @expression
(symbol) @declaration @definition
; Certified direct calls join identity closure; ordinary reads need optional capture.
(symbol) @reference
(symbol) @expression
[(directive) (ERROR)] @expression
[(comment) (block_comment) (sexp_comment)] @comment
(string) @string
