; D native header ownership is retained without evaluating templates or imports.
(source_file) @root @module @scope
(module_def) @declaration @definition @scope
(function_declaration) @declaration @definition @signature
(function_declaration) @scope
(parameter) @declaration @definition
[(module_def) (function_declaration) (parameter) (class_declaration) (struct_declaration) (template_declaration) (mixin_declaration) (conditional_declaration) (ERROR)] @expression
(comment) @comment
(string_literal) @string
