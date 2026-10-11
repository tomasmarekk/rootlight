; HLSL captures retain written owners, not inherited C++ binding semantics.
; Buffer labels and shader annotations remain physical source context only.

(translation_unit) @root @module @scope

[(function_definition)
 (struct_specifier)
 (parameter_declaration)
 (optional_parameter_declaration)
 (declaration)] @declaration @definition @signature

(declaration declarator: (_) @declaration @definition @signature)
(field_declaration declarator: (_) @declaration @definition @signature)
[(compound_statement) (field_declaration_list) (declaration_list)] @scope

[(function_definition)
 (struct_specifier)
 (parameter_declaration)
 (optional_parameter_declaration)
 (declaration)
 (field_declaration)
 (compound_statement)
 (field_declaration_list)
 (declaration_list)
 (cbuffer_specifier)
 (resource_binding)
 (semantics)
 (bitfield_clause)
 (hlsl_attribute)
 (qualifiers)
 (type_qualifier)
 (storage_class_specifier)
 (discard_statement)
 (preproc_if)
 (preproc_ifdef)
 (preproc_elif)
 (preproc_elifdef)
 (preproc_else)
 (preproc_def)
 (preproc_function_def)
 (preproc_call)
 (preproc_include)
 (call_expression)
 (field_expression)
 (class_specifier)
 (union_specifier)
 (enum_specifier)
 (type_definition)
 (pointer_declarator)
 (reference_declarator)
 (namespace_definition)
 (namespace_alias_definition)
 (template_declaration)
 (alias_declaration)
 (using_declaration)
 (friend_declaration)
 (concept_definition)
 (variadic_parameter_declaration)
 (attribute_specifier)
 (attribute_declaration)
 (ERROR)] @expression

[(identifier) (field_identifier)] @reference
(comment) @comment
[(string_literal) (raw_string_literal) (char_literal)] @string
