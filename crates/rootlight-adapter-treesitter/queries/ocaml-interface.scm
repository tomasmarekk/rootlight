; Interface declarations retain their written signatures, not implementations.
; Abstract types and unsupported signature constructs remain scoped source gaps.
(compilation_unit) @root @module @scope
(compilation_unit) @expression
[(value_specification) (module_binding) (type_binding)
 (module_type_definition)] @declaration @definition @signature
[(module_binding) (module_type_definition) (type_binding) (functor_type)] @scope
(type_binding) @expression
[(record_declaration) (variant_declaration)] @expression
[(extension) (item_extension) (floating_attribute) (attribute) (item_attribute)
 (class_binding) (class_type_binding) (exception_definition) (external)
 (open_module) (include_module_type)] @expression
(comment) @comment @documentation
[(string) (quoted_string) (character)] @string
