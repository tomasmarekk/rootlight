; F# implementation captures retain written structural declarations and open evidence.
; Native selectors pair recursive heads with their own direct body field.
(file) @root @module @scope
[(namespace) (named_module) (module_defn)] @declaration @definition @signature
[(namespace) (named_module) (module_defn)] @scope
(import_decl) @import
; The runtime permits at most three captures on one native query step.
[(function_declaration_left) (value_declaration_left)] @declaration @definition @signature
[(function_declaration_left) (value_declaration_left)] @scope
[(record_type_defn) (union_type_defn) (type_abbrev_defn) (enum_type_defn) (anon_type_defn) (interface_type_defn)] @declaration @definition @signature
[(record_type_defn) (union_type_defn) (enum_type_defn) (anon_type_defn) (interface_type_defn)] @scope
[(record_field) (union_type_case) (union_type_field) (enum_type_case)] @declaration @definition @signature
[(method_or_prop_defn) (additional_constr_defn)] @declaration @definition @signature
[(method_or_prop_defn) (additional_constr_defn)] @scope
; Body-free members retain their written contract identity, not a proved implementation.
(member_signature) @declaration @definition @signature
(member_defn) @declaration @definition @signature
; Unsupported declaration shapes retain exact spans for bounded entity coverage.
[(module_abbrev) (exception_definition) (type_extension) (delegate_type_defn) (type_declaration) (argument_patterns) (argument_name_spec)] @declaration
; Anonymous object ownership must not project members onto an enclosing binding.
(object_expression) @declaration
