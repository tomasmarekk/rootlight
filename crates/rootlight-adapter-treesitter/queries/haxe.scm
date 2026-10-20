; Haxe facts require native owners and a bounded local Read certificate.
; Quoted macro syntax and compiler conditions never become ordinary host facts.
(module) @root @module @scope
(ERROR) @root @module @scope
[(class_declaration) (function_declaration)] @scope @declaration @definition
[(class_declaration) (function_declaration)] @signature
(variable_declaration) @declaration @definition @signature
(identifier) @reference @signature
; Uncertified Reads remain optional; the identity scan keeps only certified pairs.
(identifier) @reference
[(function_expression) (preprocessor_statement) (macro_expression)
 (reification_expression) (for_statement) (comprehension_for) (catch_clause)
 (case_statement) (ERROR)] @expression
(comment) @comment
(string) @string
