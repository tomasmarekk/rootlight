; Written loop binders and native identifier value uses are admitted atomically.
; Unsupported runtime constructs remain exact source-bound signature gaps.
(program) @root @module
(for_loop_statement item: (identifier) @declaration @definition)
(identifier) @reference
[(assignment_statement) (capture_statement) (tablerow_statement)
 (render_statement) (include_statement) (increment_statement)
 (decrement_statement) (custom_unpaired_statement) (liquid_tag)] @signature
(string) @string
(comment) @comment
