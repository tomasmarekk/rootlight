; Scala source declarations use native fields, not editor highlighting heuristics.
; Anonymous contexts retain lexical ownership without claiming implicit or JVM resolution.
(compilation_unit) @root @module
(package_clause name: (_) @definition) @declaration @scope
(class_definition name: (_) @definition) @declaration @scope @signature
(object_definition name: (_) @definition) @declaration @scope @signature
(trait_definition name: (_) @definition) @declaration @scope @signature
(enum_definition name: (_) @definition) @declaration @scope @signature
(full_enum_case name: (_) @definition) @declaration @scope @signature
(simple_enum_case name: (_) @definition) @declaration
(function_definition name: (_) @definition) @declaration @scope @signature
(function_declaration name: (_) @definition) @declaration @scope @signature
(given_definition name: (_) @definition) @declaration @scope @signature
(given_definition !name) @scope @signature
(type_definition name: (_) @definition) @declaration @scope
(extension_definition) @scope @signature
(lambda_expression) @scope
(block) @scope
(indented_block) @scope
(case_clause) @scope
(for_expression) @scope
; The checked binding classifier retains only declaration positions from these leaves.
[(identifier) (operator_identifier)] @declaration @definition
[(identifier) (operator_identifier) (type_identifier) (stable_type_identifier)] @reference
(import_declaration) @import
(export_declaration) @import
(call_expression) @call
(call_expression function: [(identifier) (operator_identifier) (field_expression)] @call_name)
(comment) @comment
((comment) @documentation (#match? @documentation "^/\\*\\*"))
(string) @string
