; Generic reader lists require source-shape and reader-context admission.
(program) @root @module @scope
(program) @expression
(list) @declaration @definition @signature
(list) @scope
(symbol) @declaration @definition
(symbol) @reference
[(list) (extension) (symbol) (ERROR)] @expression
[(comment) (block_comment) (sexp_comment)] @comment
[(string) (byte_string) (here_string) (regex)] @string
