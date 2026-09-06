; Structural Lua candidates for the audited native grammar.
; Calls to require remain calls: Lua has no import statement, and the binding
; can be shadowed. Module dependency resolution needs separate semantic evidence.

(chunk) @root @module @scope
(block) @scope
(for_statement) @scope
(parameters name: (identifier) @declaration @definition)
(for_numeric_clause name: (identifier) @declaration @definition)
(for_generic_clause (variable_list name: (identifier) @declaration @definition))

(function_declaration
  name: (identifier) @definition
  parameters: (parameters) @signature) @declaration @scope
(function_declaration
  name: (dot_index_expression) @definition
  parameters: (parameters) @signature) @declaration @scope
(function_declaration
  name: (method_index_expression) @definition
  parameters: (parameters) @signature) @declaration @scope

(function_definition parameters: (parameters) @signature) @scope
; Each uninitialized binding owns one declaration, including name lists.
(variable_declaration (variable_list name: (identifier) @declaration @definition))
(variable_declaration
  (assignment_statement
    (variable_list . name: (identifier) @definition .))) @declaration
(variable_declaration
  (assignment_statement
    (variable_list . name: (identifier) @definition . (attribute) .))) @declaration
(variable_declaration
  (assignment_statement
    (variable_list . (attribute) . name: (identifier) @definition .))) @declaration
(variable_declaration
  (assignment_statement
    (variable_list . (attribute) . name: (identifier) @definition . (attribute) .))) @declaration
; Adjacent pairs cover every multi-binding name with linear match growth.
(variable_declaration
  (assignment_statement
    (variable_list name: (identifier) @declaration @definition . name: (identifier) @declaration @definition)))
(variable_declaration
  (assignment_statement
    (variable_list name: (identifier) @declaration @definition . (attribute) . name: (identifier) @declaration @definition)))

(assignment_statement
  (variable_list . name: (identifier) @definition .)
  (expression_list . value: (function_definition parameters: (parameters) @signature) .)) @declaration
(assignment_statement
  (variable_list . name: (dot_index_expression) @definition .)
  (expression_list . value: (function_definition parameters: (parameters) @signature) .)) @declaration
(field name: (identifier) @definition
  value: (function_definition parameters: (parameters) @signature)) @declaration

(function_call) @call
(function_call name: (identifier) @call_name)
(function_call name: (dot_index_expression field: (identifier) @call_name))
(function_call name: (method_index_expression method: (identifier) @call_name))
(identifier) @reference
(comment) @comment
((comment) @documentation (#match? @documentation "^---"))
(string) @string
