; Ruby declarations and source occurrences from the audited native grammar.
; require, include and DSL macros remain calls, not evaluated dependencies.

(program) @root @module @scope
(body_statement) @scope
(block) @scope
(do_block) @scope
(singleton_class) @scope
(class name: (_) @definition) @declaration @scope @signature
(module name: (_) @definition) @declaration @scope
(method name: (_) @definition) @declaration @scope @signature
(singleton_method name: (_) @definition) @declaration @scope @signature
(method_parameters (identifier) @declaration @definition)
(block_parameters (identifier) @declaration @definition)
[(optional_parameter name: (identifier) @declaration @definition)
 (keyword_parameter name: (identifier) @declaration @definition)
 (splat_parameter name: (identifier) @declaration @definition)
 (hash_splat_parameter name: (identifier) @declaration @definition)
 (block_parameter name: (identifier) @declaration @definition)]
(assignment left: [(identifier) (constant) (scope_resolution) (instance_variable) (class_variable) (global_variable)] @definition) @declaration
(call) @call
(call method: (_) @call_name)
[(identifier) (constant) (scope_resolution) (instance_variable) (class_variable) (global_variable)] @reference
(comment) @comment
((comment) @documentation (#match? @documentation "^#"))
[(string) (heredoc_body) (simple_symbol) (delimited_symbol)] @string
