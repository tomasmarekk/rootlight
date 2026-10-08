; F# signature captures preserve authored contracts without inferring implementations.
; Separate packs keep implementation-only node kinds out of the signature factory.
(file) @root @module @scope
(file) @expression
(type_definition) @expression
[(namespace) (named_module) (module_defn)] @declaration @definition @signature
[(namespace) (named_module) (module_defn)] @scope
; Conditional scopes preserve authored contracts without selecting a compilation arm.
[(preproc_if) (preproc_else)] @scope
(import_decl) @import
(value_definition) @declaration @definition @signature
[(record_type_defn) (union_type_defn) (type_abbrev_defn) (enum_type_defn) (anon_type_defn) (interface_type_defn)] @declaration @definition @signature
[(record_type_defn) (union_type_defn) (enum_type_defn) (anon_type_defn) (interface_type_defn)] @scope
[(record_field) (union_type_case) (union_type_field) (enum_type_case)] @declaration @definition @signature
[(member_signature) (constructor_signature)] @declaration @definition @signature
(member_defn) @declaration @definition @signature
; Unsupported declaration shapes retain exact spans for bounded entity coverage.
[(module_abbrev) (exception_definition) (type_extension) (delegate_type_defn) (type_declaration) (argument_name_spec)] @declaration
