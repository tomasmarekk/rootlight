; Native written declarations are independent of unrelated recovered siblings.
; Signature carriers are separate from ordinary optional references.
(build_definition) @root
(assignment_statement) @declaration
(assignment_statement . (expression (identifier) @definition))
(iteration_statement (identifier_list (identifier) @declaration))
(iteration_statement (identifier_list (identifier) @definition))
(iteration_statement (identifier) @signature)
(iteration_statement) @signature
(ERROR) @signature
(identifier) @reference
(comment) @comment
[(string_literal) (fstring_literal)] @string
