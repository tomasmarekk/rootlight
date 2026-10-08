; F# implementation captures retain written structural declarations and open evidence.
; Native selectors pair recursive heads with their own direct body field.
(file) @root @module @scope
; A complete optional context closes qualified lookup when any direct owner is lost.
(file) @expression
; Reviewed type wrappers conserve outer names, including ordinary DU constructors.
(type_definition) @expression
[(namespace) (named_module) (module_defn)] @declaration @definition @signature
[(namespace) (named_module) (module_defn)] @scope
; Conditional scopes preserve branch ownership without selecting a compilation arm.
[(preproc_if) (preproc_else)] @scope
(import_decl) @import
; The runtime permits at most three captures on one native query step.
[(function_declaration_left) (value_declaration_left)] @declaration @definition @signature
[(function_declaration_left) (value_declaration_left)] @scope
; Optional native witnesses preserve module binding eligibility without changing
; head identities. Recursive connectors partition only a complete native group.
[(function_declaration_left) (value_declaration_left)] @expression
(function_or_value_defn) @expression
(function_or_value_defn "rec" @expression)
(function_or_value_defn "and" @expression)
; A reviewed local wrapper owns its continuation without extending the authored
; head or its own RHS scope. Optional markers never create an entity.
(declaration_expression) @scope @expression
(declaration_expression in: (_) @expression)
; Anonymous lexical scopes retain authored formal identities without inventing
; a callable entity. Optional native witnesses own only the reviewed body.
(fun_expression) @scope
(fun_expression) @expression
(fun_expression "->" @expression)
(fun_expression . "fun" (argument_patterns) "->" (_) @expression .)
; Every arm remains a lexical barrier, including unreviewed patterns and guards.
; Optional native witnesses own only a completely reviewed pattern and body.
(rule) @scope
(rule) @expression
(rule "->" @expression)
(rule pattern: (_) @expression)
(rule block: (_) @expression)
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
; Formal identities are independent of optional RHS reads. The native selector
; requires a complete parameter-list proof for formal resolution.
(argument_patterns) @signature
; Complete unit-slot lists and exact unit tokens are optional callable witnesses,
; never parameter declarations or callable header identities.
(argument_patterns) @expression
(argument_patterns (const (unit) @expression))
; Compound lists distinguish written leaf identities from argument-slot arity.
(argument_patterns (repeat_pattern) @expression)
(identifier) @declaration @definition
; Optional authored binder headers must not enter the identity capture pattern.
(identifier) @expression
; Unsupported semantic bodies are optional, separate from binder identities.
(function_or_value_defn
  body: (_) @expression)
; Written attribute text does not prove attribute type or constructor relations.
(attribute) @expression
(identifier) @reference
(long_identifier) @reference
; Dispatch candidates preserve the complete outer application spine; argument
; boundaries do not claim coverage or evaluation of their contained syntax.
(application_expression) @call
(application_expression . (_) (_) @expression .)
