; Direct native owners are proved by the CMake admission helper.
(source_file) @root @module
[(function_def) (macro_def) (normal_command)] @declaration @definition @signature
(function_command (argument_list (argument) @declaration @definition @signature))
(macro_command (argument_list (argument) @declaration @definition @signature))

; Calls retain written command sites without selecting evaluated targets.
(normal_command (identifier) @call)
(normal_command) @import
(variable_ref) @reference
[(if_condition) (foreach_loop) (while_loop) (block_def)] @signature
(configure_placeholder) @signature
[(line_comment) (bracket_comment)] @comment
[(quoted_argument) (bracket_argument)] @string
