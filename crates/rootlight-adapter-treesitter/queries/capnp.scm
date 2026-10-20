; Physical schema owners and written names do not imply wire/compiler evaluation.
[(message) (ERROR)] @root @module
[(struct) (enum) (interface) (field) (union_field) (group) (union)
 (method) (parameter) (return_identifier) (enum_field) (const) (annotation)] @declaration @definition @signature
(union) @scope
[(custom_type) (type_identifier) (extend_type) (const_identifier)
 (annotation_identifier) (property) (type_definition) (param_identifier)] @reference
(comment) @comment
[(string) (import_path)] @string

; Mandatory source gaps survive optional-expression/query truncation.
[(using_directive) (import) (generics) (implicit_generics) (custom_type) (ERROR)] @signature
