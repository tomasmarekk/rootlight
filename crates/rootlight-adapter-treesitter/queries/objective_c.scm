; Objective-C written declarations and source-backed selector components.
; Implementations and categories are lexical scopes, not invented class types.
; The pinned runtime retains at most three captures per query step. Split headers
; into their own patterns so a fourth capture cannot silently lose identity evidence.

(translation_unit) @root @module @scope
(class_interface) @declaration @definition @scope
(class_interface) @signature @scope_type
(class_interface (identifier) @scope_trait)
(class_implementation) @scope @scope_type
(class_implementation (identifier) @scope_trait)
(protocol_declaration) @declaration @definition @scope
(protocol_declaration) @signature
[(method_declaration) (method_definition)] @declaration @definition @scope
[(method_declaration) (method_definition)] @signature
[(method_declaration (identifier) @definition_part)
 (method_definition (identifier) @definition_part)
 (method_parameter ":" @definition_part)]
(property_declaration (struct_declaration (struct_declarator) @definition)) @declaration
(function_definition declarator: (function_declarator declarator: (identifier) @definition)) @declaration @scope @signature
[(struct_specifier name: (type_identifier) @definition)
 (union_specifier name: (type_identifier) @definition)
 (enum_specifier name: (type_identifier) @definition)
 (type_definition declarator: (type_identifier) @definition)] @declaration
[(preproc_include) (module_import)] @import
(compound_statement) @scope
[(call_expression) (message_expression)] @call
[(identifier) (field_identifier) (type_identifier)] @reference
(comment) @comment
((comment) @documentation (#match? @documentation "^/\\*\\*"))
[(string_literal) (concatenated_string)] @string
