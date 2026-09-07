; Bash syntax evidence without executing commands or expanding shell words.
; source, eval and external command lookup are not statically resolved imports.

(program) @root @module @scope
[(compound_statement) (subshell) (do_group)] @scope
(function_definition name: (word) @definition) @declaration @scope @signature
(variable_assignment name: (variable_name) @definition) @declaration
(variable_assignment name: (subscript name: (variable_name) @definition)) @declaration
(for_statement variable: (variable_name) @declaration @definition)
(declaration_command (variable_name) @declaration @definition)
(command) @call
(command name: (command_name (word) @call_name))
[(variable_name) (special_variable_name)] @reference
(comment) @comment
((comment) @documentation (#match? @documentation "^#"))
[(string) (raw_string) (heredoc_body)] @string
