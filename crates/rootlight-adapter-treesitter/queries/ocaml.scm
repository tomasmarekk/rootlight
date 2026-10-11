; Implementation captures describe written declarations and applications only.
; The helper fences extension payloads and retains unsupported type source.
(compilation_unit) @root @module @scope
(compilation_unit) @expression
[(let_binding) (module_binding) (module_type_definition) (type_binding)
 (value_specification) (parameter)] @declaration @definition @signature
[(let_binding) (module_binding) (module_type_definition) (type_binding)
 (fun_expression) (let_expression) (match_case)] @scope
(let_binding) @expression
(type_binding) @expression
[(record_declaration) (variant_declaration)] @expression
(application_expression) @call
(value_path) @reference
[(extension) (item_extension) (floating_attribute) (attribute) (item_attribute)
 (class_binding) (class_type_binding) (exception_definition) (external)
 (open_module) (include_module) (include_module_type)] @expression
(comment) @comment @documentation
[(string) (quoted_string) (character)] @string
