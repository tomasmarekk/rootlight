; MATLAB written definitions and uses retain native source positions.
; Parenthesized applications are not classified as calls without binding evidence.
(source_file) @root @module @scope
(function_definition name: (_) @definition) @declaration @scope
(function_definition) @signature
(class_definition name: (_) @definition) @declaration @scope
(class_definition) @signature
(function_signature name: (_) @definition) @declaration @scope
(function_signature) @signature
(properties (property name: (_) @definition) @declaration)
(identifier) @declaration @definition
(lambda) @scope
(identifier) @reference
(function_call) @reference
(command) @reference
(comment) @comment
(string) @string
