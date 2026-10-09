; GLSL captures prove written owners without borrowing C dialect identities.
; Qualifiers, preprocessor selection and shader bindings remain source context.

(translation_unit) @root @module @scope

[(function_definition)
 (struct_specifier)
 (parameter_declaration)
 (declaration)] @declaration @definition @signature

(declaration declarator: (_) @declaration @definition @signature)
(field_declaration declarator: (_) @declaration @definition @signature)
(declaration
  (field_declaration_list) .
  [(identifier) (array_declarator)] @declaration @definition @signature)

[(compound_statement) (field_declaration_list)] @scope

[(function_definition)
 (struct_specifier)
 (declaration)
 (field_declaration)
 (parameter_declaration)
 (compound_statement)
 (field_declaration_list)
 (layout_specification)
 (qualifier)
 (type_qualifier)
 (extension_storage_class)
 (preproc_if)
 (preproc_ifdef)
 (preproc_elif)
 (preproc_elifdef)
 (preproc_else)
 (preproc_def)
 (preproc_function_def)
 (preproc_call)
 (preproc_include)
 (preproc_extension)
 (call_expression)
 (field_expression)
 (expression_statement)
 (union_specifier)
 (enum_specifier)
 (type_definition)
 (pointer_declarator)
 (variadic_parameter)
 (attribute_specifier)
 (attribute_declaration)
 (ERROR)] @expression

[(identifier) (field_identifier)] @reference
(comment) @comment
(string_literal) @string
