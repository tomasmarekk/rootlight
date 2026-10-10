; WGSL captures retain complete written owners and unresolved source uses.
; Hidden comment tokens and absent strings cannot supply occurrence captures.

(translation_unit) @root @module @scope
; A separate step preserves source availability within the three-capture limit.
(translation_unit) @expression

[(function_decl)
 (struct_decl)
 (struct_member)
 (param)
 (type_alias_decl)
 (global_variable_decl)
 (global_value_decl)
 (variable_or_value_statement)] @declaration @definition @signature

[(compound_statement)
 (struct_body_decl)
 (loop_statement)
 (for_statement)
 (continuing_compound_statement)] @scope

[(function_decl)
 (struct_decl)
 (struct_member)
 (param)
 (type_alias_decl)
 (global_variable_decl)
 (global_value_decl)
 (variable_or_value_statement)
 (variable_decl)
 (compound_statement)
 (struct_body_decl)
 (loop_statement)
 (for_statement)
 (continuing_compound_statement)
 (attribute)
 (global_directive)
 (type_specifier)
 (call_phrase)
 (component_or_swizzle_specifier)
 (ERROR)] @expression

[(ident) (member_ident) (swizzle_name)] @reference
