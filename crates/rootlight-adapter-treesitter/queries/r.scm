; R source candidates use AST ownership, not runtime evaluation or package loading.
; Native selectors separate simple assignment names from replacement expressions.
(program) @root @module
(function_definition) @scope @declaration @signature
(binary_operator operator: ["<-" "=" "->" "<<-" "->>"]) @declaration @definition @signature
(parameter) @declaration @definition
(for_statement variable: (_) @declaration @definition)
(call) @call
(call function: [(identifier) (string) (namespace_operator) (extract_operator)] @call_name)
[(identifier) (dots) (dot_dot_i)] @reference
(namespace_operator) @reference
(extract_operator) @reference
(comment) @comment
((comment) @documentation (#match? @documentation "^#'"))
(string) @string
