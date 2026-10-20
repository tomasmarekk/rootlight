; Native label context proves static fields without evaluating CUE expressions.
; Alias, pattern, computed and comprehension fields retain explicit proof gaps.
(source_file) @root @module @scope
(label) @declaration @definition
[(field) (comprehension) (function) (ERROR)] @expression
(comment) @comment
(string) @string
