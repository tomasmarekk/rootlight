; Identity captures and mandatory negative witnesses use independent patterns.
(document) @root @module @scope
[(typedef_definition) (enum_definition) (struct_definition) (union_definition)
 (exception_definition) (service_definition) (function_definition)
 (field) (parameter) (const_definition) (namespace_declaration)] @declaration @definition
(identifier) @declaration @definition
[(typedef_definition) (enum_definition) (struct_definition) (union_definition)
 (exception_definition) (service_definition) (function_definition)
 (field) (parameter) (const_definition) (namespace_declaration) (identifier)
 (ERROR) (senum_definition) (interaction_definition) (package_declaration)
 (annotation_definition) (fb_annotation_definition) (xsd_attrs)
 (include_statement) (literal) (number) (type) (definition_type) (container_type) (string)] @signature
[(type) (definition_type) (service_definition)] @reference
(include_statement) @import
(comment) @comment
(string) @string
