; Capture complete DDL owners; SQL's native field selector chooses authored names.
; Catalog state, migration order and dialect-dependent resolution are not inferred.
(program) @root @module
[(create_table) (create_view) (create_materialized_view) (create_function)
 (create_index) (create_schema) (create_database) (create_role) (create_sequence)
 (create_extension) (create_trigger) (create_type) (column_definition)
 (function_argument)] @declaration @definition
(create_function (function_arguments) @signature)
(function_body) @signature
(object_reference) @reference
(literal) @string
[(comment) (marginalia)] @comment @documentation
