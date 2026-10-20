; Function headers require native option-field and execution-context proof.
; Command names are deliberately not guessed as resolved calls.
(program) @root @module @scope
(function_definition) @declaration @definition @signature
(function_definition) @scope
(function_definition) @expression
(function_definition option: (_) @declaration @definition)
(comment) @comment
[(single_quote_string) (double_quote_string)] @string
