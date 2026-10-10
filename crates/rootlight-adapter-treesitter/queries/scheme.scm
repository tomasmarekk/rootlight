; The default reader has lists and symbols, not semantic declaration nodes.
; The private helper proves direct owners and executable positions before admission.
(program) @root @module @scope
(list) @declaration @definition @signature
(list) @scope
(list) @expression
(symbol) @declaration @definition
; Identity-only extraction discards reads, so references need an optional pattern.
(symbol) @reference
(symbol) @expression
[(directive) (ERROR)] @expression
[(comment) (block_comment) (sexp_comment)] @comment
(string) @string
