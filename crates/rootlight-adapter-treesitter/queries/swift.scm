; Swift syntax evidence from the pinned native grammar, without type evaluation.
; Extensions retain a stable source-backed scope, not an invented class definition.

(source_file) @root @module @scope
(class_declaration declaration_kind: ["class" "struct" "actor" "enum"] name: (type_identifier) @definition) @declaration @scope @signature
(class_declaration declaration_kind: "extension" name: (_) @scope_trait) @scope @scope_type
(protocol_declaration name: (type_identifier) @definition) @declaration @scope @signature
[(function_declaration name: (simple_identifier) @definition)
 (protocol_function_declaration name: (simple_identifier) @definition)] @declaration @scope @signature
(init_declaration name: "init" @definition) @declaration @scope @signature
(deinit_declaration "deinit" @definition) @declaration @scope @signature
[(property_declaration name: (pattern bound_identifier: (simple_identifier) @definition) @declaration)
 (protocol_property_declaration name: (pattern bound_identifier: (simple_identifier) @definition) @declaration)]
(parameter name: (simple_identifier) @definition) @declaration
(typealias_declaration name: (type_identifier) @definition) @declaration
(enum_entry name: (simple_identifier) @definition @declaration)
(import_declaration) @import
[(class_body) (enum_class_body) (protocol_body) (function_body) (statements) (lambda_literal)] @scope
(call_expression) @call
[(simple_identifier) (type_identifier)] @reference
[(comment) (multiline_comment)] @comment
((comment) @documentation (#match? @documentation "^///"))
[(line_string_literal) (multi_line_string_literal) (raw_string_literal)] @string
