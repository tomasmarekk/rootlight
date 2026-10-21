; Structural roles stay within the reviewed three-capture active-step bound.
; References are separate optional patterns and never acquire targets by spelling.
(source_file) @root @module @scope
(class_definition) @scope @declaration @definition
(class_definition) @signature
(method_definition) @scope @declaration @definition
(method_definition) @signature
(assignment_statement) @declaration @definition
(identifier) @reference
[(function_call) (module_exports_statement) (import_statement) (embedded_js)
 (embedded_html) (class_property_block) (function_definition) (ERROR)] @expression
(comment) @comment
(string) @string
